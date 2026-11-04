//! Folds journal records into what augeo currently owns and the values it must restore.
//!
//! The ledger only says what augeo expects. Whether a tweak is really applied is decided by reading
//! the system.

use std::collections::BTreeMap;

use augeo_ports::Value;

use crate::{Body, Op, Outcome, Params, Record};

/// One apply of a resource: the value it replaced and the value it set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Epoch {
    pub baseline: Value,
    pub desired: Value,
    pub txn: u64,
}

/// A resource augeo has changed and not yet released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceEntry {
    pub owner: String,
    /// Oldest first. Re-applying after an external change adds an epoch.
    pub epochs: Vec<Epoch>,
}

/// A tweak instance augeo has applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub instance: String,
    pub tweak_id: String,
    pub rev: u32,
    pub params: Params,
    pub txn: u64,
    /// When it committed, for undoing in reverse order.
    pub commit_seq: u64,
    pub boot_id: String,
    pub os_build: u32,
    pub driver: Option<String>,
    /// Rolling back failed, so the system may be partly changed.
    pub rollback_failed: bool,
    /// A revert stopped partway.
    pub revert_failed: bool,
}

/// One write recorded in a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub res_key: String,
    pub value: Value,
    pub finished: bool,
}

/// A transaction with no end record: the process died during it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTxn {
    pub txn: u64,
    pub op: Op,
    pub instance: String,
    pub tweak_id: String,
    pub rev: u32,
    pub params: Params,
    /// `(res_key, baseline, desired)` for each resource.
    pub baselines: Vec<(String, Value, Value)>,
    pub steps: Vec<Step>,
}

/// The state folded from the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    pub applied: BTreeMap<String, Applied>,
    pub resources: BTreeMap<String, ResourceEntry>,
    pub open: Vec<OpenTxn>,
    /// `(text_sha256, sid)` of accepted disclaimers.
    pub accepted: Vec<(String, String)>,
}

impl Ledger {
    pub fn fold(records: &[Record]) -> Self {
        let mut ledger = Ledger::default();
        let mut open: BTreeMap<u64, OpenTxn> = BTreeMap::new();

        for record in records {
            match &record.body {
                Body::TxnBegin {
                    txn,
                    op,
                    instance,
                    tweak_id,
                    rev,
                    params,
                    ..
                } => {
                    open.insert(
                        *txn,
                        OpenTxn {
                            txn: *txn,
                            op: *op,
                            instance: instance.clone(),
                            tweak_id: tweak_id.clone(),
                            rev: *rev,
                            params: params.clone(),
                            baselines: Vec::new(),
                            steps: Vec::new(),
                        },
                    );
                }
                Body::Baseline {
                    txn,
                    res_key,
                    snapshot,
                    desired,
                } => {
                    if let Some(t) = open.get_mut(txn) {
                        t.baselines
                            .push((res_key.clone(), snapshot.clone(), desired.clone()));
                    }
                }
                Body::Intent {
                    txn,
                    res_key,
                    value,
                    ..
                } => {
                    if let Some(t) = open.get_mut(txn) {
                        t.steps.push(Step {
                            res_key: res_key.clone(),
                            value: value.clone(),
                            finished: false,
                        });
                    }
                }
                Body::Done { txn, step, .. } => {
                    if let Some(s) = open
                        .get_mut(txn)
                        .and_then(|t| t.steps.get_mut(*step as usize))
                    {
                        s.finished = true;
                    }
                }
                Body::TxnEnd { txn, outcome } => {
                    if let Some(t) = open.remove(txn) {
                        ledger.finish(t, *outcome, record);
                    }
                }
                Body::Recovery { .. } => {}
                Body::Accept {
                    text_sha256, sid, ..
                } => ledger.accepted.push((text_sha256.clone(), sid.clone())),
            }
        }
        ledger.open = open.into_values().collect();
        ledger
    }

    fn finish(&mut self, t: OpenTxn, outcome: Outcome, end: &Record) {
        match (t.op, outcome) {
            (Op::Apply, Outcome::Committed | Outcome::RollbackFailed) => {
                let rollback_failed = outcome == Outcome::RollbackFailed;
                for (res_key, baseline, desired) in t.baselines {
                    let entry = self
                        .resources
                        .entry(res_key)
                        .or_insert_with(|| ResourceEntry {
                            owner: t.instance.clone(),
                            epochs: Vec::new(),
                        });
                    entry.epochs.push(Epoch {
                        baseline,
                        desired,
                        txn: t.txn,
                    });
                }
                self.applied.insert(
                    t.instance.clone(),
                    Applied {
                        instance: t.instance,
                        tweak_id: t.tweak_id,
                        rev: t.rev,
                        params: t.params,
                        txn: t.txn,
                        commit_seq: end.seq,
                        boot_id: end.boot_id.clone(),
                        os_build: end.os_build,
                        driver: end.driver.clone(),
                        rollback_failed,
                        revert_failed: false,
                    },
                );
            }
            (Op::Revert, Outcome::Committed) => {
                self.applied.remove(&t.instance);
                self.resources.retain(|_, r| r.owner != t.instance);
            }
            (Op::Revert, Outcome::RevertFailed) => {
                if let Some(applied) = self.applied.get_mut(&t.instance) {
                    applied.revert_failed = true;
                }
            }
            // A rollback or abort means the transaction left nothing behind.
            _ => {}
        }
    }
}
