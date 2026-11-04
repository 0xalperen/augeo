//! Process hardening that must happen before anything else.

use windows::Win32::System::LibraryLoader::{
    LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
};

/// Makes every later DLL load search System32 only.
///
/// augeo may run elevated from a Downloads folder, where anything could sit beside the exe. The linker
/// flag `/DEPENDENTLOADFLAG:0x800` protects the imports loaded before `main`, and this protects the
/// ones loaded after. Call this first.
pub fn restrict_dll_search() -> Result<(), String> {
    // SAFETY: this takes a plain flag and affects only how this process searches for DLLs.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map_err(|e| format!("restricting DLL search: {e}"))
}
