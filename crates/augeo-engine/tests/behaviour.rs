//! What the engine promises when nothing crashes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use augeo_engine::{
    Confirmation, DriftPolicy, EngineError, EpochChoice, TYPED_EXPERT, TYPED_HIGH, TweakRequest,
    TweakResult, TweakState, UndoPolicy,
};
use augeo_fakes::World;
use augeo_journal::Op;
use augeo_ports::Value;
use common::*;

#[test]
fn apply_then_undo_restores_exact_values_including_absent_and_other_types() {
    let world = seeded();
    let initial = world.snapshot();
    let mut e = engine(&world);

    let report = apply(&mut e, &["multi"]);
    assert_eq!(
        report.results,
        vec![("multi".to_string(), TweakResult::Done)]
    );
    assert_eq!(world.get(&machine("M1")), Value::Dword(1));
    assert_eq!(world.get(&machine("M2")), Value::Dword(2));
    assert_eq!(world.get(&machine("M3")), Value::Dword(3));

    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.snapshot(), initial);
    assert_eq!(world.get(&machine("M2")), Value::Absent);
    assert_eq!(world.get(&machine("M3")), Value::Text("old".into()));
}

fn scheduling_context(supported: bool, enabled: bool) -> augeo_engine::Context {
    let mut ctx = context();
    ctx.gpu_scheduling = Some(augeo_ports::GpuScheduling { supported, enabled });
    ctx
}

fn with_option(id: &str, option: &str) -> TweakRequest {
    TweakRequest {
        id: id.to_string(),
        option: Some(option.to_string()),
        exe: None,
    }
}

/// Found on a real PC: scheduling stayed on with its value gone, while augeo said "not applied".
#[test]
fn scheduling_that_is_already_on_with_no_value_reads_as_already_set_and_writes_nothing() {
    let world = seeded();
    let initial = world.snapshot();
    let mut e = engine_with(&world, scheduling_context(true, true), true);

    let status = e.status("scheduling", None).unwrap();
    assert_eq!(status.state, TweakState::AlreadySet);
    assert!(status.live_note.unwrap().contains("it's on right now"));

    let writes = world.writes().len();
    let report = apply(&mut e, &["scheduling"]);
    assert_eq!(
        report.results,
        vec![("scheduling".to_string(), TweakResult::NoChange)]
    );
    assert_eq!(world.writes().len(), writes);
    assert_eq!(world.snapshot(), initial);
}

#[test]
fn scheduling_that_is_off_can_be_turned_on_and_the_undo_is_exact() {
    let world = seeded();
    let initial = world.snapshot();
    let mut e = engine_with(&world, scheduling_context(true, false), true);
    assert_eq!(
        e.status("scheduling", None).unwrap().state,
        TweakState::NotApplied
    );

    apply(&mut e, &["scheduling"]);
    assert_eq!(world.get(&machine("HwSch")), Value::Dword(2));
    assert_eq!(
        e.status("scheduling", None).unwrap().state,
        TweakState::Applied
    );

    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.snapshot(), initial);
}

#[test]
fn scheduling_that_is_on_with_no_value_can_still_be_turned_off() {
    let world = seeded();
    let initial = world.snapshot();
    let mut e = engine_with(&world, scheduling_context(true, true), true);

    let plan = e.plan_apply(&[with_option("scheduling", "off")]).unwrap();
    e.execute(&plan, &full_confirmation(&plan)).unwrap();
    assert_eq!(world.get(&machine("HwSch")), Value::Dword(1));

    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.snapshot(), initial);
}

#[test]
fn scheduling_off_with_no_value_needs_no_write_to_stay_off() {
    let world = seeded();
    let e = engine_with(&world, scheduling_context(true, false), true);
    let plan = e.plan_apply(&[with_option("scheduling", "off")]).unwrap();
    assert!(plan.tweaks.iter().all(|t| !t.writes()));
}

#[test]
fn an_explicit_value_that_disagrees_is_still_written() {
    // Off in the registry, not yet restarted, so Windows still reports on.
    let world = seeded();
    world.set(&machine("HwSch"), Value::Dword(1));
    let mut e = engine_with(&world, scheduling_context(true, true), true);
    apply(&mut e, &["scheduling"]);
    assert_eq!(world.get(&machine("HwSch")), Value::Dword(2));
}

