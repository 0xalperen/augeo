//! Turns a catalog resource plus an instance's parameters into concrete addresses and values.
//!
//! Every write goes through here, and locations come only from the embedded catalog. The journal can
//! supply parameters and old values but never a location, so tampering with it can't redirect a write.
//! The denylists are checked again here, in case a bad entry ever got past the catalog lint.

use augeo_catalog::{
    lint::{ALLOWED_BCD_ELEMENTS, is_denied_registry},
    schema::{Desired, RegType, Resource, Revision, Scope},
};
use augeo_command::is_valid_exe;
use augeo_journal::Params;
use augeo_ports::{Address, DrsProfile, RegRoot, Value};

/// One concrete thing a tweak should change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) address: Address,
    pub(crate) desired: Value,
    /// A missing value also counts as satisfied.
    pub(crate) accept_absent: bool,
}

impl Target {
    pub(crate) fn satisfied_by(&self, live: &Value) -> bool {
        *live == self.desired || (self.accept_absent && *live == Value::Absent)
    }
}

fn is_valid_sid(sid: &str) -> bool {
    sid.starts_with("S-1-")
        && sid.len() <= 184
        && sid
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == 'S')
}

fn is_valid_guid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn root(scope: Scope, params: &Params) -> Result<RegRoot, String> {
    match scope {
        Scope::Machine => Ok(RegRoot::Machine),
        Scope::User if is_valid_sid(&params.user_sid) => Ok(RegRoot::User {
            sid: params.user_sid.clone(),
        }),
        Scope::User => Err(format!("invalid user SID '{}'", params.user_sid)),
    }
}

fn scheme(params: &Params) -> Result<String, String> {
    match &params.power_scheme {
        Some(s) if is_valid_guid(s) => Ok(s.to_lowercase()),
        other => Err(format!("invalid power plan {other:?}")),
    }
}

/// How a catalog value is stored for a kind of resource.
#[derive(Clone, Copy)]
enum Shape {
    Reg(RegType),
    Text,
    Dword,
}

fn to_value(desired: &Desired, shape: Shape, params: &Params) -> Result<Value, String> {
    match desired {
        Desired::Absent { absent: true } => Ok(Value::Absent),
        Desired::Absent { absent: false } => Err("'absent = false' is meaningless".to_string()),
        Desired::ByOption { by_option } => {
            let key = params
                .option
                .as_deref()
                .ok_or("this tweak needs an option")?;
            let chosen = by_option
                .get(key)
                .ok_or_else(|| format!("unknown option '{key}'"))?;
            to_value(chosen, shape, params)
        }
        Desired::Computed { .. } => params
            .computed
            .clone()
            .ok_or_else(|| "the value for this tweak couldn't be worked out".to_string()),
        Desired::Int(n) => match shape {
            Shape::Reg(RegType::Qword) => u64::try_from(*n)
                .map(Value::Qword)
                .map_err(|_| format!("{n} doesn't fit")),
            Shape::Reg(RegType::Dword) | Shape::Dword => u32::try_from(*n)
                .map(Value::Dword)
                .map_err(|_| format!("{n} doesn't fit")),
            Shape::Text => Ok(Value::Text(n.to_string())),
            Shape::Reg(_) => Err(format!("{n} is a number, but the value is text")),
        },
        Desired::Text(s) => match shape {
            Shape::Reg(RegType::Sz) | Shape::Text => Ok(Value::Text(s.clone())),
            _ => Err(format!("'{s}' is text, but the value is a number")),
        },
        Desired::List(items) => match shape {
            Shape::Reg(RegType::MultiSz) => Ok(Value::Multi(items.clone())),
            _ => Err("a list only fits a multi-string value".to_string()),
        },
    }
}

