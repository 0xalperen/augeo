//! augeo composition root. Later milestones wire the TUI and engine here.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    // First, before anything can load a DLL.
    if let Err(e) = augeo_sys::hardening::restrict_dll_search() {
        eprintln!("augeo can't start safely: {e}");
        return ExitCode::FAILURE;
    }
    println!("augeo {}", env!("CARGO_PKG_VERSION"));
    ExitCode::SUCCESS
}
