#![allow(dead_code, unreachable_pub)]

use augeo_catalog::Catalog;
use augeo_engine::{
    Confirmation, Context, Disclaimer, Engine, Plan, TYPED_EXPERT, TYPED_HIGH, TweakRequest,
    UndoPolicy,
    applicability::{CpuVendor, Environment},
};
use augeo_fakes::{FakeClock, FakeStore, FakeSystem, World};
use augeo_journal::{Decision, Stamp};
use augeo_ports::{Address, DrsProfile, RegRoot};

pub type TestEngine = Engine<FakeSystem, FakeStore, FakeClock>;

pub const SID: &str = "S-1-5-21-111-222-333-1001";
pub const BALANCED: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";
pub const HIGH_PERFORMANCE: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";
pub const PLAN_SUBGROUP: &str = "2a737441-1930-4402-8d77-b2bebba308a3";
pub const PLAN_SETTING: &str = "48e6b7a6-50f5-4782-a5d4-53bb8f07e226";

/// A per-plan setting's address on one power plan.
pub fn plan_setting(scheme: &str) -> Address {
    Address::PowerAc {
        scheme: scheme.into(),
        subgroup: PLAN_SUBGROUP.into(),
        setting: PLAN_SETTING.into(),
    }
}

/// A world on the Balanced plan, with the per-plan setting at 1 on both plans.
pub fn on_balanced() -> World {
    let world = seeded();
    world.set(
        &Address::PowerActiveScheme,
        augeo_ports::Value::Text(BALANCED.into()),
    );
    world.set(&plan_setting(BALANCED), augeo_ports::Value::Dword(1));
    world.set(
        &plan_setting(HIGH_PERFORMANCE),
        augeo_ports::Value::Dword(1),
    );
    world
}

const SOURCE: &str = r#"
[[tweak.sources]]
url = "https://example.com/doc"
retrieved = "2026-11-04"
claim = "Says so."
"#;

fn common(id: &str, component: &str, number: u32, danger: u8, extra: &str) -> String {
    format!(
        r#"
[[tweak]]
id = "{id}"
component = "{component}"
number = {number}
title = "Title of {id}"
summary = "Summary."
danger = {danger}
evidence = "Documented"
effect = "now"
risks = ["A risk."]
undo_note = "Restores it."
{extra}
{SOURCE}
[[tweak.rev]]
rev = 1
"#
    )
}

fn reg(scope: &str, name: &str, desired: &str) -> String {
    format!(
        "[[tweak.rev.resource]]\nkind = \"reg_value\"\nscope = \"{scope}\"\n\
         path = 'Software\\Test'\nname = \"{name}\"\ntype = \"dword\"\ndesired = {desired}\n"
    )
}

/// The catalog used by engine tests.
pub fn catalog() -> Catalog {
    let system = [
        common("single", "system", 1, 0, "") + &reg("user", "Single", "1"),
        common("multi", "system", 2, 1, "")
            + &reg("machine", "M1", "1")
            + &reg("machine", "M2", "2")
            + &reg("machine", "M3", "3"),
        common("medium", "system", 3, 2, "") + &reg("machine", "Medium", "1"),
        common("high", "system", 4, 3, "") + &reg("machine", "High", "1"),
        common("expert", "system", 5, 4, "security_lowering = true")
            + &reg("machine", "Expert", "0"),
        common("restart", "system", 6, 0, "").replace("effect = \"now\"", "effect = \"restart\"")
            + &reg("machine", "Restart", "1"),
        common("needs-single", "system", 7, 0, "requires = [\"single\"]")
            + &reg("machine", "Needs", "1"),
        common("preset-yes", "system", 8, 0, "preset = true") + &reg("machine", "PresetYes", "1"),
        common(
            "picky",
            "system",
            9,
            0,
            "[[tweak.option]]\nkey = \"low\"\nname = \"Low\"\nrecommended = true\n\
             [[tweak.option]]\nkey = \"high\"\nname = \"High\"\n",
        ) + &reg("machine", "Picky", "{ by_option = { low = 1, high = 2 } }"),
        common(
            "old-win",
            "system",
            10,
            0,
            "[tweak.applicability]\nbuild = \">=99999\"\n",
        ) + &reg("machine", "OldWin", "1"),
        common(
            "retired",
            "system",
            11,
            0,
            "status = \"retired\"\nretired_reason = \"Gone.\"",
        ) + &reg("machine", "Retired", "1"),
        common(
            "conflict-a",
            "system",
            12,
            0,
            "conflicts = [\"conflict-b\"]",
        ) + &reg("machine", "Shared", "1"),
        common(
            "conflict-b",
            "system",
            13,
            0,
            "conflicts = [\"conflict-a\"]",
        ) + &reg("machine", "Shared", "2"),
    ]
    .concat();

    let cpu = common("repair", "cpu", 1, 2, "kind = \"repair\"")
        + "[[tweak.rev.resource]]\nkind = \"bcd_element\"\nelement = \"useplatformclock\"\n\
           [[tweak.rev.resource]]\nkind = \"bcd_element\"\nelement = \"useplatformtick\"\n";

    let drs = |profile: &str, setting: u32, desired: u32| {
        format!(
            "[[tweak.rev.resource]]\nkind = \"drs_setting\"\nprofile = \"{profile}\"\n\
             setting = {setting}\nid_source = \"public\"\ndesired = {desired}\n"
        )
    };
    let gpu = common("drs-global", "gpu", 1, 1, "")
        + &drs("base", 100, 1)
        + &common(
            "scheduling",
            "gpu",
            2,
            1,
            "live_state = \"gpu_scheduling\"\n\
             [[tweak.option]]\nkey = \"on\"\nname = \"On\"\nrecommended = true\n\
             [[tweak.option]]\nkey = \"off\"\nname = \"Off\"\n",
        )
        + &reg("machine", "HwSch", "{ by_option = { on = 2, off = 1 } }");
    let game = common("drs-game", "game", 1, 1, "") + &drs("{exe}", 100, 1);

    let power = common("plan-switch", "power", 1, 1, "")
        + &format!(
            "[[tweak.rev.resource]]\nkind = \"power_active_scheme\"\nscheme = \"{HIGH_PERFORMANCE}\"\n"
        )
        + &common("plan-setting", "power", 2, 0, "")
        + &format!(
            "[[tweak.rev.resource]]\nkind = \"power_index_ac\"\nsubgroup = \"{PLAN_SUBGROUP}\"\n\
             setting = \"{PLAN_SETTING}\"\ndesired = 0\n"
        );

    Catalog::parse(&[
        ("system", &system),
        ("cpu", &cpu),
        ("gpu", &gpu),
        ("game", &game),
        ("power", &power),
    ])
    .expect("the test catalog parses")
}

