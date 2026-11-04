use augeo_fakes::{FakeStore, World};
use augeo_ports::{JournalFile, Value};

use super::*;

fn stamp() -> Stamp {
    Stamp {
        boot_id: "boot1".into(),
        os_build: 26100,
        driver: Some("610.88".into()),
        app: "0.1.0".into(),
    }
}

fn open(world: &World) -> Journal<FakeStore> {
    Journal::open(world.store(), true, stamp()).unwrap()
}

fn accept(n: u32) -> Body {
    Body::Accept {
        text_sha256: format!("hash{n}"),
        version: n,
        sid: "S-1-5-21-1".into(),
    }
}

fn write_records(world: &World, count: u32) {
    let mut journal = open(world);
    for n in 0..count {
        journal.append(5, accept(n)).unwrap();
    }
    world.restart_process();
}

fn seqs(journal: &Journal<FakeStore>) -> Vec<u64> {
    journal.records().iter().map(|r| r.seq).collect()
}

#[test]
fn records_survive_reopening_and_sequence_continues() {
    let world = World::new();
    write_records(&world, 3);
    let mut journal = open(&world);
    assert_eq!(seqs(&journal), vec![1, 2, 3]);
    assert_eq!(*journal.report(), OpenReport::default());
    journal.append(5, accept(9)).unwrap();
    assert_eq!(seqs(&journal), vec![1, 2, 3, 4]);
    assert_eq!(journal.records()[3].os_build, 26100);
    assert_eq!(journal.records()[3].driver.as_deref(), Some("610.88"));
}

#[test]
fn both_copies_are_identical() {
    let world = World::new();
    write_records(&world, 3);
    assert_eq!(
        world.journal_bytes(JournalFile::Primary),
        world.journal_bytes(JournalFile::Mirror)
    );
}

#[test]
fn torn_tail_is_cut_off_and_nothing_is_lost() {
    let world = World::new();
    write_records(&world, 3);
    for file in [JournalFile::Primary, JournalFile::Mirror] {
        let mut bytes = world.journal_bytes(file);
        bytes.truncate(bytes.len() - 5);
        world.set_journal_bytes(file, bytes);
    }
    let journal = open(&world);
    assert_eq!(seqs(&journal), vec![1, 2]);
    assert!(journal.report().repaired);
    assert!(!journal.report().history_lost);
    assert!(!journal.report().preserved);
    world.restart_process();

    // The repair sticks: the next open finds nothing to do.
    let journal = open(&world);
    assert!(!journal.report().repaired);
    assert_eq!(seqs(&journal), vec![1, 2]);
}

#[test]
fn a_torn_primary_heals_from_the_mirror() {
    let world = World::new();
    write_records(&world, 3);
    let mirror = world.journal_bytes(JournalFile::Mirror);
    let mut torn = mirror.clone();
    torn.truncate(torn.len() - 5);
    world.set_journal_bytes(JournalFile::Primary, torn);

    let journal = open(&world);
    assert_eq!(seqs(&journal), vec![1, 2, 3]);
    assert!(journal.report().repaired);
    assert_eq!(world.journal_bytes(JournalFile::Primary), mirror);
}

#[test]
fn a_missing_mirror_is_rebuilt() {
    let world = World::new();
    write_records(&world, 3);
    let primary = world.journal_bytes(JournalFile::Primary);
    world.set_journal_bytes(JournalFile::Mirror, Vec::new());
    let journal = open(&world);
    assert_eq!(seqs(&journal), vec![1, 2, 3]);
    assert_eq!(world.journal_bytes(JournalFile::Mirror), primary);
}

fn flip_byte(world: &World, file: JournalFile, position: usize) {
    let mut bytes = world.journal_bytes(file);
    bytes[position] ^= 0xFF;
    world.set_journal_bytes(file, bytes);
}

#[test]
fn a_flipped_bit_in_the_middle_heals_from_the_other_copy_and_keeps_the_damaged_file() {
    let world = World::new();
    write_records(&world, 5);
    let good = world.journal_bytes(JournalFile::Mirror);
    flip_byte(&world, JournalFile::Primary, good.len() / 2);

    let journal = open(&world);
    assert_eq!(seqs(&journal), vec![1, 2, 3, 4, 5]);
    assert!(journal.report().repaired);
    assert!(journal.report().preserved);
    assert!(!journal.report().history_lost);
    assert_eq!(world.preserved(), vec![JournalFile::Primary]);
    assert_eq!(world.journal_bytes(JournalFile::Primary), good);
}

#[test]
fn different_damage_in_each_copy_still_recovers_everything() {
    let world = World::new();
    write_records(&world, 6);
    let good = world.journal_bytes(JournalFile::Mirror);
    flip_byte(&world, JournalFile::Primary, good.len() / 4);
    flip_byte(&world, JournalFile::Mirror, good.len() * 3 / 4);

    let journal = open(&world);
    assert_eq!(seqs(&journal), vec![1, 2, 3, 4, 5, 6]);
    assert!(!journal.report().history_lost);
    assert_eq!(world.journal_bytes(JournalFile::Primary), good);
    assert_eq!(world.journal_bytes(JournalFile::Mirror), good);
}

#[test]
fn the_same_damage_in_both_copies_reports_lost_history() {
    let world = World::new();
    write_records(&world, 5);
    let middle = world.journal_bytes(JournalFile::Primary).len() / 2;
    flip_byte(&world, JournalFile::Primary, middle);
    flip_byte(&world, JournalFile::Mirror, middle);

    let mut journal = open(&world);
    assert!(journal.report().history_lost);
    assert_eq!(journal.records().len(), 4);
    // New records still work, and never reuse a number.
    journal.append(5, accept(9)).unwrap();
    assert_eq!(journal.records().last().unwrap().seq, 6);
    world.restart_process();

    // The loss stays visible, but repair isn't repeated.
    let journal = open(&world);
    assert!(journal.report().history_lost);
    assert!(!journal.report().repaired);
}

