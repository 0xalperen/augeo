//! Link settings that protect the shipped binary.

fn main() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("augeo.manifest");
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
}
