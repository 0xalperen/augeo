//! The interfaces between the engine and the operating system.
//!
//! The engine only sees addresses and values. `augeo-sys` implements these against Windows, and
//! `augeo-fakes` implements them in memory with fault injection, so every failure path can be tested.

#![forbid(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Serialize};

/// A value as it exists on the system, including "does not exist".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Absent,
    Dword(u32),
    Qword(u64),
    Text(String),
    Multi(Vec<String>),
    /// A registry value of any other type, kept byte for byte so it can be restored exactly.
    Raw {
        ty: u32,
        data: Vec<u8>,
    },
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Absent => f.write_str("(does not exist)"),
            Value::Dword(v) => write!(f, "{v} (0x{v:08X})"),
            Value::Qword(v) => write!(f, "{v} (0x{v:016X})"),
            Value::Text(v) => write!(f, "\"{v}\""),
            Value::Multi(v) => write!(f, "{v:?}"),
            Value::Raw { ty, data } => write!(f, "type {ty}, {} bytes", data.len()),
        }
    }
}

/// Which registry hive a value lives in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegRoot {
    /// `HKEY_LOCAL_MACHINE`.
    Machine,
    /// `HKEY_USERS\<sid>`: the interactive user, even when elevated as a different account.
    User { sid: String },
}

/// Which NVIDIA driver profile a setting lives in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrsProfile {
    Base,
    Game { exe: String },
}

/// One thing on the system that a tweak can read or change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Address {
    Reg {
        root: RegRoot,
        path: String,
        name: String,
    },
    /// One `key=value;` entry inside a registry string shared with other keys.
    RegKv {
        root: RegRoot,
        path: String,
        name: String,
        key: String,
    },
    PowerAc {
        scheme: String,
        subgroup: String,
        setting: String,
    },
    PowerDc {
        scheme: String,
        subgroup: String,
        setting: String,
    },
    PowerActiveScheme,
    Bcd {
        element: String,
    },
    Drs {
        profile: DrsProfile,
        setting: u32,
    },
    Nic {
        adapter: String,
        keyword: String,
    },
    DisplayMode {
        device: String,
    },
}

impl Address {
    /// A stable key for the journal. Windows paths are case-insensitive, so it is lowercase.
    pub fn key(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| format!("{self:?}"))
            .to_lowercase()
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let root = |r: &RegRoot| match r {
            RegRoot::Machine => "HKLM".to_string(),
            RegRoot::User { sid } => format!("HKU\\{sid}"),
        };
        match self {
            Address::Reg {
                root: r,
                path,
                name,
            } => write!(f, "{}\\{path} : {name}", root(r)),
            Address::RegKv {
                root: r,
                path,
                name,
                key,
            } => write!(f, "{}\\{path} : {name} : {key}", root(r)),
            Address::PowerAc {
                scheme,
                subgroup,
                setting,
            } => write!(f, "power plan {scheme}, plugged in: {subgroup} / {setting}"),
            Address::PowerDc {
                scheme,
                subgroup,
                setting,
            } => write!(f, "power plan {scheme}, on battery: {subgroup} / {setting}"),
            Address::PowerActiveScheme => f.write_str("active power plan"),
            Address::Bcd { element } => write!(f, "boot setting {element}"),
            Address::Drs { profile, setting } => match profile {
                DrsProfile::Base => write!(f, "NVIDIA global profile: 0x{setting:08X}"),
                DrsProfile::Game { exe } => write!(f, "NVIDIA profile for {exe}: 0x{setting:08X}"),
            },
            Address::Nic { adapter, keyword } => write!(f, "network adapter {adapter}: {keyword}"),
            Address::DisplayMode { device } => write!(f, "display mode of {device}"),
        }
    }
}

/// Why an operating system call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    /// Refused by permissions.
    Denied(String),
    /// Doesn't exist on this system, such as an NVIDIA setting the driver doesn't list.
    NotAvailable(String),
    Failed(String),
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortError::Denied(m) => write!(f, "access denied: {m}"),
            PortError::NotAvailable(m) => write!(f, "not available: {m}"),
            PortError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for PortError {}

/// Reading and changing the system.
pub trait System {
    /// Reads a live value. A missing value is `Value::Absent`, not an error.
    fn read(&self, address: &Address) -> Result<Value, PortError>;

    /// Writes a value. `Value::Absent` deletes it.
    ///
    /// When this returns, the write must be durable (for the registry, flushed with `RegFlushKey`).
    /// The engine relies on that: once an undo commits it stops tracking the value, so a write lost
    /// after that point could never be detected. A write lost before returning is safe, because the
    /// journal still shows the operation as unfinished and recovery measures everything.
    fn write(&mut self, address: &Address, value: &Value) -> Result<(), PortError>;

    /// Whether this address exists here, such as whether the driver lists an NVIDIA setting.
    fn is_available(&self, address: &Address) -> Result<bool, PortError>;
}

/// The two copies of the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JournalFile {
    Primary,
    Mirror,
}

/// Durable storage for the journal.
pub trait Store {
    /// Takes the exclusive writer lock. `Ok(false)` means another instance holds it.
    fn try_lock(&mut self) -> Result<bool, PortError>;
    fn load(&self, file: JournalFile) -> Result<Vec<u8>, PortError>;
    fn append(&mut self, file: JournalFile, data: &[u8]) -> Result<(), PortError>;
    /// Returns only when everything appended so far is on disk.
    fn flush(&mut self, file: JournalFile) -> Result<(), PortError>;
    fn truncate(&mut self, file: JournalFile, len: u64) -> Result<(), PortError>;
    /// Keeps a copy of a damaged file for investigation. It is never deleted automatically.
    fn preserve(&mut self, file: JournalFile) -> Result<(), PortError>;
}

/// The current time.
pub trait Clock {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}