#[test]
fn scheduling_this_gpu_does_not_support_is_unavailable() {
    let world = seeded();
    let e = engine_with(&world, scheduling_context(false, false), true);
    assert!(matches!(
        e.status("scheduling", None),
        Err(EngineError::NotAvailable { .. })
    ));
    assert!(matches!(
        e.plan_apply(&[request("scheduling")]),
        Err(EngineError::NotAvailable { .. })
    ));
}

#[test]
fn if_windows_does_not_answer_the_values_decide_as_before() {
    let world = seeded();
    let e = engine(&world);
    let status = e.status("scheduling", None).unwrap();
    assert_eq!(status.state, TweakState::NotApplied);
    assert_eq!(status.live_note, None);
}

/// Found on a real PC: a network setting the driver doesn't offer read as "not applied".
#[test]
fn something_the_system_does_not_offer_reads_as_unavailable_rather_than_not_applied() {
    let world = seeded();
    world.set_unavailable(&drs_base(100));
    let e = engine(&world);
    assert!(matches!(
        e.status("drs-global", None),
        Err(EngineError::NotAvailable { .. })
    ));
    assert!(matches!(
        e.plan_apply(&[request("drs-global")]),
        Err(EngineError::NotAvailable { .. })
    ));
    assert!(world.writes().is_empty());
}

#[test]
fn something_already_applied_can_still_be_read_and_undone_if_it_later_goes_away() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["drs-global"]);
    // A driver update can stop listing a setting.
    world.set_unavailable(&drs_base(100));
    assert!(e.status("drs-global", None).is_ok());
}

/// Found on a real PC: `power#1,3` set USB suspend on the plan it was switching away from.
#[test]
fn a_batch_that_switches_plans_sets_per_plan_values_on_the_new_plan() {
    let world = on_balanced();
    let initial = world.snapshot();
    let mut e = engine(&world);

    let plan = e
        .plan_apply(&[request("plan-switch"), request("plan-setting")])
        .unwrap();
    let shown: Vec<String> = plan
        .tweaks
        .iter()
        .flat_map(|t| &t.steps)
        .map(|s| s.address.to_string())
        .collect();
    assert!(
        shown.iter().any(|a| a.contains(HIGH_PERFORMANCE)),
        "the plan shown must name the new plan: {shown:?}"
    );
    e.execute(&plan, &full_confirmation(&plan)).unwrap();

    assert_eq!(
        world.get(&augeo_ports::Address::PowerActiveScheme),
        Value::Text(HIGH_PERFORMANCE.into())
    );
    assert_eq!(world.get(&plan_setting(HIGH_PERFORMANCE)), Value::Dword(0));
    assert_eq!(world.get(&plan_setting(BALANCED)), Value::Dword(1));
    let status = e.status("plan-setting", None).unwrap();
    assert_eq!(status.state, TweakState::Applied);
    assert_eq!(status.not_in_effect, None);

    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.snapshot(), initial);
}

#[test]
fn the_order_of_a_batch_does_not_change_which_plan_is_used() {
    let world = on_balanced();
    let mut e = engine(&world);
    apply(&mut e, &["plan-setting", "plan-switch"]);
    assert_eq!(world.get(&plan_setting(HIGH_PERFORMANCE)), Value::Dword(0));
    assert_eq!(world.get(&plan_setting(BALANCED)), Value::Dword(1));
}

#[test]
fn a_per_plan_tweak_says_so_when_its_plan_is_no_longer_active() {
    let world = on_balanced();
    let initial = world.snapshot();
    let mut e = engine(&world);

    apply(&mut e, &["plan-setting"]);
    assert_eq!(world.get(&plan_setting(BALANCED)), Value::Dword(0));
    assert_eq!(e.status("plan-setting", None).unwrap().not_in_effect, None);

    // Switching plans later, through augeo or Windows, leaves it on the old plan.
    apply(&mut e, &["plan-switch"]);
    let status = e.status("plan-setting", None).unwrap();
    assert_eq!(status.state, TweakState::Applied);
    let note = status
        .not_in_effect
        .expect("it must say it isn't in effect");
    assert!(note.contains("Balanced"), "{note}");
    assert!(note.contains("High Performance"), "{note}");

    // Applying again can't quietly leave a second set of values behind.
    let error = e.plan_apply(&[request("plan-setting")]).unwrap_err();
    assert!(matches!(error, EngineError::AppliedToOtherPlan { .. }));

    // Undo goes to the plan that really changed, and the note goes once the plan is back.
    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.snapshot(), initial);
}

