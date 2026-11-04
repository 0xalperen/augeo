//! The append-only journal: the durable record of every change augeo makes and the value it replaced.
//!
//! Each record is framed as `[u32 length][u32 CRC-32][JSON]` and flushed to a primary file, then to a
//! mirror, before the engine goes on. Files only grow, apart from cutting off damage on open. There is
//! no compaction and there are no renames, which avoids the failure modes of replacing files.
//!
//! The journal is not a security boundary: the directory's permissions are. It also never decides
//! what may be written, because locations come only from the catalog.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use augeo_ports::{JournalFile, PortError, Store, Value};
use serde::{Deserialize, Serialize};

mod ledger;
pub use ledger::{Applied, Epoch, Ledger, OpenTxn, ResourceEntry, Step};

/// The record format this build understands.
pub const SCHEMA: u32 = 1;
const HEADER_LEN: usize = 8;
const MAX_RECORD_LEN: usize = 1 << 20;

/// What a transaction does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Apply,
    Revert,
}

/// How a transaction ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Committed,
    RolledBack,
    /// Rolling back failed. The baseline is kept so that undo can retry.
    RollbackFailed,
    /// Nothing was written.
    Aborted,
    /// A revert stopped partway. It is safe to run again.
    RevertFailed,
}

/// How the user resolved an interrupted transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    RollBack,
    Keep,
    FinishRevert,
}

/// The inputs that identify a tweak instance, needed to rebuild its addresses later for undo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Params {
    pub option: Option<String>,
    pub exe: Option<String>,
    pub user_sid: String,
    pub power_scheme: Option<String>,
    pub adapters: Vec<String>,
    pub display_device: Option<String>,
    /// A value worked out when the plan was made, such as a frame cap or a display mode.
    pub computed: Option<Value>,
}

/// The body of a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Body {
    TxnBegin {
        txn: u64,
        op: Op,
        instance: String,
        tweak_id: String,
        rev: u32,
        params: Params,
        plan_hash: String,
    },
    /// The value before any change. Durable before the first write of the transaction.
    Baseline {
        txn: u64,
        res_key: String,
        snapshot: Value,
        desired: Value,
    },
    /// About to write. Durable before the write happens.
    Intent {
        txn: u64,
        step: u32,
        res_key: String,
        value: Value,
    },
    /// The write finished, and what a read afterwards saw.
    Done {
        txn: u64,
        step: u32,
        observed: Option<Value>,
        error: Option<String>,
    },
    TxnEnd {
        txn: u64,
        outcome: Outcome,
    },
    Recovery {
        txn: u64,
        decision: Decision,
    },
    /// The user accepted this exact disclaimer text.
    Accept {
        text_sha256: String,
        version: u32,
        sid: String,
    },
}

/// One journal record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub seq: u64,
    pub ts_ms: u64,
    pub schema: u32,
    pub boot_id: String,
    pub os_build: u32,
    pub driver: Option<String>,
    pub app: String,
    pub body: Body,
}

/// Facts stamped on every record, used later to explain drift ("driver A to B since apply").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stamp {
    pub boot_id: String,
    pub os_build: u32,
    pub driver: Option<String>,
    pub app: String,
}

fn frame(record: &Record) -> Result<Vec<u8>, JournalError> {
    let json = serde_json::to_vec(record).map_err(|e| JournalError::Encode(e.to_string()))?;
    let mut out = Vec::with_capacity(HEADER_LEN + json.len());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&json).to_le_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

enum FrameAt {
    Good(Box<Record>, usize),
    NewerSchema,
    Bad,
}

fn frame_at(bytes: &[u8], pos: usize) -> FrameAt {
    let Some(header) = bytes.get(pos..pos + HEADER_LEN) else {
        return FrameAt::Bad;
    };
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let crc = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if len == 0 || len > MAX_RECORD_LEN {
        return FrameAt::Bad;
    }
    let end = pos + HEADER_LEN + len;
    let Some(json) = bytes.get(pos + HEADER_LEN..end) else {
        return FrameAt::Bad;
    };
    if crc32fast::hash(json) != crc {
        return FrameAt::Bad;
    }
    match serde_json::from_slice::<Record>(json) {
        Ok(record) if record.schema <= SCHEMA => FrameAt::Good(Box::new(record), end),
        // The checksum is right but this build can't read it, so a newer version wrote it.
        _ => FrameAt::NewerSchema,
    }
}

#[derive(Debug, Default)]
struct Scan {
    frames: Vec<(Record, Vec<u8>)>,
    /// Length of the run of good frames from the start.
    valid_prefix: usize,
    /// Bad bytes with good frames after them, rather than only a torn tail.
    corrupt_middle: bool,
    newer_schema: bool,
}

fn scan(bytes: &[u8]) -> Scan {
    let mut result = Scan::default();
    let mut pos = 0;
    let mut contiguous = true;
    while pos < bytes.len() {
        match frame_at(bytes, pos) {
            FrameAt::Good(record, end) => {
                result.frames.push((*record, bytes[pos..end].to_vec()));
                if contiguous {
                    result.valid_prefix = end;
                }
                pos = end;
            }
            FrameAt::NewerSchema => {
                result.newer_schema = true;
                break;
            }
            FrameAt::Bad => {
                contiguous = false;
                // Look for a good frame further on. Finding one means this is damage in the middle.
                let next = (pos + 1..bytes.len()).find(|&i| {
                    matches!(frame_at(bytes, i), FrameAt::Good(..) | FrameAt::NewerSchema)
                });
                match next {
                    Some(i) => {
                        result.corrupt_middle = true;
                        pos = i;
                    }
                    None => break,
                }
            }
        }
    }
    result
}

