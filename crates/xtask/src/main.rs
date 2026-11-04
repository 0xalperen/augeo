//! Repository checks: `cargo xtask <check-numbering | update-lock | check-boundaries | check-binary>`.

#![forbid(unsafe_code)]

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use augeo_catalog::{
    Catalog,
    lock::{
        LockEntry, check_append_only, check_lock_matches_catalog, entries_from_catalog, parse_lock,
        render_lock,
    },
};

const LOCK_PATH: &str = "catalog/registry.lock";
/// The one crate allowed to use `unsafe` and the `windows` crate.
const SYS_CRATE: &str = "augeo-sys";
/// augeo makes no network calls, so its binary must not import any of these.
const NETWORK_DLLS: &[&str] = &[
    "ws2_32.dll",
    "wsock32.dll",
    "winhttp.dll",
    "wininet.dll",
    "urlmon.dll",
    "dnsapi.dll",
    "webio.dll",
    "bitsproxy.dll",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root())
        .output()
        .map_err(|e| format!("couldn't run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn catalog_entries() -> Result<Vec<LockEntry>, String> {
    let catalog = Catalog::embedded()?;
    entries_from_catalog(&catalog.tweaks).map_err(|e| e.to_string())
}

fn read_lock() -> Result<Vec<LockEntry>, String> {
    let text = std::fs::read_to_string(repo_root().join(LOCK_PATH)).map_err(|e| e.to_string())?;
    parse_lock(&text)
}

/// Adds entries for new tweaks and revisions. It never removes one, so the checks still catch a
/// deleted tweak, and a changed revision shows up as two conflicting entries.
fn update_lock() -> Result<(), String> {
    let mut entries = read_lock()?;
    let existing: BTreeSet<LockEntry> = entries.iter().cloned().collect();
    let mut added = 0;
    for entry in catalog_entries()? {
        if !existing.contains(&entry) {
            println!(
                "adding {} {} rev {}",
                entry.component, entry.number, entry.rev
            );
            entries.push(entry);
            added += 1;
        }
    }
    std::fs::write(repo_root().join(LOCK_PATH), render_lock(&entries))
        .map_err(|e| e.to_string())?;
    println!("{added} added");
    Ok(())
}

/// Walks every committed version of the lock, oldest first, then the working copy, and fails on any
/// broken promise. Using the full history means a squashed batch can't hide a change, as long as the
/// checkout has full history (`fetch-depth: 0`).
fn check_numbering() -> Result<(), String> {
    let mut problems = Vec::new();

    let shallow = git(&["rev-parse", "--is-shallow-repository"])?;
    if shallow.trim() == "true" {
        return Err("needs full history; use fetch-depth: 0".to_string());
    }

    let log = git(&["log", "--reverse", "--format=%H", "--", LOCK_PATH])?;
    let mut previous: Vec<LockEntry> = Vec::new();
    for commit in log.lines() {
        // The file may have been deleted in a commit. That is a problem in itself.
        let text = git(&["show", &format!("{commit}:{LOCK_PATH}")]).unwrap_or_default();
        let entries = parse_lock(&text).map_err(|e| format!("{commit}: {e}"))?;
        for p in check_append_only(&previous, &entries) {
            problems.push(format!("{commit}: {p}"));
        }
        previous = entries;
    }

    let current = read_lock()?;
    for p in check_append_only(&previous, &current) {
        problems.push(format!("working copy: {p}"));
    }
    for p in check_lock_matches_catalog(&current, &catalog_entries()?) {
        problems.push(format!("working copy: {p}"));
    }

    let tags = git(&["tag", "--list", "v*"])?;
    for tag in tags.lines() {
        if let Ok(text) = git(&["show", &format!("{tag}:{LOCK_PATH}")]) {
            let released = parse_lock(&text).map_err(|e| format!("{tag}: {e}"))?;
            for p in check_append_only(&released, &current) {
                problems.push(format!("since {tag}: {p}"));
            }
        }
    }

    if problems.is_empty() {
        println!("tweak numbering is stable ({} entries)", current.len());
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Confines `unsafe` and the `windows` crate to one crate, and fails if any other crate's root stops
/// forbidding `unsafe`.
fn check_boundaries() -> Result<(), String> {
    let mut problems = Vec::new();
    let crates = repo_root().join("crates");
    let entries = std::fs::read_dir(&crates).map_err(|e| e.to_string())?;
    let mut checked = 0;
    for entry in entries {
        let dir = entry.map_err(|e| e.to_string())?.path();
        let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if !dir.join("Cargo.toml").exists() || name == SYS_CRATE {
            continue;
        }
        checked += 1;
        let manifest =
            std::fs::read_to_string(dir.join("Cargo.toml")).map_err(|e| e.to_string())?;
        if manifest
            .lines()
            .any(|l| l.trim_start().starts_with("windows") || l.contains("dependencies.windows"))
        {
            problems.push(format!(
                "{name} depends on the windows crate; only {SYS_CRATE} may"
            ));
        }
        let roots: Vec<PathBuf> = ["src/lib.rs", "src/main.rs"]
            .iter()
            .map(|r| dir.join(r))
            .filter(|p| p.exists())
            .collect();
        if roots.is_empty() {
            problems.push(format!("{name} has no crate root"));
        }
        for root in roots {
            let text = std::fs::read_to_string(&root).map_err(|e| e.to_string())?;
            if !text.contains("#![forbid(unsafe_code)]") {
                problems.push(format!("{} must forbid unsafe code", root.display()));
            }
        }
    }
    if problems.is_empty() {
        println!("{checked} crates keep to the boundary");
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn dumpbin() -> Result<PathBuf, String> {
    let program_files = std::env::var("ProgramFiles(x86)")
        .map_err(|_| "ProgramFiles(x86) isn't set".to_string())?;
    let vswhere = Path::new(&program_files).join(r"Microsoft Visual Studio\Installer\vswhere.exe");
    let output = Command::new(vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .map_err(|e| format!("couldn't run vswhere: {e}"))?;
    let install = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let version_file =
        Path::new(&install).join(r"VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt");
    let version = std::fs::read_to_string(&version_file)
        .map_err(|e| format!("reading {}: {e}", version_file.display()))?;
    Ok(Path::new(&install)
        .join(r"VC\Tools\MSVC")
        .join(version.trim())
        .join(r"bin\HostX64\x64\dumpbin.exe"))
}

fn run_dumpbin(tool: &Path, flag: &str, binary: &str) -> Result<String, String> {
    let output = Command::new(tool)
        .args(["/nologo", flag, binary])
        .output()
        .map_err(|e| format!("couldn't run dumpbin: {e}"))?;
    if !output.status.success() {
        return Err(format!("dumpbin {flag} failed on {binary}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Checks the release binary itself: no network DLLs, and protection against planted DLLs.
fn check_binary(binary: Option<String>) -> Result<(), String> {
    let binary = binary.ok_or("usage: cargo xtask check-binary <path to augeo.exe>")?;
    let tool = dumpbin()?;
    let mut problems = Vec::new();

    let imports: BTreeSet<String> = run_dumpbin(&tool, "/imports", &binary)?
        .lines()
        .filter_map(|l| {
            let word = l.trim();
            let lower = word.to_ascii_lowercase();
            (lower.ends_with(".dll") && !word.contains(' ')).then_some(lower)
        })
        .collect();
    println!(
        "imports: {}",
        imports.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    if imports.is_empty() {
        problems
            .push("found no imports, so this check isn't reading the file properly".to_string());
    }
    for dll in NETWORK_DLLS {
        if imports.contains(*dll) {
            problems.push(format!("imports {dll}, but augeo makes no network calls"));
        }
    }

    let load_config = run_dumpbin(&tool, "/loadconfig", &binary)?;
    // A line reads like "    0800 Dependent Load Flag".
    let protected = load_config
        .lines()
        .filter(|l| l.contains("Dependent Load Flag"))
        .filter_map(|l| l.split_whitespace().next())
        .any(|flag| u32::from_str_radix(flag, 16) == Ok(0x800));
    if !protected {
        problems.push("the binary isn't linked with /DEPENDENTLOADFLAG:0x800".to_string());
    }

    if problems.is_empty() {
        println!("{binary} passes");
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// The layouts asserted in `augeo-sys/src/nvapi.rs`, as `(C expression, expected)`.
const NVAPI_LAYOUT: &[(&str, usize)] = &[
    ("sizeof(NVDRS_SETTING_V1)", 12320),
    ("offsetof(NVDRS_SETTING_V1, settingId)", 4100),
    ("offsetof(NVDRS_SETTING_V1, settingLocation)", 4108),
    ("offsetof(NVDRS_SETTING_V1, u32CurrentValue)", 8220),
    ("sizeof(NVDRS_PROFILE_V1)", 4116),
    ("offsetof(NVDRS_PROFILE_V1, numOfSettings)", 4112),
    ("sizeof(NVDRS_APPLICATION_V4)", 20492),
    ("offsetof(NVDRS_APPLICATION_V4, commandLine)", 16396),
    ("NVDRS_SETTING_VER1", 0x0001_3020),
    ("NVDRS_PROFILE_VER1", 0x0001_1014),
    ("NVDRS_APPLICATION_VER_V4", 0x0004_500C),
];

/// Compiles a small C program against NVIDIA's headers and compares what it reports with the layouts
/// augeo uses. CI has no GPU, so a wrong layout would otherwise only show up on a user's machine.
fn check_nvapi_layout(headers: Option<String>) -> Result<(), String> {
    let headers = headers.ok_or("usage: cargo xtask check-nvapi-layout <NVIDIA nvapi folder>")?;
    let tool = dumpbin()?;
    let install = tool
        .ancestors()
        .nth(8)
        .ok_or("no Visual Studio folder")?
        .to_path_buf();
    let vcvars = install.join(r"VC\Auxiliary\Build\vcvars64.bat");

    let work = std::env::temp_dir().join(format!("augeo-layout-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let mut source = String::from(
        "#include <stdio.h>\n#include <stddef.h>\n#include \"nvapi.h\"\nint main(void) {\n",
    );
    for (expression, _) in NVAPI_LAYOUT {
        source.push_str(&format!("printf(\"%zu\\n\", (size_t)({expression}));\n"));
    }
    source.push_str("return 0;\n}\n");
    std::fs::write(work.join("layout.c"), source).map_err(|e| e.to_string())?;

    let script = format!(
        "@call \"{}\" >nul\r\n@cl /nologo /I \"{headers}\" layout.c /Fe:layout.exe\r\n@if errorlevel 1 exit /b 1\r\n@.\\layout.exe\r\n",
        vcvars.display()
    );
    std::fs::write(work.join("run.cmd"), script).map_err(|e| e.to_string())?;
    let output = Command::new("cmd")
        .arg("/c")
        .arg(work.join("run.cmd"))
        .current_dir(&work)
        .output()
        .map_err(|e| format!("couldn't run the compiler: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&work);
    if !output.status.success() {
        return Err(format!(
            "compiling against nvapi.h failed:\n{text}\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let reported: Vec<usize> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
    if reported.len() != NVAPI_LAYOUT.len() {
        return Err(format!(
            "expected {} numbers, got:\n{text}",
            NVAPI_LAYOUT.len()
        ));
    }
    let mut problems = Vec::new();
    for ((expression, expected), actual) in NVAPI_LAYOUT.iter().zip(reported) {
        if *expected != actual {
            problems.push(format!(
                "{expression}: C says {actual}, augeo assumes {expected}"
            ));
        }
    }
    if problems.is_empty() {
        println!("{} NVAPI layout facts match", NVAPI_LAYOUT.len());
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let task = args.next().unwrap_or_default();
    let result = match task.as_str() {
        "check-numbering" => check_numbering(),
        "update-lock" => update_lock(),
        "check-boundaries" => check_boundaries(),
        "check-binary" => check_binary(args.next()),
        "check-nvapi-layout" => check_nvapi_layout(args.next()),
        _ => Err(
            "usage: cargo xtask <check-numbering | update-lock | check-boundaries | check-binary | check-nvapi-layout>"
                .to_string(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
