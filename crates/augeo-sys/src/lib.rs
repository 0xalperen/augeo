//! Windows implementations of the ports. This is the only crate allowed to use `unsafe`.
//!
//! Every other crate forbids it, so a memory-safety bug can only live here. Each `unsafe` block calls
//! one documented Windows API with arguments that are valid for the length of the call.

mod bcd;
pub mod elevation;
pub mod hardening;
mod power;
mod registry;
pub mod restore;
pub mod state_dir;
mod store;
mod util;

pub use store::FileStore;

use augeo_ports::{Address, PortError, System, Value};

/// The real system.
#[derive(Debug, Default)]
pub struct WindowsSystem;

impl WindowsSystem {
    pub fn new() -> Self {
        Self
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
            other => Err(not_yet(other)),
        }
    }

    fn is_available(&self, address: &Address) -> Result<bool, PortError> {
        Ok(matches!(
            address,
            Address::Reg { .. }
                | Address::RegKv { .. }
                | Address::PowerAc { .. }
                | Address::PowerDc { .. }
                | Address::PowerActiveScheme
                | Address::Bcd { .. }
        ))
    }
}
