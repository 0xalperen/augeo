//! Link settings that protect the shipped binary, and its icon and file details.

use std::{env, fs, path::Path};

/// The icon and the details Windows shows in the file's properties.
fn embed_resources(crate_dir: &Path) {
    let icon = crate_dir.join("augeo.ico");
    println!("cargo:rerun-if-changed=augeo.ico");
    let number = |name: &str| env::var(name).unwrap_or_else(|_| "0".to_string());
    let (major, minor, patch) = (
        number("CARGO_PKG_VERSION_MAJOR"),
        number("CARGO_PKG_VERSION_MINOR"),
        number("CARGO_PKG_VERSION_PATCH"),
    );
    let version = format!("{major}.{minor}.{patch}");
    let script = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "ProductName", "augeo"
      VALUE "FileDescription", "augeo"
      VALUE "InternalName", "augeo"
      VALUE "OriginalFilename", "augeo.exe"
      VALUE "ProductVersion", "{version}"
      VALUE "FileVersion", "{version}"
      VALUE "LegalCopyright", "GPL-3.0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
    );
    let out_dir = env::var("OUT_DIR").unwrap_or_else(|_| panic!("cargo always sets OUT_DIR"));
    let script_path = Path::new(&out_dir).join("augeo.rc");
    fs::write(&script_path, script).unwrap_or_else(|e| panic!("writing the resource script: {e}"));
    embed_resource::compile(&script_path, embed_resource::NONE)
        .manifest_required()
        .unwrap_or_else(|e| panic!("compiling the icon and file details: {e}"));
}

fn main() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = crate_dir.join("augeo.manifest");
    println!("cargo:rerun-if-changed=augeo.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    // Imports resolve from System32 only, so a DLL planted beside the exe can't load before main.
    // Tested in docs/decisions/0001: without this flag, a planted powrprof.dll or dxgi.dll runs.
    println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
    embed_resources(crate_dir);
}