#[test]
fn garbage_files_lose_nothing_that_can_be_saved() {
    let world = World::new();
    world.set_journal_bytes(JournalFile::Primary, vec![0xAB; 500]);
    world.set_journal_bytes(JournalFile::Mirror, vec![0xCD; 3]);
    let mut journal = open(&world);
    assert!(journal.records().is_empty());
    journal.append(5, accept(1)).unwrap();
    assert_eq!(journal.records().len(), 1);
}

#[test]
fn a_reader_never_changes_anything_on_disk() {
    let world = World::new();
    write_records(&world, 3);
    let mut torn = world.journal_bytes(JournalFile::Primary);
    torn.truncate(torn.len() - 5);
    world.set_journal_bytes(JournalFile::Primary, torn.clone());
    let mirror = world.journal_bytes(JournalFile::Mirror);

    let mut reader = Journal::open(world.store(), false, stamp()).unwrap();
    assert_eq!(seqs(&reader), vec![1, 2, 3]);
    assert!(!reader.is_writer());
    assert_eq!(reader.append(5, accept(9)), Err(JournalError::ReadOnly));
    assert_eq!(world.journal_bytes(JournalFile::Primary), torn);
    assert_eq!(world.journal_bytes(JournalFile::Mirror), mirror);
}

#[test]
fn a_second_writer_becomes_read_only() {
    let world = World::new();
    let mut first = open(&world);
    first.append(5, accept(1)).unwrap();

    let mut second = Journal::open(world.store(), true, stamp()).unwrap();
    assert!(!second.is_writer());
    assert!(second.report().locked_elsewhere);
    assert_eq!(second.append(5, accept(2)), Err(JournalError::ReadOnly));

    // A dead process's lock is released by the kernel.
    world.restart_process();
    assert!(open(&world).is_writer());
}

#[test]
fn a_newer_schema_makes_this_build_read_only() {
    let world = World::new();
    write_records(&world, 1);
    let record = Record {
        seq: 2,
        ts_ms: 1,
        schema: SCHEMA + 1,
        boot_id: "b".into(),
        os_build: 1,
        driver: None,
        app: "9".into(),
        body: accept(1),
    };
    let future = frame(&record).unwrap();
    for file in [JournalFile::Primary, JournalFile::Mirror] {
        let mut bytes = world.journal_bytes(file);
        bytes.extend_from_slice(&future);
        world.set_journal_bytes(file, bytes);
    }
    let before = world.journal_bytes(JournalFile::Primary);
    let journal = open(&world);
    assert!(journal.report().newer_schema);
    assert!(!journal.is_writer());
    assert_eq!(world.journal_bytes(JournalFile::Primary), before);
}

#[test]
fn power_loss_before_a_flush_loses_only_the_unflushed_record() {
    let world = World::new();
    let mut journal = open(&world);
    journal.append(5, accept(1)).unwrap();
    // Fail the flush of the next append's primary copy: append, then flush.
    world.fail_after(1);
    assert!(journal.append(5, accept(2)).is_err());
    world.power_loss(10);

    let journal = open(&world);
    assert_eq!(seqs(&journal), vec![1]);
    assert!(!journal.report().history_lost);
}

fn apply_records(journal: &mut Journal<FakeStore>, txn: u64, outcome: Option<Outcome>) {
    journal
        .append(
            5,
            Body::TxnBegin {
                txn,
                op: Op::Apply,
                instance: "a".into(),
                tweak_id: "a".into(),
                rev: 1,
                params: Params::default(),
                plan_hash: "h".into(),
            },
        )
        .unwrap();
    journal
        .append(
            5,
            Body::Baseline {
                txn,
                res_key: "r".into(),
                snapshot: Value::Dword(1),
                desired: Value::Dword(2),
            },
        )
        .unwrap();
    if let Some(outcome) = outcome {
        journal.append(5, Body::TxnEnd { txn, outcome }).unwrap();
    }
}

#[test]
fn the_ledger_tracks_commits_rollbacks_and_open_transactions() {
    let world = World::new();
    let mut journal = open(&world);

    apply_records(&mut journal, 1, Some(Outcome::RolledBack));
    let ledger = journal.ledger();
    assert!(ledger.applied.is_empty());
    assert!(ledger.resources.is_empty());
    assert!(ledger.open.is_empty());

    apply_records(&mut journal, 2, None);
    let ledger = journal.ledger();
    assert!(ledger.applied.is_empty());
    assert_eq!(ledger.open.len(), 1);
    assert_eq!(ledger.open[0].baselines.len(), 1);

    journal
        .append(
            5,
            Body::TxnEnd {
                txn: 2,
                outcome: Outcome::Committed,
            },
        )
        .unwrap();
    let ledger = journal.ledger();
    assert!(ledger.open.is_empty());
    assert!(ledger.applied.contains_key("a"));
    assert_eq!(ledger.resources["r"].epochs[0].baseline, Value::Dword(1));
    assert_eq!(journal.next_txn(), 3);

    journal
        .append(
            5,
            Body::TxnBegin {
                txn: 3,
                op: Op::Revert,
                instance: "a".into(),
                tweak_id: "a".into(),
                rev: 1,
                params: Params::default(),
                plan_hash: "h".into(),
            },
        )
        .unwrap();
    journal
        .append(
            5,
            Body::TxnEnd {
                txn: 3,
                outcome: Outcome::Committed,
            },
        )
        .unwrap();
    let ledger = journal.ledger();
    assert!(ledger.applied.is_empty());
    assert!(ledger.resources.is_empty());
}
