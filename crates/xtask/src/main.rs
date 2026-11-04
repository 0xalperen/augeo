//! Repository checks: `cargo xtask check-numbering` and `cargo xtask update-lock`.

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
/// broken promise. Using the full history means a force-push or a squashed batch can't hide a change,
/// as long as the checkout has full history (`fetch-depth: 0`).
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

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let result = match task.as_str() {
        "check-numbering" => check_numbering(),
        "update-lock" => update_lock(),
        _ => Err("usage: cargo xtask <check-numbering | update-lock>".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