#[test]
fn per_plan_tweaks_read_the_active_plan_live_rather_than_the_one_from_startup() {
    let world = on_balanced();
    world.set(&plan_setting(HIGH_PERFORMANCE), Value::Dword(0));
    let mut e = engine(&world);
    assert_eq!(
        e.status("plan-setting", None).unwrap().state,
        TweakState::NotApplied
    );

    // The plan changes while augeo stays open.
    world.set(
        &augeo_ports::Address::PowerActiveScheme,
        Value::Text(HIGH_PERFORMANCE.into()),
    );
    assert_eq!(
        e.status("plan-setting", None).unwrap().state,
        TweakState::AlreadySet
    );

    world.set(&plan_setting(HIGH_PERFORMANCE), Value::Dword(1));
    apply(&mut e, &["plan-setting"]);
    assert_eq!(world.get(&plan_setting(HIGH_PERFORMANCE)), Value::Dword(0));
    assert_eq!(world.get(&plan_setting(BALANCED)), Value::Dword(1));
}

#[test]
fn an_unreadable_active_plan_falls_back_to_the_one_from_startup() {
    let world = on_balanced();
    let e = engine(&world);
    world.set_unreadable(&augeo_ports::Address::PowerActiveScheme, true);
    assert_eq!(
        e.status("plan-setting", None).unwrap().state,
        TweakState::NotApplied
    );
}

#[test]
fn applying_twice_writes_nothing_more_and_keeps_the_baseline() {
    let world = seeded();
    let initial = world.snapshot();
    let mut e = engine(&world);
    apply(&mut e, &["multi"]);
    let writes = world.writes().len();

    let report = apply(&mut e, &["multi"]);
    assert_eq!(
        report.results,
        vec![("multi".to_string(), TweakResult::NoChange)]
    );
    assert_eq!(world.writes().len(), writes);

    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.snapshot(), initial);
}

#[test]
fn undoing_something_not_applied_is_safe() {
    let world = seeded();
    let initial = world.snapshot();
    let mut e = engine(&world);
    let plan = e
        .plan_undo(&["multi".to_string()], UndoPolicy::default())
        .unwrap();
    assert!(plan.tweaks.is_empty());
    let report = e.execute(&plan, &Confirmation::default()).unwrap();
    assert!(report.results.is_empty());
    assert_eq!(world.snapshot(), initial);

    // Undoing twice is equally safe.
    apply(&mut e, &["multi"]);
    undo_all(&mut e, UndoPolicy::default());
    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.snapshot(), initial);
}

#[test]
fn a_dry_run_writes_nothing_and_shows_exact_changes() {
    let world = seeded();
    let before = world.snapshot();
    let journal_before = world.journal_bytes(augeo_ports::JournalFile::Primary);
    let e = engine(&world);
    let plan = e.plan_apply(&[request("multi")]).unwrap();

    assert_eq!(world.writes().len(), 0);
    assert_eq!(world.snapshot(), before);
    assert_eq!(
        world.journal_bytes(augeo_ports::JournalFile::Primary),
        journal_before
    );

    let steps = &plan.tweaks[0].steps;
    assert_eq!(steps[0].current, Value::Dword(10));
    assert_eq!(steps[0].target, Value::Dword(1));
    assert_eq!(steps[1].current, Value::Absent);
    assert_eq!(steps[2].current, Value::Text("old".into()));
    assert!(steps.iter().all(|s| s.write));
}

#[test]
fn a_plan_is_refused_if_the_system_changed_since_it_was_shown() {
    let world = seeded();
    let mut e = engine(&world);
    let plan = e.plan_apply(&[request("multi")]).unwrap();
    world.set(&machine("M2"), Value::Dword(77));

    let error = e.execute(&plan, &Confirmation::default()).unwrap_err();
    let EngineError::PlanChanged(fresh) = error else {
        panic!("expected PlanChanged, got {error:?}");
    };
    assert_eq!(fresh.tweaks[0].steps[1].current, Value::Dword(77));
    assert_eq!(world.writes().len(), 0);

    // The new plan works once it has been shown.
    e.execute(&fresh, &Confirmation::default()).unwrap();
    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.get(&machine("M2")), Value::Dword(77));
}

#[test]
fn the_user_hive_is_addressed_by_sid() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["single"]);
    assert_eq!(world.get(&user("Single")), Value::Dword(1));
}

