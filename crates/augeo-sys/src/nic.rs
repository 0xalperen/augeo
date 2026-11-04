//! Network adapter properties. These are the registry values behind the "Advanced" tab of an adapter
//! in Device Manager. An adapter must restart to pick up a change, which drops the network briefly.

use augeo_ports::{PortError, RegRoot, Value};
use windows::{
    Win32::Devices::DeviceAndDriverInstallation::{
        CM_Disable_DevNode, CM_Enable_DevNode, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW,
        CR_SUCCESS,
    },
    core::HSTRING,
};

use crate::registry;

const CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4D36E972-E325-11CE-BFC1-08002bE10318}";
const NCF_PHYSICAL: u32 = 0x4;
/// The `*PhysicalMediaType` value for wired Ethernet.
const MEDIA_802_3: u32 = 14;

/// Drivers store this either as a number or as text, so both count.
fn is_wired_media(value: &Value) -> bool {
    match value {
        Value::Dword(media) => *media == MEDIA_802_3,
        Value::Text(media) => media.trim().parse() == Ok(MEDIA_802_3),
        _ => false,
    }
}

fn adapter_path(adapter: &str) -> Result<String, PortError> {
    // A class subkey is always four digits. This keeps a tampered value from reaching another key.
    if adapter.len() == 4 && adapter.chars().all(|c| c.is_ascii_digit()) {
        Ok(format!(r"{CLASS}\{adapter}"))
    } else {
        Err(PortError::Failed(format!("invalid adapter '{adapter}'")))
    }
}

/// Wired physical adapters, as their class subkey numbers. Wi-Fi and virtual adapters are left out.
pub fn wired_adapters() -> Vec<String> {
    let mut found = Vec::new();
    for index in 0..64 {
        let id = format!("{index:04}");
        let Ok(path) = adapter_path(&id) else {
            continue;
        };
        let Ok(Value::Dword(characteristics)) =
            registry::read(&RegRoot::Machine, &path, "Characteristics")
        else {
            continue;
        };
        let media = registry::read(&RegRoot::Machine, &path, "*PhysicalMediaType");
        if characteristics & NCF_PHYSICAL != 0 && media.as_ref().is_ok_and(is_wired_media) {
            found.push(id);
        }
    }
    found
}

pub(crate) fn read(adapter: &str, keyword: &str) -> Result<Value, PortError> {
    registry::read(&RegRoot::Machine, &adapter_path(adapter)?, keyword)
}

/// Whether this adapter's driver offers a property. Keywords vary by vendor and driver.
pub(crate) fn is_available(adapter: &str, keyword: &str) -> Result<bool, PortError> {
    let params = format!(r"{}\Ndi\Params\{keyword}", adapter_path(adapter)?);
    Ok(registry::read(&RegRoot::Machine, &params, "ParamDesc")? != Value::Absent)
}

pub(crate) fn write(adapter: &str, keyword: &str, value: &Value) -> Result<(), PortError> {
    let path = adapter_path(adapter)?;
    registry::write(&RegRoot::Machine, &path, keyword, value)?;
    restart(&path)
}

fn restart(path: &str) -> Result<(), PortError> {
    let Value::Text(instance) = registry::read(&RegRoot::Machine, path, "DeviceInstanceID")? else {
        return Err(PortError::Failed(
            "the setting changed, but the adapter couldn't be found to restart it".into(),
        ));
    };
    let name = HSTRING::from(instance.as_str());
    let mut node = 0u32;
    // SAFETY: the name outlives the call, and `node` is a valid place for the result.
    let located = unsafe { CM_Locate_DevNodeW(&mut node, &name, CM_LOCATE_DEVNODE_NORMAL) };
    if located != CR_SUCCESS {
        return Err(PortError::Failed(format!(
            "finding adapter {instance} failed (code {})",
            located.0
        )));
    }
    // SAFETY: the node came from the call above.
    let disabled = unsafe { CM_Disable_DevNode(node, 0) };
    if disabled != CR_SUCCESS {
        return Err(PortError::Failed(format!(
            "restarting adapter {instance} failed (code {})",
            disabled.0
        )));
    }
    // SAFETY: as above.
    let enabled = unsafe { CM_Enable_DevNode(node, 0) };
    if enabled == CR_SUCCESS {
        Ok(())
    } else {
        Err(PortError::Failed(format!(
            "adapter {instance} didn't come back (code {}). Re-enable it in Device Manager.",
            enabled.0
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wired_media_is_recognised_as_a_number_or_as_text() {
        // An Intel I225-V stores a number here. Wi-Fi is 9 and a loopback adapter is 0.
        assert!(is_wired_media(&Value::Dword(14)));
        assert!(is_wired_media(&Value::Text("14".into())));
        assert!(!is_wired_media(&Value::Dword(9)));
        assert!(!is_wired_media(&Value::Dword(0)));
        assert!(!is_wired_media(&Value::Text("9".into())));
        assert!(!is_wired_media(&Value::Absent));
    }

    #[test]
    fn only_four_digit_adapters_reach_the_registry() {
        assert!(adapter_path("0007").unwrap().ends_with(r"\0007"));
        for bad in ["7", "00070", r"0007\..", "abcd", "", r"..\.."] {
            assert!(adapter_path(bad).is_err(), "{bad}");
            assert!(read(bad, "*EEE").is_err(), "{bad}");
            assert!(write(bad, "*EEE", &Value::Absent).is_err(), "{bad}");
        }
    }

    #[test]
    fn listing_real_adapters_does_not_panic() {
        for adapter in wired_adapters() {
            assert_eq!(adapter.len(), 4);
            let _ = is_available(&adapter, "*InterruptModeration").unwrap();
        }
    }
}
