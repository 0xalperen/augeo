//! The engine on the real registry and real journal files, with a real process being killed.
//!
//! Everything happens under a private key in the current user's hive and a scratch folder, so this
//! needs no elevation and touches nothing else.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use augeo_catalog::Catalog;
use augeo_engine::{
    Confirmation, Context, Disclaimer, Engine, TweakRequest, TweakResult, TweakState, UndoPolicy,
    applicability::{CpuVendor, Environment},
};
use augeo_journal::Stamp;
use augeo_ports::{Address, Clock, RegRoot, System, Value};
use augeo_sys::{FileStore, WindowsSystem, elevation::current_user_sid};

struct RealClock;

impl Clock for RealClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

type RealEngine = Engine<WindowsSystem, FileStore, RealClock>;

fn key_path(tag: &str) -> String {
    format!(r"Software\augeo-test\e2e-{tag}")
}

fn catalog(tag: &str) -> Catalog {
    let path = key_path(tag);
    let resources: String = ["One", "Two", "Three"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            format!(
                "[[tweak.rev.resource]]\nkind = \"reg_value\"\nscope = \"user\"\n\
                 path = '{path}'\nname = \"{name}\"\ntype = \"dword\"\ndesired = {}\n",
                i + 100
            )
        })
        .collect();
    let text = format!(
        r#"
[[tweak]]
id = "e2e"
component = "system"
number = 1
title = "End to end"
summary = "Test."
danger = 0
evidence = "Documented"
effect = "now"
risks = ["None."]
undo_note = "Restores it."
[[tweak.sources]]
url = "https://example.com"
retrieved = "2026-11-04"
claim = "Test."
[[tweak.rev]]
rev = 1
{resources}"#
    );
    Catalog::parse(&[("system", &text)]).unwrap()
}

fn context() -> Context {
    Context {
        env: Environment {
            build: 26100,
            nvidia: false,
            laptop: false,
            cpu_vendor: CpuVendor::Other,
            dual_ccd_x3d: false,
            vrr_active: false,
        },
        stamp: Stamp {
            boot_id: "test".into(),
            os_build: 26100,
            driver: None,
            app: "test".into(),
        },
        user_sid: current_user_sid().unwrap(),
        power_scheme: "381b4222-f694-41f0-9685-ff5bb260df2e".into(),
        gpu_scheduling: None,
        adapters: vec![],
        display_device: None,
        max_display_mode: None,
        frame_cap: None,
        disclaimer: Disclaimer {
            version: 1,
            text_sha256: "test".into(),
        },
    }
}

fn open(dir: &Path, tag: &str) -> RealEngine {
    Engine::open(
        WindowsSystem::new(),
        FileStore::new(dir),
        RealClock,
        catalog(tag),
        context(),
        true,
    )
    .unwrap()
}

