//! Small helpers shared by the adapters.

use std::path::PathBuf;

use augeo_ports::PortError;
use windows::{
    Win32::{
        Foundation::{ERROR_ACCESS_DENIED, WIN32_ERROR},
        System::SystemInformation::GetSystemDirectoryW,
    },
    core::{GUID, HSTRING},
};

pub(crate) fn wide(text: &str) -> HSTRING {
    HSTRING::from(text)
}

pub(crate) fn win32_error(context: &str, code: WIN32_ERROR) -> PortError {
    let message = windows::core::Error::from(code.to_hresult()).message();
    let text = format!("{context}: {message} (error {})", code.0);
    if code == ERROR_ACCESS_DENIED {
        PortError::Denied(text)
    } else {
        PortError::Failed(text)
    }
}

/// Parses `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
pub(crate) fn parse_guid(text: &str) -> Result<GUID, PortError> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let value = (hex.len() == 32)
        .then(|| u128::from_str_radix(&hex, 16).ok())
        .flatten()
        .ok_or_else(|| PortError::Failed(format!("invalid GUID '{text}'")))?;
    Ok(GUID::from_u128(value))
}

pub(crate) fn guid_text(guid: &GUID) -> String {
    let d = guid.data4;
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1, guid.data2, guid.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

/// The real System32, never taken from an environment variable.
pub(crate) fn system_directory() -> Result<PathBuf, PortError> {
    let mut buffer = [0u16; 260];
    // SAFETY: the buffer is valid for its whole length, and the API writes at most that much.
    let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if len == 0 || len >= buffer.len() {
        return Err(PortError::Failed("couldn't find System32".into()));
    }
    Ok(PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_round_trip() {
        let text = "381b4222-f694-41f0-9685-ff5bb260df2e";
        assert_eq!(guid_text(&parse_guid(text).unwrap()), text);
        assert!(parse_guid("nonsense").is_err());
        assert!(parse_guid("381b4222-f694-41f0-9685-ff5bb260df2").is_err());
    }

    #[test]
    fn system32_is_found() {
        let dir = system_directory().unwrap();
        assert!(dir.join("kernel32.dll").exists(), "{dir:?}");
    }
}
