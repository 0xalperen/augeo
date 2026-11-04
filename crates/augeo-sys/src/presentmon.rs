//! Runs PresentMon. It never downloads anything, and only runs an exact, hash-checked build.
//!
//! An elevated program running a file from a user-writable folder would be a gift to malware, so the
//! file is copied into the locked-down state folder, checked there, and held open against writes
//! while it runs. PresentMon needs elevation to see other programs' frames.

use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::util::system_directory;

/// The pinned build. Updating it means checking the new columns against the parser's tests.
pub const FILE_NAME: &str = "PresentMon-2.6.0-x64.exe";
pub const SHA256: &str = "b2a706bc6ad475749e3b7e3409263aa1e6906d45bdcf993f6dbc0f660188f1af";
pub const DOWNLOAD_PAGE: &str = "https://github.com/GameTechDev/PresentMon/releases/tag/v2.6.0";

const FILE_SHARE_READ: u32 = 0x1;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How long past the planned end to wait, in case the game hasn't started presenting.
const GRACE: Duration = Duration::from_secs(30);

fn sha256_of(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn missing_message() -> String {
    format!(
        "{FILE_NAME} wasn't found. The benchmark needs it next to augeo.exe. It comes in the augeo \
         release zip, or from {DOWNLOAD_PAGE}. augeo never downloads anything itself."
    )
}

/// Finds a verified PresentMon in the state folder, copying it there from beside this exe if needed.
pub fn prepare(state_dir: &Path) -> Result<PathBuf, String> {
    let bin = state_dir.join("bin");
    let installed = bin.join(FILE_NAME);
    if installed.exists() && sha256_of(&installed)?.eq_ignore_ascii_case(SHA256) {
        return Ok(installed);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let source = exe.parent().map(|dir| dir.join(FILE_NAME));
    let source = source.filter(|p| p.exists()).ok_or_else(missing_message)?;
    let hash = sha256_of(&source)?;
    if !hash.eq_ignore_ascii_case(SHA256) {
        return Err(format!(
            "{} doesn't match the expected build, so it won't be run. Its SHA-256 is {hash}.",
            source.display()
        ));
    }
    fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    fs::copy(&source, &installed).map_err(|e| format!("copying PresentMon: {e}"))?;
    // Checked again after copying, because the source folder may be writable by anyone.
    if !sha256_of(&installed)?.eq_ignore_ascii_case(SHA256) {
        let _ = fs::remove_file(&installed);
        return Err(
            "PresentMon changed while it was being copied, so it won't be run.".to_string(),
        );
    }
    Ok(installed)
}

/// A capture in progress.
#[derive(Debug)]
pub struct Capture {
    child: Child,
    output: PathBuf,
    give_up_at: Instant,
    _hold: File,
}

/// Starts capturing a game's frames for `seconds`, writing a CSV to `output`.
pub fn start(tool: &Path, exe: &str, seconds: u32, output: &Path) -> Result<Capture, String> {
    // Held open without sharing write access, so the file can't change between checking and running.
    let hold = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(tool)
        .map_err(|e| format!("opening PresentMon: {e}"))?;
    let mut bytes = Vec::new();
    (&hold)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("reading PresentMon: {e}"))?;
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !hash.eq_ignore_ascii_case(SHA256) {
        return Err("PresentMon doesn't match the expected build, so it won't be run.".to_string());
    }
    let system32 = system_directory().map_err(|e| e.to_string())?;
    // A unique session name means only augeo's own trace is ever stopped.
    let session = format!("augeo_{}", std::process::id());
    let child = Command::new(tool)
        .args(["--process_name", exe])
        .arg("--output_file")
        .arg(output)
        .args(["--timed", &seconds.to_string()])
        .args(["--terminate_after_timed", "--no_console_stats"])
        .args(["--session_name", &session])
        .current_dir(system32)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("starting PresentMon: {e}"))?;
    Ok(Capture {
        child,
        output: output.to_path_buf(),
        give_up_at: Instant::now() + Duration::from_secs(u64::from(seconds)) + GRACE,
        _hold: hold,
    })
}

impl Capture {
    /// `None` while still capturing. Otherwise the CSV text, or why there isn't any.
    pub fn poll(&mut self, exe: &str) -> Option<Result<String, String>> {
        let exit = match self.child.try_wait() {
            Ok(status) => status,
            Err(e) => return Some(Err(format!("watching PresentMon: {e}"))),
        };
        match exit {
            Some(status) if status.success() => Some(
                fs::read_to_string(&self.output)
                    .map_err(|_| format!("No frames were captured. Was {exe} running?")),
            ),
            Some(status) => Some(Err(format!(
                "PresentMon stopped with {status}. It needs elevation to see other programs."
            ))),
            None if Instant::now() >= self.give_up_at => {
                self.stop();
                Some(Err(format!(
                    "No frames arrived in time. Was {exe} running?"
                )))
            }
            None => None,
        }
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("augeo-pm-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_wrong_file_is_never_accepted() {
        let dir = scratch("wrong");
        fs::create_dir_all(dir.join("bin")).unwrap();
        let fake = dir.join("bin").join(FILE_NAME);
        fs::write(&fake, b"not the real thing").unwrap();
        // The installed copy fails its check, and there's no good source, so this must refuse.
        let result = prepare(&dir);
        assert!(result.is_err(), "{result:?}");
        // Starting it directly refuses too, and nothing gets run.
        let started = start(&fake, "game.exe", 1, &dir.join("out.csv"));
        assert!(started.unwrap_err().contains("expected build"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_tool_explains_where_to_get_it_and_downloads_nothing() {
        let dir = scratch("missing");
        let message = prepare(&dir).unwrap_err();
        assert!(message.contains(DOWNLOAD_PAGE), "{message}");
        assert!(message.contains("never downloads"), "{message}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