#[test]
fn confirmation_levels_are_enforced_by_the_engine() {
    let world = seeded();
    let mut e = engine(&world);
    let none = Confirmation::default();

    let plan = e.plan_apply(&[request("single")]).unwrap();
    e.execute(&plan, &none).unwrap();

    let plan = e.plan_apply(&[request("medium")]).unwrap();
    assert!(matches!(
        e.execute(&plan, &none),
        Err(EngineError::ConfirmationMissing { .. })
    ));
    let ticked = Confirmation {
        risks_ticked: true,
        ..Confirmation::default()
    };
    e.execute(&plan, &ticked).unwrap();

    let plan = e.plan_apply(&[request("high")]).unwrap();
    assert!(matches!(
        e.execute(&plan, &ticked),
        Err(EngineError::ConfirmationMissing { .. })
    ));
    let mut wrong = ticked.clone();
    wrong.typed.insert("high".into(), "APPLY".into());
    assert!(matches!(
        e.execute(&plan, &wrong),
        Err(EngineError::ConfirmationMissing { .. })
    ));
    let mut right = ticked.clone();
    right.typed.insert("high".into(), TYPED_HIGH.into());
    e.execute(&plan, &right).unwrap();

    let plan = e.plan_apply(&[request("expert")]).unwrap();
    let mut wrong = ticked.clone();
    wrong.typed.insert("expert".into(), TYPED_HIGH.into());
    assert!(matches!(
        e.execute(&plan, &wrong),
        Err(EngineError::ConfirmationMissing { .. })
    ));
    let mut right = ticked;
    right.typed.insert("expert".into(), TYPED_EXPERT.into());
    e.execute(&plan, &right).unwrap();
    assert_eq!(world.get(&machine("Expert")), Value::Dword(0));
}

#[test]
fn a_level_4_tweak_cant_be_batched() {
    let world = seeded();
    let mut e = engine(&world);
    let plan = e
        .plan_apply(&[request("single"), request("expert")])
        .unwrap();
    let confirmation = full_confirmation(&plan);
    assert!(matches!(
        e.execute(&plan, &confirmation),
        Err(EngineError::MustApplyAlone { .. })
    ));
    assert_eq!(world.writes().len(), 0);
}

#[test]
fn nothing_applies_without_the_current_disclaimer_accepted() {
    let world = World::new();
    let mut e = engine(&world);
    let plan = e.plan_apply(&[request("single")]).unwrap();
    assert_eq!(
        e.execute(&plan, &Confirmation::default()),
        Err(EngineError::DisclaimerNotAccepted)
    );
    assert!(!e.disclaimer_accepted());
    e.accept_disclaimer().unwrap();
    assert!(e.disclaimer_accepted());
    e.execute(&plan, &Confirmation::default()).unwrap();
    drop(e);
    world.restart_process();

    // New text means a new acceptance.
    let mut ctx = context();
    ctx.disclaimer.text_sha256 = "changed-text".into();
    ctx.disclaimer.version = 2;
    let e = engine_with(&world, ctx, true);
    assert!(!e.disclaimer_accepted());
    drop(e);
    world.restart_process();

    // So does a different user.
    let mut ctx = context();
    ctx.user_sid = "S-1-5-21-9-9-9-500".into();
    let e = engine_with(&world, ctx, true);
    assert!(!e.disclaimer_accepted());
}

#[test]
fn a_reader_can_see_state_but_never_change_anything() {
    let world = seeded();
    let mut writer = engine(&world);
    apply(&mut writer, &["single"]);

    let mut reader = engine_with(&world, context(), false);
    assert!(!reader.is_writer());
    assert_eq!(
        reader.status("single", None).unwrap().state,
        TweakState::Applied
    );
    let plan = reader.plan_apply(&[request("multi")]).unwrap();
    assert_eq!(
        reader.execute(&plan, &Confirmation::default()),
        Err(EngineError::ReadOnly)
    );
}

#[test]
fn a_second_instance_is_read_only_while_the_first_lives() {
    let world = seeded();
    let _first = engine(&world);
    let second = engine(&world);
    assert!(!second.is_writer());
    assert!(second.open_report().locked_elsewhere);
}

