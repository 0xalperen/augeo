use augeo_command::{Purpose, parse, resolve};

use super::*;
use crate::lint::{is_denied_registry, lint};
use crate::lock::{
    LockEntry, check_append_only, check_lock_matches_catalog, entries_from_catalog, parse_lock,
    render_lock,
};

#[test]
fn embedded_catalog_passes_every_lint_rule() {
    let catalog = Catalog::embedded().unwrap();
    assert!(!catalog.tweaks.is_empty());
    assert_eq!(lint(&catalog.tweaks), Vec::<String>::new());
}

#[test]
fn every_myth_says_why_and_any_source_is_https() {
    let catalog = Catalog::embedded().unwrap();
    assert!(catalog.myths.len() >= 20);
    for myth in &catalog.myths {
        assert!(!myth.name.trim().is_empty());
        assert!(myth.reason.trim().len() > 20, "{}", myth.name);
        if let Some(source) = &myth.source {
            assert!(source.starts_with("https://"), "{source}");
        }
    }
}

#[test]
fn the_catalog_keeps_its_promises() {
    let catalog = Catalog::embedded().unwrap();
    // The one security-lowering tweak is at the top level and in no preset.
    let lowering: Vec<_> = catalog
        .tweaks
        .iter()
        .filter(|t| t.security_lowering)
        .collect();
    assert_eq!(lowering.len(), 1);
    assert_eq!(lowering[0].danger, 4);
    assert!(!lowering[0].preset);
    // v0.1.0 disables no Windows service, and touches no boot setting except debug overrides.
    for tweak in &catalog.tweaks {
        for rev in &tweak.rev {
            for resource in &rev.resource {
                if let schema::Resource::RegValue { path, .. } = resource {
                    assert!(!path.to_lowercase().contains(r"\services\"), "{path}");
                }
            }
        }
    }
    // Every component's file loaded.
    for component in augeo_command::Component::ALL {
        assert!(
            catalog.tweaks.iter().any(|t| t.component == component),
            "{component} has no tweaks"
        );
    }
}

#[test]
fn embedded_lock_matches_the_catalog() {
    let catalog = Catalog::embedded().unwrap();
    let lock = parse_lock(EMBEDDED_LOCK).unwrap();
    let entries = entries_from_catalog(&catalog.tweaks).unwrap();
    assert_eq!(
        check_lock_matches_catalog(&lock, &entries),
        Vec::<String>::new()
    );
}

#[test]
fn commands_resolve_against_the_embedded_catalog() {
    let catalog = Catalog::embedded().unwrap();
    let view = catalog.view(None);
    let augeo_command::Command::Apply(target) = parse("auge!system#1").unwrap() else {
        panic!("expected apply");
    };
    assert_eq!(
        resolve(&target, &view, Purpose::Apply).unwrap().numbers,
        vec![1]
    );
}

/// A valid tweak in TOML. Tests change one thing about it and expect one rule to fire.
const GOOD: &str = r#"
[[tweak]]
id = "system-sample"
component = "system"
number = 1
title = "Sample"
summary = "Sets a value."
danger = 0
evidence = "Documented"
effect = "now"
risks = ["None known."]
undo_note = "Restores the old value."
[[tweak.sources]]
url = "https://learn.microsoft.com/x"
retrieved = "2026-11-04"
claim = "It exists."
[[tweak.rev]]
rev = 1
[[tweak.rev.resource]]
kind = "reg_value"
scope = "user"
path = 'Software\Sample'
name = "Enabled"
type = "dword"
desired = 1
"#;

fn lint_variant(text: &str) -> Vec<String> {
    lint(&Catalog::parse(&[("system", text)]).unwrap().tweaks)
}

fn assert_rule(from: &str, to: &str, expected: &str) {
    assert!(GOOD.contains(from), "fixture lacks {from}");
    let problems = lint_variant(&GOOD.replacen(from, to, 1));
    assert!(
        problems.iter().any(|p| p.contains(expected)),
        "expected {expected:?} in {problems:?}"
    );
}

#[test]
fn the_good_fixture_is_clean() {
    assert_eq!(lint_variant(GOOD), Vec::<String>::new());
}

#[test]
fn required_fields_are_enforced() {
    assert_rule(
        r#"risks = ["None known."]"#,
        "risks = []",
        "must list its risks",
    );
    assert_rule(r#"title = "Sample""#, r#"title = " ""#, "title is required");
    assert_rule(
        r#"undo_note = "Restores the old value.""#,
        r#"undo_note = """#,
        "undo_note is required",
    );
    assert_rule(r#"danger = 0"#, "danger = 5", "danger must be 0 to 4");
    assert_rule("number = 1", "number = 0", "numbers start at 1");
    assert_rule(
        r#"id = "system-sample""#,
        r#"id = "Bad Id""#,
        "lowercase slug",
    );
}

#[test]
fn missing_safety_fields_fail_to_parse() {
    for field in [
        r#"risks = ["None known."]"#,
        r#"undo_note = "Restores the old value.""#,
        r#"evidence = "Documented""#,
        "danger = 0",
    ] {
        let text = GOOD.replacen(field, "", 1);
        assert!(Catalog::parse(&[("system", &text)]).is_err(), "{field}");
    }
}

#[test]
fn unknown_fields_and_script_kinds_fail_to_parse() {
    let typo = GOOD.replacen("danger = 0", "danger = 0\nrevert_script = 'x'", 1);
    assert!(Catalog::parse(&[("system", &typo)]).is_err());
    let script = GOOD.replacen(r#"kind = "reg_value""#, r#"kind = "powershell""#, 1);
    assert!(Catalog::parse(&[("system", &script)]).is_err());
}

#[test]
fn a_tweak_must_be_in_its_components_file() {
    assert!(Catalog::parse(&[("cpu", GOOD)]).is_err());
}

#[test]
fn sources_are_enforced() {
    assert_rule(
        "https://learn.microsoft.com/x",
        "http://example.com",
        "must use https",
    );
    assert_rule("2026-11-04", "yesterday", "YYYY-MM-DD");
    assert_rule(r#"claim = "It exists.""#, r#"claim = """#, "needs a claim");
    assert_rule(
        r#"evidence = "Documented""#,
        r#"evidence = "Measured""#,
        "requires a source with measured results",
    );
}

#[test]
fn security_lowering_rules() {
    assert_rule(
        "danger = 0",
        "danger = 3\nsecurity_lowering = true",
        "must be danger 4",
    );
    let problems = lint_variant(&GOOD.replacen(
        "danger = 0",
        "danger = 4\nsecurity_lowering = true\npreset = true",
        1,
    ));
    assert!(
        problems
            .iter()
            .any(|p| p.contains("can never be in a preset"))
    );
}

#[test]
fn preset_rules() {
    assert_rule(
        "danger = 0",
        "danger = 2\npreset = true",
        "must be danger 0 or 1",
    );
    let text = GOOD
        .replacen("danger = 0", "danger = 0\npreset = true", 1)
        .replacen(r#"evidence = "Documented""#, r#"evidence = "Community""#, 1);
    assert!(
        lint_variant(&text)
            .iter()
            .any(|p| p.contains("needs Measured or Documented"))
    );
}

#[test]
fn denied_registry_locations_are_rejected() {
    for (path, name) in [
        (
            r"SOFTWARE\Policies\Microsoft\Windows Defender",
            "DisableAntiSpyware",
        ),
        (
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\a.exe",
            "Debugger",
        ),
        (r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run", "x"),
        (r"SYSTEM\CurrentControlSet\Services\WinDefend", "Start"),
        (r"SYSTEM\CurrentControlSet\Services\wuauserv", "Start"),
        (
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System",
            "EnableLUA",
        ),
        (
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Windows",
            "AppInit_DLLs",
        ),
        (r"SOFTWARE\Policies\Microsoft\WindowsFirewall", "x"),
        (r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate", "x"),
        (r"SYSTEM\CurrentControlSet\Control\SecureBoot\State", "x"),
        (
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management",
            "FeatureSettingsOverride",
        ),
    ] {
        assert!(is_denied_registry(path, name), "{path}");
    }
    assert!(!is_denied_registry(
        r"Software\Microsoft\GameBar",
        "AutoGameModeEnabled"
    ));

    assert_rule(
        r"path = 'Software\Sample'",
        r"path = 'SOFTWARE\Policies\Microsoft\Windows Defender'",
        "denied registry location",
    );
    assert_rule(
        r"path = 'Software\Sample'",
        r"path = 'HKLM\Software\Sample'",
        "must not include a hive",
    );
}

fn with_resource(resource: &str) -> String {
    let start = GOOD.find("[[tweak.rev.resource]]").unwrap();
    format!("{}{resource}", &GOOD[..start])
}

#[test]
fn only_listed_boot_settings_are_allowed() {
    for bad in [
        "testsigning",
        "nointegritychecks",
        "nx",
        "hypervisorlaunchtype",
        "debug",
    ] {
        let text = with_resource(&format!(
            "[[tweak.rev.resource]]\nkind = \"bcd_element\"\nelement = \"{bad}\"\n"
        ));
        assert!(
            lint_variant(&text)
                .iter()
                .any(|p| p.contains("is not allowed")),
            "{bad}"
        );
    }
    let text = with_resource(
        "[[tweak.rev.resource]]\nkind = \"bcd_element\"\nelement = \"useplatformclock\"\n",
    );
    assert_eq!(lint_variant(&text), Vec::<String>::new());
}

#[test]
fn drs_rules() {
    let resource = |profile: &str, source: &str| {
        format!(
            "[[tweak.rev.resource]]\nkind = \"drs_setting\"\nprofile = \"{profile}\"\n\
             setting = 1\nid_source = \"{source}\"\ndesired = 1\n"
        )
    };
    assert_eq!(
        lint_variant(&with_resource(&resource("base", "public"))),
        Vec::<String>::new()
    );
    assert!(
        lint_variant(&with_resource(&resource("base", "community")))
            .iter()
            .any(|p| p.contains("requires Experimental"))
    );
    assert!(
        lint_variant(&with_resource(&resource("{exe}", "public")))
            .iter()
            .any(|p| p.contains("game component only"))
    );
    assert!(
        lint_variant(&with_resource(&resource("Some Game", "public")))
            .iter()
            .any(|p| p.contains("must be base or"))
    );
}

#[test]
fn options_must_match_and_recommend_one() {
    let resource = "[[tweak.rev.resource]]\nkind = \"nic_property\"\nkeyword = \"*X\"\n\
                    desired = { by_option = { on = 1, off = 0 } }\n";
    let options = |on: bool, off: bool| {
        format!(
            "[[tweak.option]]\nkey = \"on\"\nname = \"On\"\nrecommended = {on}\n\
             [[tweak.option]]\nkey = \"off\"\nname = \"Off\"\nrecommended = {off}\n"
        )
    };
    let build = |opts: String| {
        with_resource(resource).replacen(
            "[[tweak.sources]]",
            &format!("{opts}[[tweak.sources]]"),
            1,
        )
    };
    assert_eq!(
        lint_variant(&build(options(true, false))),
        Vec::<String>::new()
    );
    assert!(
        lint_variant(&build(options(true, true)))
            .iter()
            .any(|p| p.contains("exactly one"))
    );
    assert!(
        lint_variant(&with_resource(resource))
            .iter()
            .any(|p| p.contains("don't match"))
    );
}

#[test]
fn revisions_must_count_up_and_change_something() {
    assert_rule("rev = 1", "rev = 2", "must count up from 1");
    assert!(
        lint_variant(&format!("{}[[tweak.rev]]\nrev = 2\n", GOOD))
            .iter()
            .any(|p| p.contains("changes nothing"))
    );
    let doubled = GOOD.to_string()
        + "[[tweak.rev.resource]]\nkind = \"reg_value\"\nscope = \"user\"\n\
           path = 'SOFTWARE\\sample'\nname = \"ENABLED\"\ntype = \"dword\"\ndesired = 2\n";
    assert!(
        lint_variant(&doubled)
            .iter()
            .any(|p| p.contains("repeats resource"))
    );
}

#[test]
fn retired_tweaks_need_a_reason_and_keep_working() {
    assert_rule(
        "danger = 0",
        "danger = 0\nstatus = \"retired\"",
        "needs retired_reason",
    );
    let text = GOOD.replacen(
        "danger = 0",
        "danger = 0\nstatus = \"retired\"\nretired_reason = \"Stopped working.\"",
        1,
    );
    assert_eq!(lint_variant(&text), Vec::<String>::new());
    let catalog = Catalog::parse(&[("system", &text)]).unwrap();
    let view = catalog.view(None);
    let augeo_command::Command::Apply(target) = parse("auge!system#1").unwrap() else {
        panic!("expected apply");
    };
    assert!(
        resolve(&target, &view, Purpose::Apply)
            .unwrap_err()
            .message
            .contains("Stopped working.")
    );
    assert!(resolve(&target, &view, Purpose::UndoOrInspect).is_ok());
}

fn second_tweak(extra: &str) -> String {
    GOOD.replacen("system-sample", "system-other", 1)
        .replacen("number = 1", "number = 2", 1)
        .replacen("danger = 0", &format!("danger = 0\n{extra}"), 1)
}

#[test]
fn two_tweaks_cant_share_a_resource_unless_both_declare_it() {
    let shared = format!("{GOOD}{}", second_tweak(""));
    assert!(lint_variant(&shared).iter().any(|p| p.contains("both own")));

    let one_way = format!("{GOOD}{}", second_tweak(r#"conflicts = ["system-sample"]"#));
    assert!(
        lint_variant(&one_way)
            .iter()
            .any(|p| p.contains("both own"))
    );

    let first = GOOD.replacen(
        "danger = 0",
        "danger = 0\nconflicts = [\"system-other\"]",
        1,
    );
    let both = format!(
        "{first}{}",
        second_tweak(r#"conflicts = ["system-sample"]"#)
    );
    assert_eq!(lint_variant(&both), Vec::<String>::new());
}

#[test]
fn duplicates_and_dangling_references_are_rejected() {
    let same_number = format!(
        "{GOOD}{}",
        GOOD.replacen("system-sample", "system-other", 1)
    );
    assert!(
        lint_variant(&same_number)
            .iter()
            .any(|p| p.contains("duplicate number"))
    );
    assert!(
        lint_variant(&format!("{GOOD}{GOOD}"))
            .iter()
            .any(|p| p.contains("duplicate id"))
    );
    assert_rule(
        "danger = 0",
        "danger = 0\nrequires = [\"nope\"]",
        "unknown tweak nope",
    );
}

fn entry(component: &str, number: u32, id: &str, rev: u32, hash: &str) -> LockEntry {
    LockEntry {
        component: component.into(),
        number,
        id: id.into(),
        rev,
        hash: hash.into(),
    }
}

#[test]
fn lock_round_trips() {
    let entries = vec![entry("cpu", 1, "a", 1, "aa"), entry("cpu", 2, "b", 1, "bb")];
    assert_eq!(parse_lock(&render_lock(&entries)).unwrap(), entries);
    assert!(parse_lock("cpu 1 a").is_err());
    assert!(parse_lock("cpu x a 1 aa").is_err());
    assert_eq!(parse_lock("# c\r\n\r\ncpu 1 a 1 aa\r\n").unwrap().len(), 1);
}

#[test]
fn hash_ignores_formatting_but_sees_changes() {
    let hash = |text: &str| {
        let c = Catalog::parse(&[("system", text)]).unwrap();
        lock::revision_hash(&c.tweaks[0].rev[0]).unwrap()
    };
    let base = hash(GOOD);
    assert_eq!(
        base,
        hash(&GOOD.replacen("title = \"Sample\"", "title = \"Renamed\"", 1))
    );
    assert_eq!(
        base,
        hash(&GOOD.replacen("desired = 1", "desired   =   1 # note", 1))
    );
    assert_ne!(base, hash(&GOOD.replacen("desired = 1", "desired = 2", 1)));
    assert_ne!(base, hash(&GOOD.replacen("Enabled", "Other", 1)));
}

#[test]
fn append_only_accepts_valid_growth() {
    let old = vec![entry("cpu", 1, "a", 1, "aa")];
    for new in [
        old.clone(),
        vec![entry("cpu", 1, "a", 1, "aa"), entry("cpu", 2, "b", 1, "bb")],
        vec![entry("cpu", 1, "a", 1, "aa"), entry("cpu", 1, "a", 2, "a2")],
        vec![entry("cpu", 1, "a", 1, "aa"), entry("gpu", 1, "g", 1, "gg")],
        vec![
            entry("cpu", 1, "a", 1, "aa"),
            entry("cpu", 3, "c", 1, "cc"),
            entry("cpu", 2, "b", 1, "bb"),
        ],
    ] {
        assert_eq!(
            check_append_only(&old, &new),
            Vec::<String>::new(),
            "{new:?}"
        );
    }
    assert_eq!(check_append_only(&[], &old), Vec::<String>::new());
}

#[test]
fn append_only_rejects_broken_promises() {
    let old = vec![entry("cpu", 1, "a", 1, "aa"), entry("cpu", 2, "b", 1, "bb")];
    for (new, expected) in [
        (vec![entry("cpu", 1, "a", 1, "aa")], "removed or changed"),
        (
            vec![entry("cpu", 1, "a", 1, "XX"), entry("cpu", 2, "b", 1, "bb")],
            "removed or changed",
        ),
        (
            vec![
                entry("cpu", 1, "a", 1, "aa"),
                entry("cpu", 2, "b", 1, "bb"),
                entry("cpu", 2, "c", 1, "cc"),
            ],
            "can't reuse it",
        ),
        (
            vec![
                entry("cpu", 1, "a", 1, "aa"),
                entry("cpu", 2, "b", 1, "bb"),
                entry("cpu", 5, "c", 1, "cc"),
            ],
            "next free number is 3",
        ),
        (
            vec![
                entry("cpu", 1, "a", 1, "aa"),
                entry("cpu", 2, "b", 1, "bb"),
                entry("gpu", 1, "a", 2, "a2"),
            ],
            "moved from",
        ),
    ] {
        let problems = check_append_only(&old, &new);
        assert!(
            problems.iter().any(|p| p.contains(expected)),
            "{expected}: {problems:?}"
        );
    }
}

#[test]
fn lock_and_catalog_must_agree() {
    let lock = vec![entry("cpu", 1, "a", 1, "aa")];
    let changed = vec![entry("cpu", 1, "a", 1, "XX")];
    let problems = check_lock_matches_catalog(&lock, &changed);
    assert!(problems.iter().any(|p| p.contains("new revision")));
    assert!(problems.iter().any(|p| p.contains("never deleted")));
}