fn address(tag: &str, name: &str) -> Address {
    Address::Reg {
        root: RegRoot::User {
            sid: current_user_sid().unwrap(),
        },
        path: key_path(tag),
        name: name.into(),
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("augeo-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Sets the starting values and removes them when the test ends.
struct Fixture {
    tag: String,
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let mut system = WindowsSystem::new();
        // One value exists, one has another type, and one doesn't exist.
        system
            .write(&address(tag, "One"), &Value::Dword(7))
            .unwrap();
        system
            .write(&address(tag, "Two"), &Value::Text("old text".into()))
            .unwrap();
        system
            .write(&address(tag, "Three"), &Value::Absent)
            .unwrap();
        Self {
            tag: tag.to_string(),
            dir: scratch(tag),
        }
    }

    fn assert_original(&self, label: &str) {
        let system = WindowsSystem::new();
        assert_eq!(
            system.read(&address(&self.tag, "One")).unwrap(),
            Value::Dword(7),
            "{label}"
        );
        assert_eq!(
            system.read(&address(&self.tag, "Two")).unwrap(),
            Value::Text("old text".into()),
            "{label}"
        );
        assert_eq!(
            system.read(&address(&self.tag, "Three")).unwrap(),
            Value::Absent,
            "{label}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let mut system = WindowsSystem::new();
        for name in ["One", "Two", "Three"] {
            let _ = system.write(&address(&self.tag, name), &Value::Absent);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn apply_and_undo_on_the_real_registry_survive_reopening() {
    let fixture = Fixture::new("basic");
    {
        let mut e = open(&fixture.dir, "basic");
        e.accept_disclaimer().unwrap();
        let plan = e.plan_apply(&[TweakRequest::new("e2e")]).unwrap();
        assert_eq!(plan.tweaks[0].steps[0].current, Value::Dword(7));
        assert_eq!(plan.tweaks[0].steps[2].current, Value::Absent);
        let report = e.execute(&plan, &Confirmation::default()).unwrap();
        assert_eq!(report.results[0].1, TweakResult::Done);
    }

    let system = WindowsSystem::new();
    assert_eq!(
        system.read(&address("basic", "One")).unwrap(),
        Value::Dword(100)
    );
    assert_eq!(
        system.read(&address("basic", "Three")).unwrap(),
        Value::Dword(102)
    );

    // A new process knows it was applied, and can undo it.
    let mut e = open(&fixture.dir, "basic");
    assert_eq!(e.status("e2e", None).unwrap().state, TweakState::Applied);
    let plan = e.plan_undo_all(UndoPolicy::default()).unwrap();
    e.execute(&plan, &Confirmation::default()).unwrap();
    fixture.assert_original("after undo");
}

#[test]
fn a_second_process_is_read_only_while_the_first_holds_the_lock() {
    let fixture = Fixture::new("lock");
    let first = open(&fixture.dir, "lock");
    assert!(first.is_writer());
    let second = open(&fixture.dir, "lock");
    assert!(!second.is_writer());
    assert!(second.open_report().locked_elsewhere);
}

const CHILD_DIR: &str = "AUGEO_E2E_CHILD_DIR";
const CHILD_TAG: &str = "AUGEO_E2E_CHILD_TAG";

/// Not a test of its own: when re-run as a child, this applies and undoes forever until it's killed.
#[test]
fn child_process_worker() {
    let (Some(dir), Some(tag)) = (std::env::var_os(CHILD_DIR), std::env::var(CHILD_TAG).ok())
    else {
        return;
    };
    let mut e = open(Path::new(&dir), &tag);
    e.accept_disclaimer().unwrap();
    loop {
        let plan = e.plan_apply(&[TweakRequest::new("e2e")]).unwrap();
        e.execute(&plan, &Confirmation::default()).unwrap();
        let plan = e.plan_undo_all(UndoPolicy::default()).unwrap();
        e.execute(&plan, &Confirmation::default()).unwrap();
    }
}

#[test]
fn killing_a_real_process_at_random_moments_never_loses_the_original_values() {
    let fixture = Fixture::new("kill");
    let exe = std::env::current_exe().unwrap();
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let start = Instant::now();
    let mut kills = 0;
    let mut interrupted = 0;

    while kills < 25 && start.elapsed() < Duration::from_secs(60) {
        let mut child = Command::new(&exe)
            .args(["child_process_worker", "--exact", "--nocapture"])
            .env(CHILD_DIR, &fixture.dir)
            .env(CHILD_TAG, "kill")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        // A simple generator: the point is spread across the operation, not quality.
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        std::thread::sleep(Duration::from_millis(u64::from(30 + seed % 120)));
        child.kill().unwrap();
        child.wait().unwrap();
        kills += 1;

        let mut e = open(&fixture.dir, "kill");
        assert!(
            e.is_writer(),
            "the kernel should have released the dead process's lock"
        );
        for item in e.in_doubt().unwrap() {
            interrupted += 1;
            e.resolve(item.txn, item.default_decision()).unwrap();
        }
        let plan = e.plan_undo_all(UndoPolicy::default()).unwrap();
        e.execute(&plan, &Confirmation::default()).unwrap();
        fixture.assert_original(&format!("after kill {kills}"));
    }
    assert!(kills >= 10, "only {kills} kills ran");
    println!("{kills} kills, {interrupted} caught mid-change");
}