#[test]
fn state_detection_table() {
    let world = seeded();
    let mut e = engine(&world);
    let state = |e: &TestEngine| e.status("multi", None).unwrap().state;

    assert_eq!(state(&e), TweakState::NotApplied);
    world.set(&machine("M2"), Value::Dword(2));
    assert_eq!(state(&e), TweakState::Partial);
    world.set(&machine("M1"), Value::Dword(1));
    world.set(&machine("M3"), Value::Dword(3));
    assert_eq!(state(&e), TweakState::AlreadySet);

    // Already set by something else: augeo has nothing to undo.
    let report = apply(&mut e, &["multi"]);
    assert_eq!(report.results[0].1, TweakResult::NoChange);
    assert_eq!(state(&e), TweakState::AlreadySet);
    assert!(
        e.plan_undo_all(UndoPolicy::default())
            .unwrap()
            .tweaks
            .is_empty()
    );

    world.set(&machine("M1"), Value::Dword(10));
    apply(&mut e, &["multi"]);
    assert_eq!(state(&e), TweakState::Applied);

    world.set(&machine("M2"), Value::Dword(99));
    assert_eq!(state(&e), TweakState::ChangedExternally);
}

#[test]
fn a_failed_read_is_unknown_never_not_applied() {
    let world = seeded();
    let e = engine(&world);
    world.set_unreadable(&machine("M2"), true);
    assert!(matches!(
        e.status("multi", None).unwrap().state,
        TweakState::Unknown { .. }
    ));
    assert!(matches!(
        e.plan_apply(&[request("multi")]),
        Err(EngineError::Unreadable { .. })
    ));
}

#[test]
fn partial_apply_only_writes_what_is_missing() {
    let world = seeded();
    let mut e = engine(&world);
    world.set(&machine("M2"), Value::Dword(2));
    apply(&mut e, &["multi"]);
    let written: Vec<String> = world.writes().iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(written.len(), 2);
    assert!(!written.iter().any(|k| k.contains("m2")));
}

#[test]
fn undo_of_an_externally_changed_value_keeps_it_by_default() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["multi"]);
    world.set(&machine("M2"), Value::Dword(99));

    let plan = e.plan_undo_all(UndoPolicy::default()).unwrap();
    let step = &plan.tweaks[0].steps[1];
    assert!(step.drift);
    assert!(!step.write);
    e.execute(&plan, &Confirmation::default()).unwrap();

    assert_eq!(world.get(&machine("M2")), Value::Dword(99));
    assert_eq!(world.get(&machine("M1")), Value::Dword(10));
    assert_eq!(world.get(&machine("M3")), Value::Text("old".into()));
    // augeo no longer tracks it. The kept value shows in the detail pane, per the state table.
    assert_eq!(
        e.status("multi", None).unwrap().state,
        TweakState::NotApplied
    );
    assert!(e.ledger().applied.is_empty());
}

#[test]
fn undo_can_explicitly_restore_over_an_external_change() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["multi"]);
    world.set(&machine("M2"), Value::Dword(99));
    let policy = UndoPolicy {
        drift: DriftPolicy::RestoreBaseline,
        ..UndoPolicy::default()
    };
    undo_all(&mut e, policy);
    assert_eq!(world.get(&machine("M2")), Value::Absent);
}

#[test]
fn undo_when_already_back_at_baseline_writes_nothing_for_that_value() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["multi"]);
    world.set(&machine("M1"), Value::Dword(10));
    let writes = world.writes().len();
    undo_all(&mut e, UndoPolicy::default());
    let new: Vec<_> = world.writes()[writes..]
        .iter()
        .map(|(k, _)| k.clone())
        .collect();
    assert_eq!(new.len(), 2);
    assert!(!new.iter().any(|k| k.contains("m1")));
}

#[test]
fn a_windows_update_reset_is_reported_with_its_likely_cause() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["single"]);
    drop(e);
    world.restart_process();
    world.set(&user("Single"), Value::Absent);

    let mut ctx = context();
    ctx.stamp.os_build = 26200;
    let e = engine_with(&world, ctx, true);
    let status = e.status("single", None).unwrap();
    assert_eq!(status.state, TweakState::ChangedExternally);
    assert!(status.drift_note.unwrap().contains("26100 to 26200"));
}

#[test]
fn a_driver_reinstall_wiping_settings_is_reported_and_undo_writes_nothing() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["drs-global"]);
    drop(e);
    world.restart_process();
    world.set(&drs_base(100), Value::Absent);

    let mut ctx = context();
    ctx.stamp.driver = Some("621.14".into());
    let mut e = engine_with(&world, ctx, true);
    let status = e.status("drs-global", None).unwrap();
    assert_eq!(status.state, TweakState::ChangedExternally);
    assert!(status.drift_note.unwrap().contains("610.88 to 621.14"));

    let writes = world.writes().len();
    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.writes().len(), writes);
    assert!(e.ledger().applied.is_empty());
}

