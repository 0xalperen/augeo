//! Who augeo is running as, and asking Windows to elevate.

use std::os::windows::ffi::OsStrExt;

use augeo_ports::PortError;
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_ELEVATION,
            TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
        },
        System::Threading::{
            GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcess, OpenProcessToken,
            PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
        },
        UI::{
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId, SW_SHOWNORMAL},
        },
    },
    core::{PCWSTR, PWSTR},
};

use crate::util::wide;

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open and is closed only here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn process_token(process: HANDLE) -> Result<OwnedHandle, PortError> {
    let mut token = HANDLE::default();
    // SAFETY: the process handle is valid, and `token` is a valid place for the result.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }
        .map_err(|e| PortError::Failed(format!("opening a process token: {e}")))?;
    Ok(OwnedHandle(token))
}

/// Whether this process has an elevated token.
pub fn is_elevated() -> Result<bool, PortError> {
    // SAFETY: GetCurrentProcess returns a pseudo handle that is always valid.
    let token = process_token(unsafe { GetCurrentProcess() })?;
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    // SAFETY: the buffer is exactly the size passed, and all pointers are valid for the call.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some(std::ptr::addr_of_mut!(elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    }
    .map_err(|e| PortError::Failed(format!("reading elevation: {e}")))?;
    Ok(elevation.TokenIsElevated != 0)
}

fn token_sid(token: &OwnedHandle) -> Result<String, PortError> {
    let failed = |e: windows::core::Error| PortError::Failed(format!("reading a user SID: {e}"));
    let mut needed = 0u32;
    // SAFETY: a null buffer with size 0 only asks for the size, so this "fails" by design.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut needed) };
    // u64 gives the buffer enough alignment for TOKEN_USER.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: the buffer has at least `needed` bytes.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
    }
    .map_err(failed)?;
    // SAFETY: the API filled the buffer with a TOKEN_USER, and it is suitably aligned.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = PWSTR::null();
    // SAFETY: the SID lives in `buffer`, which outlives this call.
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }.map_err(failed)?;
    // SAFETY: the API succeeded, so this is a terminated string.
    let sid = unsafe { text.to_string() }.map_err(|e| PortError::Failed(e.to_string()));
    // SAFETY: this memory came from ConvertSidToStringSidW, which says to use LocalFree.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    }
    sid
}

/// The SID this process runs as.
pub fn current_user_sid() -> Result<String, PortError> {
    // SAFETY: GetCurrentProcess returns a pseudo handle that is always valid.
    token_sid(&process_token(unsafe { GetCurrentProcess() })?)
}

/// The SID of the person at the desktop, found from who owns the shell window.
///
/// Elevating with another account's password makes HKCU that administrator's hive. Per-user tweaks
/// and disclaimer acceptance belong to the person at the desktop, so this is what they use.
pub fn interactive_user_sid() -> Result<String, PortError> {
    // SAFETY: this takes no arguments and returns a window handle or null.
    let shell = unsafe { GetShellWindow() };
    if shell.0.is_null() {
        return current_user_sid();
    }
    let mut pid = 0u32;
    // SAFETY: the window handle was just returned, and `pid` is a valid place for the result.
    unsafe { GetWindowThreadProcessId(shell, Some(&mut pid)) };
    if pid == 0 {
        return current_user_sid();
    }
    // SAFETY: the arguments are plain values. A failure is handled below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return current_user_sid();
    };
    let process = OwnedHandle(process);
    match process_token(process.0) {
        Ok(token) => token_sid(&token),
        Err(_) => current_user_sid(),
    }
}

/// How asking for elevation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    /// The elevated copy ran and exited with this code.
    Finished(u32),
    /// The user said no at the UAC prompt.
    Declined,
}

/// Quotes one argument the way the Windows command line parser expects.
fn quote_argument(argument: &str) -> String {
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in argument.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            other => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(other);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Relaunches this program elevated, in a new window, and waits for it to exit.
///
/// The only input passed along is one command string. The elevated copy parses it again, plans again
/// and asks for confirmation again, and there is deliberately no way to skip that. So a program that
/// starts augeo with crafted arguments can't get a change past the confirmation screen.
pub fn relaunch_elevated(command: &str) -> Result<Elevation, PortError> {
    let exe = std::env::current_exe()
        .map_err(|e| PortError::Failed(format!("finding this program: {e}")))?;
    let exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let parameters = wide(&quote_argument(command));
    let verb = wide("runas");

    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(exe_w.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: every pointer in `info` refers to a local that outlives the call.
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        if e.code() == ERROR_CANCELLED.to_hresult() {
            return Ok(Elevation::Declined);
        }
        return Err(PortError::Failed(format!("asking for elevation: {e}")));
    }
    let process = OwnedHandle(info.hProcess);
    // SAFETY: the handle is valid because SEE_MASK_NOCLOSEPROCESS was requested.
    let waited = unsafe { WaitForSingleObject(process.0, INFINITE) };
    if waited != WAIT_OBJECT_0 {
        return Err(PortError::Failed(
            "waiting for the elevated copy failed".into(),
        ));
    }
    let mut code = 0u32;
    // SAFETY: the handle is valid, and `code` is a valid place for the result.
    unsafe { GetExitCodeProcess(process.0, &mut code) }
        .map_err(|e| PortError::Failed(format!("reading the exit code: {e}")))?;
    Ok(Elevation::Finished(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_can_be_read() {
        let _ = is_elevated().unwrap();
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    #[test]
    fn the_interactive_user_is_found() {
        let sid = interactive_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    #[test]
    fn arguments_are_quoted_for_the_windows_parser() {
        assert_eq!(quote_argument("auge!cpu#1"), r#""auge!cpu#1""#);
        assert_eq!(quote_argument("a b"), r#""a b""#);
        assert_eq!(quote_argument(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_argument(r"a\b"), r#""a\b""#);
        assert_eq!(quote_argument(r"a\"), r#""a\\""#);
        assert_eq!(quote_argument(r#"a\"b"#), r#""a\\\"b""#);
    }
}
