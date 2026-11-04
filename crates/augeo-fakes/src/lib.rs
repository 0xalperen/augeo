//! In-memory implementations of the ports with fault injection. Used by tests only.
//!
//! A [`World`] is the machine: it outlives any one engine, the way the registry and disk outlive a
//! process. Every port call goes through a shared counter, so a test can crash or fail at any call.

#![forbid(unsafe_code)]

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use augeo_ports::{Address, Clock, JournalFile, PortError, Store, System, Value};

#[derive(Debug, Default)]
struct Faults {
    calls: Cell<u64>,
    /// From this call on, everything fails, as if the process died.
    crash_at: Cell<Option<u64>>,
    /// Only this call fails.
    fail_at: Cell<Option<u64>>,
    crash_fired: Cell<bool>,
    failure_fired: Cell<bool>,
}

impl Faults {
    fn check(&self) -> Result<(), PortError> {
        let call = self.calls.get();
        self.calls.set(call + 1);
        if self.crash_at.get().is_some_and(|at| call >= at) {
            self.crash_fired.set(true);
            return Err(PortError::Failed("simulated crash".into()));
        }
        if self.fail_at.get() == Some(call) {
            self.failure_fired.set(true);
            return Err(PortError::Failed("simulated failure".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
struct FileState {
    durable: Vec<u8>,
    pending: Vec<u8>,
}

#[derive(Debug, Default)]
struct Machine {
    values: BTreeMap<String, Value>,
    /// Writes to these addresses report success but change nothing.
    ignored: BTreeSet<String>,
    unavailable: BTreeSet<String>,
    unreadable: BTreeSet<String>,
    writes: Vec<(String, Value)>,
    /// The value each address held before its most recent write, for simulating a lost write.
    last_write_undo: Option<(String, Value)>,
    files: BTreeMap<JournalFile, FileState>,
    preserved: Vec<JournalFile>,
    locked: bool,
}

/// A simulated machine shared by the fakes.
#[derive(Debug, Clone, Default)]
pub struct World {
    machine: Rc<RefCell<Machine>>,
    faults: Rc<Faults>,
    now: Rc<Cell<u64>>,
}

impl World {
    pub fn new() -> Self {
        let world = Self::default();
        world.now.set(1_000);
        world
    }

    /// A new handle on the system, as a new process would get.
    pub fn system(&self) -> FakeSystem {
        FakeSystem {
            world: self.clone(),
        }
    }

    pub fn store(&self) -> FakeStore {
        FakeStore {
            world: self.clone(),
            holds_lock: false,
        }
    }

    pub fn clock(&self) -> FakeClock {
        FakeClock {
            world: self.clone(),
        }
    }

    /// Sets a value directly, as Windows or the user would.
    pub fn set(&self, address: &Address, value: Value) {
        self.machine
            .borrow_mut()
            .values
            .insert(address.key(), value);
    }

    pub fn get(&self, address: &Address) -> Value {
        self.machine
            .borrow()
            .values
            .get(&address.key())
            .cloned()
            .unwrap_or(Value::Absent)
    }

    /// Everything on the machine, with absent values left out, for comparing whole states.
    pub fn snapshot(&self) -> BTreeMap<String, Value> {
        self.machine
            .borrow()
            .values
            .iter()
            .filter(|(_, v)| **v != Value::Absent)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Makes writes to this address appear to succeed while changing nothing.
    pub fn ignore_writes(&self, address: &Address) {
        self.machine.borrow_mut().ignored.insert(address.key());
    }

    pub fn set_unavailable(&self, address: &Address) {
        self.machine.borrow_mut().unavailable.insert(address.key());
    }

    pub fn set_unreadable(&self, address: &Address, unreadable: bool) {
        let mut machine = self.machine.borrow_mut();
        if unreadable {
            machine.unreadable.insert(address.key());
        } else {
            machine.unreadable.remove(&address.key());
        }
    }

    /// Every system write so far, in order.
    pub fn writes(&self) -> Vec<(String, Value)> {
        self.machine.borrow().writes.clone()
    }

    pub fn preserved(&self) -> Vec<JournalFile> {
        self.machine.borrow().preserved.clone()
    }

    /// How many port calls have happened.
    pub fn calls(&self) -> u64 {
        self.faults.calls.get()
    }

    /// Everything from this many calls from now on fails, as if the process died.
    pub fn crash_after(&self, calls: u64) {
        self.faults
            .crash_at
            .set(Some(self.faults.calls.get() + calls));
    }

    /// Only the call this many calls from now fails.
    pub fn fail_after(&self, calls: u64) {
        self.faults
            .fail_at
            .set(Some(self.faults.calls.get() + calls));
    }

    /// Whether the scheduled crash has happened, meaning some call failed because of it.
    pub fn crash_fired(&self) -> bool {
        self.faults.crash_fired.get()
    }

    /// Whether the scheduled single failure has happened.
    pub fn failure_fired(&self) -> bool {
        self.faults.failure_fired.get()
    }

    pub fn clear_faults(&self) {
        self.faults.crash_fired.set(false);
        self.faults.failure_fired.set(false);
        self.faults.crash_at.set(None);
        self.faults.fail_at.set(None);
    }

    /// The process died: the lock is released and all faults clear. Flushed data stays.
    pub fn restart_process(&self) {
        self.clear_faults();
        self.machine.borrow_mut().locked = false;
    }

    /// Power was cut. Unflushed journal bytes vanish, except `keep_pending` bytes of them, which
    /// tears the last record.
    pub fn power_loss(&self, keep_pending: usize) {
        self.restart_process();
        let mut machine = self.machine.borrow_mut();
        for state in machine.files.values_mut() {
            let keep = keep_pending.min(state.pending.len());
            let kept: Vec<u8> = state.pending.drain(..keep).collect();
            state.durable.extend(kept);
            state.pending.clear();
        }
    }

    /// The most recent system write was lost even though it reported success.
    pub fn lose_last_write(&self) {
        let mut machine = self.machine.borrow_mut();
        if let Some((key, old)) = machine.last_write_undo.take() {
            machine.values.insert(key, old);
        }
    }

    pub fn journal_bytes(&self, file: JournalFile) -> Vec<u8> {
        let machine = self.machine.borrow();
        machine
            .files
            .get(&file)
            .map(|s| [s.durable.as_slice(), s.pending.as_slice()].concat())
            .unwrap_or_default()
    }

    /// Replaces a journal file's contents, to simulate damage.
    pub fn set_journal_bytes(&self, file: JournalFile, bytes: Vec<u8>) {
        self.machine.borrow_mut().files.insert(
            file,
            FileState {
                durable: bytes,
                pending: Vec::new(),
            },
        );
    }

    pub fn advance_clock(&self, ms: u64) {
        self.now.set(self.now.get() + ms);
    }
}

#[derive(Debug)]
pub struct FakeSystem {
    world: World,
}

impl System for FakeSystem {
    fn read(&self, address: &Address) -> Result<Value, PortError> {
        self.world.faults.check()?;
        if self
            .world
            .machine
            .borrow()
            .unreadable
            .contains(&address.key())
        {
            return Err(PortError::Denied("simulated unreadable value".into()));
        }
        Ok(self.world.get(address))
    }

    fn write(&mut self, address: &Address, value: &Value) -> Result<(), PortError> {
        self.world.faults.check()?;
        let key = address.key();
        let mut machine = self.world.machine.borrow_mut();
        machine.writes.push((key.clone(), value.clone()));
        if machine.ignored.contains(&key) {
            return Ok(());
        }
        let old = machine.values.get(&key).cloned().unwrap_or(Value::Absent);
        machine.last_write_undo = Some((key.clone(), old));
        machine.values.insert(key, value.clone());
        Ok(())
    }

    fn is_available(&self, address: &Address) -> Result<bool, PortError> {
        self.world.faults.check()?;
        Ok(!self
            .world
            .machine
            .borrow()
            .unavailable
            .contains(&address.key()))
    }
}

#[derive(Debug)]
pub struct FakeStore {
    world: World,
    holds_lock: bool,
}

impl Store for FakeStore {
    fn try_lock(&mut self) -> Result<bool, PortError> {
        self.world.faults.check()?;
        let mut machine = self.world.machine.borrow_mut();
        if machine.locked {
            return Ok(self.holds_lock);
        }
        machine.locked = true;
        self.holds_lock = true;
        Ok(true)
    }

    fn load(&self, file: JournalFile) -> Result<Vec<u8>, PortError> {
        self.world.faults.check()?;
        Ok(self.world.journal_bytes(file))
    }

    fn append(&mut self, file: JournalFile, data: &[u8]) -> Result<(), PortError> {
        self.world.faults.check()?;
        let mut machine = self.world.machine.borrow_mut();
        machine
            .files
            .entry(file)
            .or_default()
            .pending
            .extend_from_slice(data);
        Ok(())
    }

    fn flush(&mut self, file: JournalFile) -> Result<(), PortError> {
        self.world.faults.check()?;
        let mut machine = self.world.machine.borrow_mut();
        let state = machine.files.entry(file).or_default();
        let pending = std::mem::take(&mut state.pending);
        state.durable.extend(pending);
        Ok(())
    }

    fn truncate(&mut self, file: JournalFile, len: u64) -> Result<(), PortError> {
        self.world.faults.check()?;
        let mut machine = self.world.machine.borrow_mut();
        let state = machine.files.entry(file).or_default();
        state.pending.clear();
        state.durable.truncate(len as usize);
        Ok(())
    }

    fn preserve(&mut self, file: JournalFile) -> Result<(), PortError> {
        self.world.faults.check()?;
        self.world.machine.borrow_mut().preserved.push(file);
        Ok(())
    }
}

#[derive(Debug)]
pub struct FakeClock {
    world: World,
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.world.now.get()
    }
}
