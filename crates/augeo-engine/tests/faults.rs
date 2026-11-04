//! The release gate: fail or crash at every port call and prove nothing is ever lost.
//!
//! The invariant in every test: after recovery, the tweak is either fully applied, not applied or
//! honestly partial, and undoing everything puts the machine back byte for byte.
//!
//! A sweep only stops once a run completes without its fault firing, so it can't pass by stopping
//! early. If a sweep hits the limit, it fails.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;

use augeo_engine::{Confirmation, EngineError, TweakResult, TweakState, UndoPolicy};
use augeo_fakes::World;
use augeo_journal::Decision;
use augeo_ports::{JournalFile, Value};
use common::*;

const SWEEP_LIMIT: u64 = 500;

/// How the process ended in a crash test.
#[derive(Clone, Copy, Debug)]
enum Death {
    /// The process died. Everything written stays.
    Process,
    /// Power cut. Unflushed journal bytes vanish.
    Power,
    /// Power cut mid-write: part of the unflushed record survives, tearing it.
    PowerTorn,
    /// Power cut, and the last system write was lost even though it reported success.
    PowerLostWrite,
}

fn die(world: &World, death: Death) {
    match death {
        Death::Process => world.restart_process(),
        Death::Power => world.power_loss(0),
        Death::PowerTorn => world.power_loss(7),
        Death::PowerLostWrite => {
            world.power_loss(0);
            world.lose_last_write();
        }
    }
}

/// After recovery the machine must be consistent, and undoing everything must restore it exactly.
fn assert_recovers(world: &World, initial: &BTreeMap<String, Value>, label: &str) {
    let mut e = engine(world);
    assert!(e.is_writer(), "{label}: not the writer");
    recover_all(&mut e);
    assert!(e.in_doubt().unwrap().is_empty(), "{label}: still in doubt");

    let state = e.status("multi", None).unwrap().state;
    match state {
        TweakState::NotApplied | TweakState::Applied | TweakState::Partial => {}
        other => panic!("{label}: unexpected state {other:?}"),
    }
    if state == TweakState::NotApplied {
        assert_eq!(
            world.snapshot(),
            *initial,
            "{label}: not applied, but values differ"
        );
    }

    let report = undo_all(&mut e, UndoPolicy::default());
    assert!(
        report
            .results
            .iter()
            .all(|(_, r)| matches!(r, TweakResult::Done | TweakResult::NoChange)),
        "{label}: undo reported {report:?}"
    );
    assert_eq!(
        world.snapshot(),
        *initial,
        "{label}: undo didn't restore everything"
    );
    assert!(
        e.ledger().applied.is_empty(),
        "{label}: still tracking something"
    );
}

fn apply_sweep(death: Death) {
    for at in 0..SWEEP_LIMIT {
        let world = seeded();
        let initial = world.snapshot();
        let fired = {
            let mut e = engine(&world);
            let plan = e.plan_apply(&[request("multi")]).unwrap();
            let confirmation = full_confirmation(&plan);
            world.crash_after(at);
            let _ = e.execute(&plan, &confirmation);
            world.crash_fired()
        };
        die(&world, death);
        assert_recovers(&world, &initial, &format!("{death:?} at call {at}"));
        if !fired {
            assert!(at > 20, "{death:?}: the sweep ended after only {at} calls");
            return;
        }
    }
    panic!("{death:?}: the sweep never reached a completed apply");
}

#[test]
fn crash_at_every_point_of_an_apply() {
    apply_sweep(Death::Process);
}

#[test]
fn power_loss_at_every_point_of_an_apply() {
    apply_sweep(Death::Power);
}

#[test]
fn torn_record_at_every_point_of_an_apply() {
    apply_sweep(Death::PowerTorn);
}

#[test]
fn lost_write_at_every_point_of_an_apply() {
    apply_sweep(Death::PowerLostWrite);
}

fn undo_sweep(death: Death) {
    for at in 0..SWEEP_LIMIT {
        let world = seeded();
        let initial = world.snapshot();
        let fired = {
            let mut e = engine(&world);
            apply(&mut e, &["multi"]);
            let plan = e.plan_undo_all(UndoPolicy::default()).unwrap();
            world.crash_after(at);
            let _ = e.execute(&plan, &Confirmation::default());
            world.crash_fired()
        };
        // A write lost after the undo committed would break the durability contract of
        // `System::write`, and nothing could detect it. So a write is only lost while the journal
        // still shows the undo as unfinished.
        if let Death::PowerLostWrite = death {
            world.power_loss(0);
            let unfinished = !engine_with(&world, context(), false)
                .ledger()
                .applied
                .is_empty();
            if unfinished {
                world.lose_last_write();
            }
        } else {
            die(&world, death);
        }
        assert_recovers(&world, &initial, &format!("undo {death:?} at call {at}"));
        if !fired {
            assert!(
                at > 20,
                "{death:?}: the undo sweep ended after only {at} calls"
            );
            return;
        }
    }
    panic!("{death:?}: the sweep never reached a completed undo");
}