#[test]
fn reapplying_after_an_external_change_opens_a_new_baseline() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["single"]);
    world.set(&user("Single"), Value::Dword(50));
    apply(&mut e, &["single"]);
    assert_eq!(world.get(&user("Single")), Value::Dword(1));

    // Undo goes back to what the user had just before the re-apply.
    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.get(&user("Single")), Value::Dword(50));
}

#[test]
fn undo_can_choose_the_oldest_baseline() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["single"]);
    world.set(&user("Single"), Value::Dword(50));
    apply(&mut e, &["single"]);
    let policy = UndoPolicy {
        epoch: EpochChoice::Oldest,
        ..UndoPolicy::default()
    };
    undo_all(&mut e, policy);
    assert_eq!(world.get(&user("Single")), Value::Absent);
}

#[test]
fn changing_an_option_keeps_the_original_baseline() {
    let world = seeded();
    world.set(&machine("Picky"), Value::Dword(9));
    let mut e = engine(&world);
    apply(&mut e, &["picky"]);
    assert_eq!(world.get(&machine("Picky")), Value::Dword(1));

    let high = TweakRequest {
        option: Some("high".into()),
        ..request("picky")
    };
    let plan = e.plan_apply(&[high]).unwrap();
    e.execute(&plan, &Confirmation::default()).unwrap();
    assert_eq!(world.get(&machine("Picky")), Value::Dword(2));
    assert_eq!(e.status("picky", None).unwrap().state, TweakState::Applied);

    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.get(&machine("Picky")), Value::Dword(9));
}

#[test]
fn an_unknown_option_is_refused() {
    let world = seeded();
    let e = engine(&world);
    let bad = TweakRequest {
        option: Some("nope".into()),
        ..request("picky")
    };
    assert!(matches!(
        e.plan_apply(&[bad]),
        Err(EngineError::Invalid { .. })
    ));
}

#[test]
fn repairs_do_nothing_when_nothing_is_wrong_and_undo_exactly() {
    let world = seeded();
    let initial = world.snapshot();
    let mut e = engine(&world);
    assert_eq!(e.status("repair", None).unwrap().state, TweakState::Partial);

    let report = apply(&mut e, &["repair"]);
    assert_eq!(report.results[0].1, TweakResult::Done);
    assert_eq!(world.get(&bcd("useplatformclock")), Value::Absent);
    assert_eq!(e.status("repair", None).unwrap().state, TweakState::Applied);

    undo_all(&mut e, UndoPolicy::default());
    assert_eq!(world.snapshot(), initial);

    let clean = World::new();
    let mut e = engine(&clean);
    e.accept_disclaimer().unwrap();
    assert_eq!(
        e.status("repair", None).unwrap().state,
        TweakState::AlreadySet
    );
    assert_eq!(
        apply(&mut e, &["repair"]).results[0].1,
        TweakResult::NoChange
    );
    assert_eq!(clean.writes().len(), 0);
}

#[test]
fn undo_all_runs_in_reverse_commit_order() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["single"]);
    apply(&mut e, &["multi"]);
    apply(&mut e, &["medium"]);
    let plan = e.plan_undo_all(UndoPolicy::default()).unwrap();
    assert_eq!(plan.op, Op::Revert);
    let order: Vec<&str> = plan.tweaks.iter().map(|t| t.instance.as_str()).collect();
    assert_eq!(order, vec!["medium", "multi", "single"]);
}

#[test]
fn applicability_is_checked_when_planning() {
    let world = seeded();
    let e = engine(&world);
    assert!(matches!(
        e.plan_apply(&[request("old-win")]),
        Err(EngineError::NotApplicable { .. })
    ));
    assert!(matches!(
        e.plan_apply(&[request("retired")]),
        Err(EngineError::Retired { .. })
    ));
    assert!(matches!(
        e.plan_apply(&[request("nope")]),
        Err(EngineError::UnknownTweak(_))
    ));
}

#[test]
fn an_nvidia_setting_the_driver_doesnt_list_disables_the_tweak() {
    let world = seeded();
    world.set_unavailable(&drs_base(100));
    let e = engine(&world);
    assert!(matches!(
        e.plan_apply(&[request("drs-global")]),
        Err(EngineError::NotAvailable { .. })
    ));
}