pub fn context() -> Context {
    Context {
        env: Environment {
            build: 26100,
            nvidia: true,
            laptop: false,
            cpu_vendor: CpuVendor::Amd,
            dual_ccd_x3d: false,
            vrr_active: false,
        },
        stamp: Stamp {
            boot_id: "boot-1".into(),
            os_build: 26100,
            driver: Some("610.88".into()),
            app: "0.1.0".into(),
        },
        user_sid: SID.into(),
        power_scheme: "381b4222-f694-41f0-9685-ff5bb260df2e".into(),
        gpu_scheduling: None,
        adapters: vec![],
        display_device: None,
        max_display_mode: None,
        frame_cap: None,
        disclaimer: Disclaimer {
            version: 1,
            text_sha256: "disclaimer-hash".into(),
        },
    }
}

pub fn engine_with(world: &World, ctx: Context, writer: bool) -> TestEngine {
    Engine::open(
        world.system(),
        world.store(),
        world.clock(),
        catalog(),
        ctx,
        writer,
    )
    .expect("the engine opens")
}

pub fn engine(world: &World) -> TestEngine {
    engine_with(world, context(), true)
}

/// A world with a few pre-existing values, and the disclaimer accepted.
pub fn seeded() -> World {
    let world = World::new();
    world.set(&machine("M1"), augeo_ports::Value::Dword(10));
    world.set(&machine("M3"), augeo_ports::Value::Text("old".into()));
    world.set(
        &bcd("useplatformclock"),
        augeo_ports::Value::Text("Yes".into()),
    );
    let mut e = engine(&world);
    e.accept_disclaimer().expect("accepting works");
    drop(e);
    world.restart_process();
    world
}

pub fn machine(name: &str) -> Address {
    Address::Reg {
        root: RegRoot::Machine,
        path: r"Software\Test".into(),
        name: name.into(),
    }
}

pub fn user(name: &str) -> Address {
    Address::Reg {
        root: RegRoot::User { sid: SID.into() },
        path: r"Software\Test".into(),
        name: name.into(),
    }
}

pub fn bcd(element: &str) -> Address {
    Address::Bcd {
        element: element.into(),
    }
}

pub fn drs_base(setting: u32) -> Address {
    Address::Drs {
        profile: DrsProfile::Base,
        setting,
    }
}

pub fn request(id: &str) -> TweakRequest {
    TweakRequest::new(id)
}

/// A confirmation good enough for any plan.
pub fn full_confirmation(plan: &Plan) -> Confirmation {
    let mut confirmation = Confirmation {
        risks_ticked: true,
        ..Confirmation::default()
    };
    for tweak in &plan.tweaks {
        let word = if tweak.danger >= 4 {
            TYPED_EXPERT
        } else {
            TYPED_HIGH
        };
        confirmation
            .typed
            .insert(tweak.instance.clone(), word.into());
    }
    confirmation
}

pub fn apply(e: &mut TestEngine, ids: &[&str]) -> augeo_engine::Report {
    let requests: Vec<_> = ids.iter().map(|id| request(id)).collect();
    let plan = e.plan_apply(&requests).expect("planning works");
    let confirmation = full_confirmation(&plan);
    e.execute(&plan, &confirmation).expect("execution works")
}

pub fn undo_all(e: &mut TestEngine, policy: UndoPolicy) -> augeo_engine::Report {
    let plan = e.plan_undo_all(policy).expect("planning undo works");
    e.execute(&plan, &Confirmation::default())
        .expect("undo works")
}

/// Resolves every interrupted change with its default decision.
pub fn recover_all(e: &mut TestEngine) {
    for item in e.in_doubt().expect("listing works") {
        let decision: Decision = item.default_decision();
        e.resolve(item.txn, decision).expect("recovery works");
    }
}