#[test]
fn crash_at_every_point_of_an_undo() {
    undo_sweep(Death::Process);
}

#[test]
fn power_loss_at_every_point_of_an_undo() {
    undo_sweep(Death::Power);
}

#[test]
fn lost_write_at_every_point_of_an_undo() {
    undo_sweep(Death::PowerLostWrite);
}

/// Crashes an apply after `calls` port calls and returns the world, after the process has died.
fn interrupted_apply(calls: u64) -> World {
    let world = seeded();
    {
        let mut e = engine(&world);
        let plan = e.plan_apply(&[request("multi")]).unwrap();
        let confirmation = full_confirmation(&plan);
        world.crash_after(calls);
        let _ = e.execute(&plan, &confirmation);
    }
    world.restart_process();
    world
}

/// The fewest port calls that leave an interrupted apply with at least one recorded write.
fn calls_to_reach_in_doubt() -> u64 {
    (0..SWEEP_LIMIT)
        .find(|&calls| {
            let world = interrupted_apply(calls);
            !engine(&world).in_doubt().unwrap().is_empty()
        })
        .expect("some crash point leaves an interrupted apply")
}

#[test]
fn crash_at_every_point_of_a_recovery() {
    let start = calls_to_reach_in_doubt();
    let mut depths_with_recovery = 0;
    for first in start..SWEEP_LIMIT {
        if engine(&interrupted_apply(first))
            .in_doubt()
            .unwrap()
            .is_empty()
        {
            // Past the last point that leaves work to recover.
            if depths_with_recovery > 0 {
                break;
            }
            continue;
        }
        depths_with_recovery += 1;
        let mut completed = false;
        for second in 0..SWEEP_LIMIT {
            let world = interrupted_apply(first);
            let initial = {
                // The initial state is the seed, since nothing but this apply ran.
                seeded().snapshot()
            };
            let fired = {
                let mut e = engine(&world);
                let item = e.in_doubt().unwrap().remove(0);
                world.crash_after(second);
                let _ = e.resolve(item.txn, item.default_decision());
                world.crash_fired()
            };
            world.restart_process();
            assert_recovers(
                &world,
                &initial,
                &format!("crash at {first}, then at {second}"),
            );
            if !fired {
                completed = true;
                break;
            }
        }
        assert!(completed, "recovery from crash at {first} never completed");
    }
    assert!(
        depths_with_recovery >= 3,
        "only {depths_with_recovery} crash depths were tested"
    );
}

#[test]
fn one_failure_at_every_point_of_an_apply() {
    for at in 0..SWEEP_LIMIT {
        let world = seeded();
        let initial = world.snapshot();
        let mut e = engine(&world);
        let plan = e.plan_apply(&[request("multi")]).unwrap();
        let confirmation = full_confirmation(&plan);
        world.fail_after(at);
        let result = e.execute(&plan, &confirmation);
        let fired = world.failure_fired();
        world.clear_faults();

        if let Ok(report) = &result {
            match &report.results[0].1 {
                TweakResult::Done => {
                    assert_eq!(
                        e.status("multi", None).unwrap().state,
                        TweakState::Applied,
                        "{at}"
                    );
                }
                TweakResult::RolledBack { .. } => {
                    assert_eq!(
                        world.snapshot(),
                        initial,
                        "failure at {at} wasn't rolled back"
                    );
                }
                // The single failure hit the rollback's read or write path.
                TweakResult::RollbackFailed { .. } => {}
                other => panic!("failure at {at}: {other:?}"),
            }
        }

        drop(e);
        world.restart_process();
        assert_recovers(&world, &initial, &format!("failure at {at}: {result:?}"));
        if !fired {
            assert!(at > 20, "the sweep ended after only {at} calls");
            return;
        }
    }
    panic!("the sweep never ran past the end of an apply");
}

#[test]
fn one_failure_at_every_point_of_an_undo() {
    for at in 0..SWEEP_LIMIT {
        let world = seeded();
        let initial = world.snapshot();
        let mut e = engine(&world);
        apply(&mut e, &["multi"]);
        let plan = e.plan_undo_all(UndoPolicy::default()).unwrap();
        world.fail_after(at);
        let _ = e.execute(&plan, &Confirmation::default());
        let fired = world.failure_fired();
        world.clear_faults();
        drop(e);
        world.restart_process();
        assert_recovers(&world, &initial, &format!("undo failure at {at}"));
        if !fired {
            return;
        }
    }
    panic!("the sweep never ran past the end of an undo");
}

#[test]
fn a_write_that_silently_does_nothing_is_caught_and_rolled_back() {
    let world = seeded();
    let initial = world.snapshot();
    world.ignore_writes(&machine("M2"));
    let mut e = engine(&world);
    let report = apply(&mut e, &["multi"]);
    let TweakResult::RolledBack { error } = &report.results[0].1 else {
        panic!("expected a rollback, got {report:?}");
    };
    assert!(error.contains("read back"), "{error}");
    assert_eq!(world.snapshot(), initial);
    assert!(e.ledger().applied.is_empty());
}