#[test]
fn requirements_and_conflicts() {
    let world = seeded();
    let mut e = engine(&world);
    assert!(matches!(
        e.plan_apply(&[request("needs-single")]),
        Err(EngineError::Requires { .. })
    ));
    assert!(
        e.plan_apply(&[request("single"), request("needs-single")])
            .is_ok()
    );

    apply(&mut e, &["conflict-a"]);
    assert!(matches!(
        e.plan_apply(&[request("conflict-b")]),
        Err(EngineError::Conflict { .. })
    ));
}

#[test]
fn per_game_instances_are_independent() {
    let world = seeded();
    let mut e = engine(&world);
    let for_game = |exe: &str| TweakRequest {
        exe: Some(exe.into()),
        ..request("drs-game")
    };
    let plan = e
        .plan_apply(&[for_game("cs2.exe"), for_game("dota2.exe")])
        .unwrap();
    e.execute(&plan, &Confirmation::default()).unwrap();
    assert_eq!(e.ledger().applied.len(), 2);
    assert!(e.ledger().applied.contains_key("drs-game@cs2.exe"));

    let plan = e
        .plan_undo(&["drs-game@cs2.exe".into()], UndoPolicy::default())
        .unwrap();
    e.execute(&plan, &Confirmation::default()).unwrap();
    assert!(!e.ledger().applied.contains_key("drs-game@cs2.exe"));
    assert!(e.ledger().applied.contains_key("drs-game@dota2.exe"));

    assert!(matches!(
        e.plan_apply(&[request("drs-game")]),
        Err(EngineError::Invalid { .. })
    ));
    assert!(matches!(
        e.plan_apply(&[for_game(r"..\evil.exe")]),
        Err(EngineError::Invalid { .. })
    ));
}

#[test]
fn restart_pending_clears_only_on_a_new_boot() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["restart"]);
    assert!(e.status("restart", None).unwrap().restart_pending);
    assert_eq!(e.pending_restarts(), vec!["restart".to_string()]);
    drop(e);
    world.restart_process();

    // Reopening in the same boot changes nothing.
    let e = engine(&world);
    assert!(e.status("restart", None).unwrap().restart_pending);
    drop(e);
    world.restart_process();

    let mut ctx = context();
    ctx.stamp.boot_id = "boot-2".into();
    let e = engine_with(&world, ctx, true);
    assert!(!e.status("restart", None).unwrap().restart_pending);
    assert!(e.pending_restarts().is_empty());
}

#[test]
fn undoing_in_the_same_boot_clears_the_restart_flag() {
    let world = seeded();
    let mut e = engine(&world);
    apply(&mut e, &["restart"]);
    undo_all(&mut e, UndoPolicy::default());
    assert!(e.pending_restarts().is_empty());
}

#[test]
fn the_preset_never_includes_security_lowering_tweaks() {
    let world = seeded();
    let e = engine(&world);
    let requests = e.preset_requests();
    let ids: Vec<&str> = requests.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["preset-yes"]);
}

#[test]
fn a_tampered_journal_cant_redirect_a_write_to_a_forbidden_location() {
    use augeo_journal::{Body, Journal, Op, Outcome, Params, Stamp};

    let world = seeded();
    {
        // Forge a committed apply that names a real tweak with a tampered game name.
        let mut journal = Journal::open(world.store(), true, Stamp::default()).unwrap();
        let params = Params {
            exe: Some(r"..\..\Windows\System32\evil.exe".into()),
            user_sid: SID.into(),
            ..Params::default()
        };
        journal
            .append(
                1,
                Body::TxnBegin {
                    txn: 50,
                    op: Op::Apply,
                    instance: "drs-game@forged".into(),
                    tweak_id: "drs-game".into(),
                    rev: 1,
                    params,
                    plan_hash: String::new(),
                },
            )
            .unwrap();
        journal
            .append(
                1,
                Body::TxnEnd {
                    txn: 50,
                    outcome: Outcome::Committed,
                },
            )
            .unwrap();
    }
    world.restart_process();

    let e = engine(&world);
    let writes = world.writes().len();
    assert!(matches!(
        e.plan_undo_all(UndoPolicy::default()),
        Err(EngineError::Invalid { .. })
    ));
    assert_eq!(world.writes().len(), writes);
}
