//! Catalog rules. A test runs these over the embedded catalog, so a violation fails CI.

use std::collections::{BTreeMap, BTreeSet};

use augeo_command::Component;

use crate::schema::{Desired, Evidence, IdSource, Resource, Status, Tweak};

/// Registry locations no tweak may touch. These enforce the project's hard safety rules. The executor
/// checks them again at runtime through [`is_denied_registry`], so a bad entry is stopped twice.
const DENIED_PATH_PARTS: &[&str] = &[
    r"image file execution options",
    r"currentversion\run",
    r"\services\",
    r"windows defender",
    r"smartscreen",
    r"policies\system",
    r"windowsfirewall",
    r"windowsupdate",
    r"\wuauserv",
    r"\fve",
    r"bitlocker",
    r"securityhealth",
    r"secureboot",
    r"\tpm",
    r"\winlogon",
    r"\lsa",
];

const DENIED_VALUE_NAMES: &[&str] = &[
    "appinit_dlls",
    "enablelua",
    "consentpromptbehavioradmin",
    "disableantispyware",
    "featuresettingsoverride",
    "featuresettingsoverridemask",
    "imagepath",
];

/// The only boot settings a tweak may touch. All of them are debugging overrides that repairs remove.
/// `testsigning`, `nointegritychecks`, `nx`, `hypervisorlaunchtype` and friends can never appear.
pub const ALLOWED_BCD_ELEMENTS: &[&str] = &[
    "useplatformclock",
    "useplatformtick",
    "disabledynamictick",
    "tscsyncpolicy",
];

/// Whether a registry location is off limits for every tweak.
pub fn is_denied_registry(path: &str, name: &str) -> bool {
    let path = path.to_lowercase();
    let name = name.to_lowercase();
    DENIED_PATH_PARTS.iter().any(|part| path.contains(part))
        || DENIED_VALUE_NAMES.contains(&name.as_str())
}