/// Builds the concrete targets for a revision. A network tweak yields one target per adapter.
pub(crate) fn expand(rev: &Revision, params: &Params) -> Result<Vec<Target>, String> {
    if let Some(exe) = &params.exe
        && !is_valid_exe(exe)
    {
        return Err(format!("invalid game name '{exe}'"));
    }
    let mut targets = Vec::new();
    for resource in &rev.resource {
        match resource {
            Resource::RegValue {
                scope,
                path,
                name,
                value_type,
                desired,
                accept_absent,
            } => {
                if is_denied_registry(path, name) {
                    return Err(format!("registry location {path}\\{name} is forbidden"));
                }
                targets.push(Target {
                    address: Address::Reg {
                        root: root(*scope, params)?,
                        path: path.clone(),
                        name: name.clone(),
                    },
                    desired: to_value(desired, Shape::Reg(*value_type), params)?,
                    accept_absent: *accept_absent,
                });
            }
            Resource::RegKvString {
                scope,
                path,
                name,
                key,
                desired,
            } => {
                if is_denied_registry(path, name) {
                    return Err(format!("registry location {path}\\{name} is forbidden"));
                }
                targets.push(Target {
                    address: Address::RegKv {
                        root: root(*scope, params)?,
                        path: path.clone(),
                        name: name.clone(),
                        key: key.clone(),
                    },
                    desired: to_value(desired, Shape::Text, params)?,
                    accept_absent: false,
                });
            }
            Resource::PowerIndexAc {
                subgroup,
                setting,
                desired,
            } => targets.push(Target {
                address: Address::PowerAc {
                    scheme: scheme(params)?,
                    subgroup: subgroup.to_lowercase(),
                    setting: setting.to_lowercase(),
                },
                desired: to_value(desired, Shape::Dword, params)?,
                accept_absent: false,
            }),
            Resource::PowerIndexDc {
                subgroup,
                setting,
                desired,
            } => targets.push(Target {
                address: Address::PowerDc {
                    scheme: scheme(params)?,
                    subgroup: subgroup.to_lowercase(),
                    setting: setting.to_lowercase(),
                },
                desired: to_value(desired, Shape::Dword, params)?,
                accept_absent: false,
            }),
            Resource::PowerActiveScheme { scheme } => targets.push(Target {
                address: Address::PowerActiveScheme,
                desired: Value::Text(scheme.to_lowercase()),
                accept_absent: false,
            }),
            Resource::BcdElement { element } => {
                if !ALLOWED_BCD_ELEMENTS.contains(&element.as_str()) {
                    return Err(format!("boot setting '{element}' is forbidden"));
                }
                targets.push(Target {
                    address: Address::Bcd {
                        element: element.clone(),
                    },
                    desired: Value::Absent,
                    accept_absent: true,
                });
            }
            Resource::DrsSetting {
                profile,
                setting,
                desired,
                ..
            } => {
                let profile = if profile == "base" {
                    DrsProfile::Base
                } else {
                    DrsProfile::Game {
                        exe: params
                            .exe
                            .clone()
                            .ok_or("this tweak needs a game")?
                            .to_lowercase(),
                    }
                };
                targets.push(Target {
                    address: Address::Drs {
                        profile,
                        setting: *setting,
                    },
                    desired: to_value(desired, Shape::Dword, params)?,
                    accept_absent: false,
                });
            }
            Resource::NicProperty { keyword, desired } => {
                let value = to_value(desired, Shape::Text, params)?;
                for adapter in &params.adapters {
                    targets.push(Target {
                        address: Address::Nic {
                            adapter: adapter.clone(),
                            keyword: keyword.clone(),
                        },
                        desired: value.clone(),
                        accept_absent: false,
                    });
                }
            }
            Resource::DisplayMode { .. } => targets.push(Target {
                address: Address::DisplayMode {
                    device: params
                        .display_device
                        .clone()
                        .ok_or("no display was found")?,
                },
                desired: params
                    .computed
                    .clone()
                    .ok_or("the display's best mode couldn't be worked out")?,
                accept_absent: false,
            }),
        }
    }
    Ok(targets)
}
