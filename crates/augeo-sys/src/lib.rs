//! Windows implementations of the ports. This is the only crate allowed to use `unsafe`.
//!
//! Every other crate forbids it, so a memory-safety bug can only live here. Each `unsafe` block calls
//! one documented API with arguments that are valid for the length of the call.

mod bcd;
pub mod elevation;
pub mod hardening;
mod nvapi;
pub mod nvml;
mod power;
mod registry;
pub mod restore;
pub mod state_dir;
mod store;
mod util;

use std::path::PathBuf;

pub use store::FileStore;

use augeo_ports::{Address, PortError, System, Value};
use nvapi::Drs;

/// The real system.
#[derive(Debug)]
pub struct WindowsSystem {
    drs: Option<Drs>,
}

impl Default for WindowsSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsSystem {
    /// With no backup location, for reading.
    pub fn new() -> Self {
        Self {
            drs: Drs::load(None),
        }
    }

    /// `drs_backup` is where the NVIDIA settings database is exported before the first change.
    pub fn with_drs_backup(drs_backup: PathBuf) -> Self {
        Self {
            drs: Drs::load(Some(drs_backup)),
        }
    }

    /// Whether an NVIDIA driver answered.
    pub fn has_nvidia_driver(&self) -> bool {
        self.drs.is_some()
    }

    fn drs(&self) -> Result<&Drs, PortError> {
        self.drs
            .as_ref()
            .ok_or_else(|| PortError::NotAvailable("no NVIDIA driver was found".into()))
    }
}

fn not_yet(address: &Address) -> PortError {
    PortError::NotAvailable(format!("{address} isn't supported in this build"))
}

impl System for WindowsSystem {
    fn read(&self, address: &Address) -> Result<Value, PortError> {
        match address {
            Address::Reg { root, path, name } => registry::read(root, path, name),
            Address::RegKv {
                root,
                path,
                name,
                key,
            } => registry::read_kv(root, path, name, key),
            Address::PowerAc {
                scheme,
                subgroup,
                setting,
            } => power::read_index(power::Line::Ac, scheme, subgroup, setting),
            Address::PowerDc {
                scheme,
                subgroup,
                setting,
            } => power::read_index(power::Line::Dc, scheme, subgroup, setting),
            Address::PowerActiveScheme => power::active_scheme().map(Value::Text),
            Address::Bcd { element } => bcd::read(element),
            Address::Drs { profile, setting } => self.drs()?.read(profile, *setting),
            other => Err(not_yet(other)),
        }
    }

    fn write(&mut self, address: &Address, value: &Value) -> Result<(), PortError> {
        match address {
            Address::Reg { root, path, name } => registry::write(root, path, name, value),
            Address::RegKv {
                root,
                path,
                name,
                key,
            } => registry::write_kv(root, path, name, key, value),
            Address::PowerAc {
                scheme,
                subgroup,
                setting,
            } => power::write_index(power::Line::Ac, scheme, subgroup, setting, value),
            Address::PowerDc {
                scheme,
                subgroup,
                setting,
            } => power::write_index(power::Line::Dc, scheme, subgroup, setting, value),
            Address::PowerActiveScheme => power::set_active_scheme(value),
            Address::Bcd { element } => bcd::write(element, value),
            Address::Drs { profile, setting } => {
                let drs = self
                    .drs
                    .as_mut()
                    .ok_or_else(|| PortError::NotAvailable("no NVIDIA driver was found".into()))?;
                drs.write(profile, *setting, value)
            }
            other => Err(not_yet(other)),
        }
    }

    fn is_available(&self, address: &Address) -> Result<bool, PortError> {
        match address {
            Address::Reg { .. }
            | Address::RegKv { .. }
            | Address::PowerAc { .. }
            | Address::PowerDc { .. }
            | Address::PowerActiveScheme
            | Address::Bcd { .. } => Ok(true),
            // Asks the driver itself, because community-known IDs can vanish in any driver.
            Address::Drs { setting, .. } => match &self.drs {
                Some(drs) => drs.is_available(*setting),
                None => Ok(false),
            },
            _ => Ok(false),
        }
    }
}