fn is_slug(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parses a build requirement such as `>=19041`. Returns the minimum build.
pub fn parse_build_rule(rule: &str) -> Option<u32> {
    rule.strip_prefix(">=")?.trim().parse().ok()
}

fn is_date(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn is_guid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn desired_of(resource: &Resource) -> Option<&Desired> {
    match resource {
        Resource::RegValue { desired, .. }
        | Resource::RegKvString { desired, .. }
        | Resource::PowerIndexAc { desired, .. }
        | Resource::PowerIndexDc { desired, .. }
        | Resource::DrsSetting { desired, .. }
        | Resource::NicProperty { desired, .. } => Some(desired),
        _ => None,
    }
}

fn lint_resource(t: &Tweak, r: &Resource, push: &mut impl FnMut(String)) {
    match r {
        Resource::RegValue { path, name, .. } | Resource::RegKvString { path, name, .. } => {
            if is_denied_registry(path, name) {
                push(format!("touches denied registry location {path}\\{name}"));
            }
            if path.starts_with("HKEY") || path.starts_with("HK") {
                push(format!("registry path must not include a hive: {path}"));
            }
        }
        Resource::PowerIndexAc {
            subgroup, setting, ..
        }
        | Resource::PowerIndexDc {
            subgroup, setting, ..
        } => {
            if !is_guid(subgroup) || !is_guid(setting) {
                push(format!("bad power GUID: {subgroup} / {setting}"));
            }
        }
        Resource::PowerActiveScheme { scheme } => {
            if !is_guid(scheme) {
                push(format!("bad power scheme GUID: {scheme}"));
            }
        }
        Resource::BcdElement { element } => {
            if !ALLOWED_BCD_ELEMENTS.contains(&element.as_str()) {
                push(format!("boot setting '{element}' is not allowed"));
            }
        }
        Resource::DrsSetting {
            profile, id_source, ..
        } => {
            let per_game = profile == "{exe}";
            if profile != "base" && !per_game {
                push(format!(
                    "DRS profile must be base or {{exe}}, not {profile}"
                ));
            }
            if per_game != (t.component == Component::Game) {
                push("per-game DRS profiles belong to the game component only".to_string());
            }
            if *id_source == IdSource::Community && t.evidence != Evidence::Experimental {
                push("a community DRS ID requires Experimental evidence".to_string());
            }
        }
        Resource::NicProperty { keyword, .. } => {
            if keyword.is_empty() {
                push("NIC keyword is empty".to_string());
            }
        }
        Resource::DisplayMode { .. } => {}
    }

    if let Some(Desired::ByOption { by_option }) = desired_of(r) {
        let keys: BTreeSet<&str> = by_option.keys().map(String::as_str).collect();
        let options: BTreeSet<&str> = t.option.iter().map(|o| o.key.as_str()).collect();
        if keys != options {
            push(format!(
                "by_option keys {keys:?} don't match the tweak's options {options:?}"
            ));
        }
    }
    if let Some(Desired::Absent { absent: false }) = desired_of(r) {
        push("'absent = false' means nothing; give a value instead".to_string());
    }
}

fn lint_tweak(t: &Tweak, push: &mut impl FnMut(String)) {
    if !is_slug(&t.id) {
        push("id must be a lowercase slug".to_string());
    }
    if t.number == 0 {
        push("numbers start at 1".to_string());
    }
    for (field, value) in [
        ("title", &t.title),
        ("summary", &t.summary),
        ("undo_note", &t.undo_note),
    ] {
        if value.trim().is_empty() {
            push(format!("{field} is required"));
        }
    }
    if t.danger > 4 {
        push(format!("danger must be 0 to 4, not {}", t.danger));
    }
    if t.risks.is_empty() || t.risks.iter().any(|r| r.trim().is_empty()) {
        push("every tweak must list its risks".to_string());
    }
    if t.sources.is_empty() {
        push("every tweak must cite a source".to_string());
    }
    for s in &t.sources {
        if !s.url.starts_with("https://") {
            push(format!("source URL must use https: {}", s.url));
        }
        if !is_date(&s.retrieved) {
            push(format!(
                "source retrieved date must be YYYY-MM-DD: {}",
                s.retrieved
            ));
        }
        if s.claim.trim().is_empty() {
            push(format!("source needs a claim: {}", s.url));
        }
    }
    if t.evidence == Evidence::Measured
        && !t.sources.iter().any(|s| {
            s.measured
                .as_ref()
                .is_some_and(|m| !m.effect.trim().is_empty())
        })
    {
        push("Measured evidence requires a source with measured results".to_string());
    }
    if t.security_lowering && t.danger != 4 {
        push("a security-lowering tweak must be danger 4".to_string());
    }
    if t.preset {
        if t.danger > 1 {
            push("a preset tweak must be danger 0 or 1".to_string());
        }
        if !matches!(t.evidence, Evidence::Measured | Evidence::Documented) {
            push("a preset tweak needs Measured or Documented evidence".to_string());
        }
        if t.security_lowering {
            push("a security-lowering tweak can never be in a preset".to_string());
        }
    }
    if parse_build_rule(&t.applicability.build).is_none() {
        push(format!("bad build rule: {}", t.applicability.build));
    }
    if t.status == Status::Retired
        && t.retired_reason
            .as_deref()
            .is_none_or(|r| r.trim().is_empty())
    {
        push("a retired tweak needs retired_reason".to_string());
    }
    if !t.option.is_empty() && t.option.iter().filter(|o| o.recommended).count() != 1 {
        push("a tweak with options must recommend exactly one".to_string());
    }

    if t.rev.is_empty() {
        push("every tweak needs at least one revision".to_string());
    }
    let revs: Vec<u32> = t.rev.iter().map(|r| r.rev).collect();
    if !revs.iter().copied().eq(1..=revs.len() as u32) {
        push(format!("revisions must count up from 1, found {revs:?}"));
    }
    for rev in &t.rev {
        if rev.resource.is_empty() {
            push(format!("revision {} changes nothing", rev.rev));
        }
        let mut keys = BTreeSet::new();
        for r in &rev.resource {
            if !keys.insert(r.key()) {
                push(format!("revision {} repeats resource {}", rev.rev, r.key()));
            }
            lint_resource(t, r, push);
        }
    }
}

/// Runs every rule. An empty result means the catalog is valid.
pub fn lint(tweaks: &[Tweak]) -> Vec<String> {
    let mut problems = Vec::new();
    for t in tweaks {
        lint_tweak(t, &mut |p| problems.push(format!("{}: {p}", t.id)));
    }

    let mut ids = BTreeSet::new();
    let mut numbers = BTreeSet::new();
    for t in tweaks {
        if !ids.insert(t.id.as_str()) {
            problems.push(format!("{}: duplicate id", t.id));
        }
        if !numbers.insert((t.component, t.number)) {
            problems.push(format!(
                "{}: duplicate number {}#{}",
                t.id, t.component, t.number
            ));
        }
    }
    for t in tweaks {
        for other in t
            .conflicts
            .iter()
            .chain(&t.requires)
            .chain(&t.superseded_by)
        {
            if !ids.contains(other.as_str()) {
                problems.push(format!("{}: refers to unknown tweak {other}", t.id));
            }
        }
    }

    let mut owners: BTreeMap<String, Vec<&Tweak>> = BTreeMap::new();
    for t in tweaks.iter().filter(|t| t.status == Status::Active) {
        if let Some(rev) = t.current_rev() {
            for r in &rev.resource {
                owners.entry(r.key()).or_default().push(t);
            }
        }
    }
    for (key, sharing) in owners {
        for (i, a) in sharing.iter().enumerate() {
            for b in &sharing[i + 1..] {
                if !(a.conflicts.contains(&b.id) && b.conflicts.contains(&a.id)) {
                    problems.push(format!(
                        "{} and {} both own {key} without declaring a conflict in both tweaks",
                        a.id, b.id
                    ));
                }
            }
        }
    }
    problems
}