#[test]
fn a_failed_rollback_keeps_the_baseline_so_undo_can_retry() {
    let mut saw_failed_rollback = false;
    for at in 0..SWEEP_LIMIT {
        let world = seeded();
        let initial = world.snapshot();
        // M2 ignores writes, so applying fails at step 2 and has to roll back M1.
        world.ignore_writes(&machine("M2"));
        let mut e = engine(&world);
        let plan = e.plan_apply(&[request("multi")]).unwrap();
        let confirmation = full_confirmation(&plan);
        world.fail_after(at);
        let result = e.execute(&plan, &confirmation);
        let fired = world.failure_fired();
        world.clear_faults();
        if let Ok(report) = &result
            && matches!(report.results[0].1, TweakResult::RollbackFailed { .. })
        {
            saw_failed_rollback = true;
            assert!(e.status("multi", None).unwrap().rollback_failed);
            let report = undo_all(&mut e, UndoPolicy::default());
            assert!(
                matches!(
                    report.results[0].1,
                    TweakResult::Done | TweakResult::NoChange
                ),
                "{report:?}"
            );
            assert_eq!(world.snapshot(), initial, "failure at {at}");
        }
        if !fired {
            break;
        }
    }
    assert!(
        saw_failed_rollback,
        "no failure position produced a failed rollback"
    );
}

/// An interrupted apply with at least one write done, for the tests below.
fn world_with_interrupted_apply() -> World {
    let calls = calls_to_reach_in_doubt();
    (calls..SWEEP_LIMIT)
        .map(interrupted_apply)
        .find(|w| w.get(&machine("M1")) == Value::Dword(1))
        .expect("some crash point leaves the first write done")
}

#[test]
fn recovery_leaves_a_third_value_alone() {
    let world = world_with_interrupted_apply();
    world.set(&machine("M1"), Value::Dword(555));
    let mut e = engine(&world);
    let items = e.in_doubt().unwrap();
    assert_eq!(items.len(), 1, "expected an interrupted apply");
    assert_eq!(items[0].default_decision(), Decision::RollBack);
    let report = e.resolve(items[0].txn, Decision::RollBack).unwrap();
    assert_eq!(report.left_alone, vec![machine("M1")]);
    assert_eq!(world.get(&machine("M1")), Value::Dword(555));
}

#[test]
fn nothing_else_can_be_applied_until_recovery_is_done() {
    let world = world_with_interrupted_apply();
    let mut e = engine(&world);
    assert_eq!(e.in_doubt().unwrap().len(), 1);
    assert!(e.status("multi", None).unwrap().in_doubt);
    assert_eq!(
        e.plan_apply(&[request("single")]).unwrap_err(),
        EngineError::RecoveryRequired
    );

    let txn = e.in_doubt().unwrap()[0].txn;
    e.resolve(txn, Decision::Keep).unwrap();
    assert!(e.plan_apply(&[request("single")]).is_ok());
    // Keeping an interrupted apply leaves it partly done, and honest about that.
    assert_eq!(e.status("multi", None).unwrap().state, TweakState::Partial);
}

#[test]
fn a_crash_before_anything_was_written_closes_itself() {
    for at in 0..calls_to_reach_in_doubt() {
        let world = interrupted_apply(at);
        let initial = seeded().snapshot();
        assert_eq!(world.snapshot(), initial, "crash at {at}");
        let e = engine(&world);
        assert!(e.in_doubt().unwrap().is_empty(), "crash at {at}");
        assert_eq!(
            e.status("multi", None).unwrap().state,
            TweakState::NotApplied,
            "crash at {at}"
        );
    }
}

#[test]
fn a_journal_with_lost_history_says_so_and_never_restores_a_guess() {
    let world = seeded();
    {
        let mut e = engine(&world);
        apply(&mut e, &["multi"]);
    }
    world.restart_process();

    // Destroy the same baseline record in both copies.
    let bytes = world.journal_bytes(JournalFile::Primary);
    let needle = b"\"baseline\"";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap();
    for file in [JournalFile::Primary, JournalFile::Mirror] {
        let mut damaged = world.journal_bytes(file);
        damaged[at] ^= 0xFF;
        world.set_journal_bytes(file, damaged);
    }

    let mut e = engine(&world);
    assert!(e.open_report().history_lost);
    let status = e.status("multi", None).unwrap();
    assert!(status.baseline_lost);

    let plan = e.plan_undo_all(UndoPolicy::default()).unwrap();
    assert!(
        plan.tweaks[0]
            .steps
            .iter()
            .any(|s| s.baseline_lost && !s.write)
    );
    e.execute(&plan, &Confirmation::default()).unwrap();
    // The value whose old value is gone stays as it is. Nothing is invented.
    assert_eq!(world.get(&machine("M1")), Value::Dword(1));
}
