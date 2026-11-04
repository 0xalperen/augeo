//! Power plans, through the native power API rather than `powercfg`.

use augeo_ports::{PortError, Value};
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HLOCAL, LocalFree, WIN32_ERROR},
        System::Power::{
            PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex,
            PowerSetActiveScheme, PowerWriteACValueIndex, PowerWriteDCValueIndex,
        },
    },
    core::GUID,
};

use crate::util::{guid_text, parse_guid, win32_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Line {
    Ac,
    Dc,
}

pub(crate) fn active_scheme() -> Result<String, PortError> {
    let mut active: *mut GUID = std::ptr::null_mut();
    // SAFETY: `active` is a valid place for the pointer the API allocates.
    let code = unsafe { PowerGetActiveScheme(None, &mut active) };
    if code != ERROR_SUCCESS || active.is_null() {
        return Err(win32_error("reading the active power plan", code));
    }
    // SAFETY: the API succeeded, so the pointer is valid until it is freed below.
    let text = guid_text(unsafe { &*active });
    // SAFETY: this memory came from the API, which says to release it with LocalFree.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(active.cast())));
    }
    Ok(text)
}

pub(crate) fn set_active_scheme(value: &Value) -> Result<(), PortError> {
    let Value::Text(scheme) = value else {
        return Err(PortError::Failed(format!("{value} isn't a power plan")));
    };
    let guid = parse_guid(scheme)?;
    // SAFETY: the GUID outlives the call.
    let code = unsafe { PowerSetActiveScheme(None, Some(&guid)) };
    check(&format!("selecting power plan {scheme}"), code)
}

fn check(context: &str, code: WIN32_ERROR) -> Result<(), PortError> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(win32_error(context, code))
    }
}

pub(crate) fn read_index(
    line: Line,
    scheme: &str,
    subgroup: &str,
    setting: &str,
) -> Result<Value, PortError> {
    let (scheme_g, sub_g, set_g) = (
        parse_guid(scheme)?,
        parse_guid(subgroup)?,
        parse_guid(setting)?,
    );
    let mut value = 0u32;
    // SAFETY: the GUIDs and `value` outlive the call.
    let code = unsafe {
        match line {
            Line::Ac => PowerReadACValueIndex(
                None,
                Some(&scheme_g),
                Some(&sub_g),
                Some(&set_g),
                &mut value,
            ),
            Line::Dc => WIN32_ERROR(PowerReadDCValueIndex(
                None,
                Some(&scheme_g),
                Some(&sub_g),
                Some(&set_g),
                &mut value,
            )),
        }
    };
    match code {
        ERROR_SUCCESS => Ok(Value::Dword(value)),
        ERROR_FILE_NOT_FOUND => Ok(Value::Absent),
        e => Err(win32_error(&format!("reading power setting {setting}"), e)),
    }
}

pub(crate) fn write_index(
    line: Line,
    scheme: &str,
    subgroup: &str,
    setting: &str,
    value: &Value,
) -> Result<(), PortError> {
    let Value::Dword(number) = value else {
        return Err(PortError::Failed(format!(
            "power setting {setting} can't be set to {value}"
        )));
    };
    let (scheme_g, sub_g, set_g) = (
        parse_guid(scheme)?,
        parse_guid(subgroup)?,
        parse_guid(setting)?,
    );
    // SAFETY: the GUIDs outlive the call.
    let code = unsafe {
        match line {
            Line::Ac => {
                PowerWriteACValueIndex(None, &scheme_g, Some(&sub_g), Some(&set_g), *number)
            }
            Line::Dc => WIN32_ERROR(PowerWriteDCValueIndex(
                None,
                &scheme_g,
                Some(&sub_g),
                Some(&set_g),
                *number,
            )),
        }
    };
    check(&format!("writing power setting {setting}"), code)?;
    // A changed index takes effect only when the plan is selected again.
    if active_scheme()?.eq_ignore_ascii_case(scheme) {
        // SAFETY: the GUID outlives the call.
        let code = unsafe { PowerSetActiveScheme(None, Some(&scheme_g)) };
        check("re-selecting the active power plan", code)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Processor power management: minimum processor state. Present on every plan.
    const SUB_PROCESSOR: &str = "54533251-82be-4824-96c1-47b60b740d00";
    const MIN_STATE: &str = "893dee8e-2bef-41e0-89c6-b55d0929964c";

    #[test]
    fn the_active_plan_is_a_valid_guid_and_readable() {
        let scheme = active_scheme().unwrap();
        assert!(parse_guid(&scheme).is_ok(), "{scheme}");
        for line in [Line::Ac, Line::Dc] {
            match read_index(line, &scheme, SUB_PROCESSOR, MIN_STATE).unwrap() {
                Value::Dword(v) => assert!(v <= 100, "{v}"),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_setting_that_does_not_exist_is_absent() {
        let scheme = active_scheme().unwrap();
        let missing = "00000000-0000-0000-0000-000000000001";
        assert_eq!(
            read_index(Line::Ac, &scheme, SUB_PROCESSOR, missing).unwrap(),
            Value::Absent
        );
    }

    #[test]
    fn bad_values_are_refused_before_touching_anything() {
        assert!(set_active_scheme(&Value::Dword(1)).is_err());
        assert!(write_index(Line::Ac, "bad", SUB_PROCESSOR, MIN_STATE, &Value::Dword(1)).is_err());
        assert!(
            write_index(
                Line::Ac,
                "381b4222-f694-41f0-9685-ff5bb260df2e",
                SUB_PROCESSOR,
                MIN_STATE,
                &Value::Text("x".into())
            )
            .is_err()
        );
    }
}