/// What opening the journal found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenReport {
    /// A newer version wrote this journal, so this build must only read it.
    pub newer_schema: bool,
    /// Records are missing from both copies, so some history is gone.
    pub history_lost: bool,
    /// One copy was repaired from the other, or a torn final record was cut off.
    pub repaired: bool,
    /// A damaged copy was preserved for investigation.
    pub preserved: bool,
    /// Another instance holds the writer lock.
    pub locked_elsewhere: bool,
}

/// A journal failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalError {
    Port(PortError),
    Encode(String),
    ReadOnly,
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalError::Port(e) => write!(f, "journal storage failed: {e}"),
            JournalError::Encode(e) => write!(f, "journal encoding failed: {e}"),
            JournalError::ReadOnly => f.write_str("this instance can't write to the journal"),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<PortError> for JournalError {
    fn from(e: PortError) -> Self {
        JournalError::Port(e)
    }
}

/// An open journal.
#[derive(Debug)]
pub struct Journal<S> {
    store: S,
    records: Vec<Record>,
    next_seq: u64,
    writer: bool,
    report: OpenReport,
    stamp: Stamp,
}

impl<S: Store> Journal<S> {
    /// Opens for writing if possible. Damage is repaired only when this instance holds the lock.
    /// With `want_writer` false it never changes anything on disk.
    pub fn open(mut store: S, want_writer: bool, stamp: Stamp) -> Result<Self, JournalError> {
        let mut report = OpenReport::default();
        let mut writer = false;
        if want_writer {
            writer = store.try_lock()?;
            report.locked_elsewhere = !writer;
        }

        let primary = store.load(JournalFile::Primary)?;
        let mirror = store.load(JournalFile::Mirror)?;
        let scan_primary = scan(&primary);
        let scan_mirror = scan(&mirror);
        report.newer_schema = scan_primary.newer_schema || scan_mirror.newer_schema;
        if report.newer_schema {
            writer = false;
        }

        // Every good frame from either copy, in sequence order.
        let mut union: BTreeMap<u64, (Record, Vec<u8>)> = BTreeMap::new();
        for (record, bytes) in scan_primary.frames.iter().chain(&scan_mirror.frames) {
            union
                .entry(record.seq)
                .or_insert_with(|| (record.clone(), bytes.clone()));
        }
        let expected: Vec<u8> = union
            .values()
            .flat_map(|(_, b)| b.iter().copied())
            .collect();
        let max_seq = union.keys().next_back().copied().unwrap_or(0);
        let present: BTreeSet<u64> = union.keys().copied().collect();
        report.history_lost = (1..=max_seq).any(|s| !present.contains(&s));

        if writer {
            for (file, bytes, scanned) in [
                (JournalFile::Primary, &primary, &scan_primary),
                (JournalFile::Mirror, &mirror, &scan_mirror),
            ] {
                if *bytes == expected {
                    continue;
                }
                report.repaired = true;
                let keep = scanned.valid_prefix;
                let simple = !scanned.corrupt_middle && expected.starts_with(&bytes[..keep]);
                let keep = if simple {
                    keep
                } else {
                    store.preserve(file)?;
                    report.preserved = true;
                    0
                };
                store.truncate(file, keep as u64)?;
                store.append(file, &expected[keep..])?;
                store.flush(file)?;
            }
        }

        Ok(Self {
            store,
            records: union.into_values().map(|(r, _)| r).collect(),
            next_seq: max_seq + 1,
            writer,
            report,
            stamp,
        })
    }

    pub fn is_writer(&self) -> bool {
        self.writer
    }

    pub fn report(&self) -> &OpenReport {
        &self.report
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Folds the records into current ownership and baselines.
    pub fn ledger(&self) -> Ledger {
        Ledger::fold(&self.records)
    }

    /// The next free transaction number.
    pub fn next_txn(&self) -> u64 {
        self.records
            .iter()
            .filter_map(|r| match r.body {
                Body::TxnBegin { txn, .. } => Some(txn),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Appends a record. When this returns, the record is on disk in both copies.
    pub fn append(&mut self, ts_ms: u64, body: Body) -> Result<(), JournalError> {
        if !self.writer {
            return Err(JournalError::ReadOnly);
        }
        let record = Record {
            seq: self.next_seq,
            ts_ms,
            schema: SCHEMA,
            boot_id: self.stamp.boot_id.clone(),
            os_build: self.stamp.os_build,
            driver: self.stamp.driver.clone(),
            app: self.stamp.app.clone(),
            body,
        };
        let bytes = frame(&record)?;
        for file in [JournalFile::Primary, JournalFile::Mirror] {
            self.store.append(file, &bytes)?;
            self.store.flush(file)?;
        }
        self.next_seq += 1;
        self.records.push(record);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
