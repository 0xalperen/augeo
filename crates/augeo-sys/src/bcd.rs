//! Boot settings, through `bcdedit` from System32 with fixed arguments and no shell.
//!
//! Only `{current}` is ever touched, and the catalog allows only a short list of elements.

use std::{os::windows::process::CommandExt, process::Command};

use augeo_ports::{PortError, Value};

use crate::util::system_directory;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn is_safe_word(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric())
}

fn run(args: &[&str]) -> Result<String, PortError> {
    let system32 = system_directory()?;
    let output = Command::new(system32.join("bcdedit.exe"))
        .args(args)
        .current_dir(&system32)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| PortError::Failed(format!("couldn't run bcdedit: {e}")))?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(text);
    }
    let message = format!("{text}{}", String::from_utf8_lossy(&output.stderr));
    let message = message.trim().to_string();
    // bcdedit can't read the store without elevation, and its message is localized.
    Err(if output.status.code() == Some(1) {
        PortError::Denied(format!("bcdedit needs elevation: {message}"))
    } else {
        PortError::Failed(format!("bcdedit failed: {message}"))
    })
}

/// Finds an element's value in `bcdedit /enum {current} /v` output.
fn parse_element(output: &str, element: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let name = parts.next()?;
        if !name.eq_ignore_ascii_case(element) {
            return None;
        }
        let rest: Vec<&str> = parts.collect();
        Some(rest.join(" "))
    })
}

pub(crate) fn read(element: &str) -> Result<Value, PortError> {
    if !is_safe_word(element) {
        return Err(PortError::Failed(format!(
            "invalid boot setting '{element}'"
        )));
    }
    let output = run(&["/enum", "{current}", "/v"])?;
    Ok(parse_element(&output, element).map_or(Value::Absent, Value::Text))
}

pub(crate) fn write(element: &str, value: &Value) -> Result<(), PortError> {
    if !is_safe_word(element) {
        return Err(PortError::Failed(format!(
            "invalid boot setting '{element}'"
        )));
    }
    match value {
        Value::Absent => run(&["/deletevalue", "{current}", element]).map(|_| ()),
        Value::Text(text) if is_safe_word(text) => {
            run(&["/set", "{current}", element, text]).map(|_| ())
        }
        other => Err(PortError::Failed(format!(
            "boot setting {element} can't be set to {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Windows Boot Loader
-------------------
identifier              {12345678-1234-1234-1234-123456789012}
device                  partition=C:
path                    \\WINDOWS\\system32\\winload.efi
description             Windows 11
useplatformclock        Yes
tscsyncpolicy           Enhanced
nx                      OptIn
";

    /// Only reads. `nx` is set on every normal Windows boot entry.
    #[cfg(feature = "admin-tests")]
    #[test]
    fn elevated_reads_find_the_real_boot_entry() {
        let output = run(&["/enum", "{current}", "/v"]).unwrap();
        println!("{output}");
        assert!(matches!(read("nx").unwrap(), Value::Text(_)), "{output}");
        assert_eq!(read("nosuchelement").unwrap(), Value::Absent);
        for element in [
            "useplatformclock",
            "useplatformtick",
            "disabledynamictick",
            "tscsyncpolicy",
        ] {
            println!("{element} = {:?}", read(element).unwrap());
        }
    }

    #[test]
    fn elements_are_found_by_exact_name() {
        assert_eq!(
            parse_element(SAMPLE, "useplatformclock").as_deref(),
            Some("Yes")
        );
        assert_eq!(
            parse_element(SAMPLE, "USEPLATFORMCLOCK").as_deref(),
            Some("Yes")
        );
        assert_eq!(
            parse_element(SAMPLE, "tscsyncpolicy").as_deref(),
            Some("Enhanced")
        );
        assert_eq!(parse_element(SAMPLE, "useplatformtick"), None);
        assert_eq!(parse_element(SAMPLE, "useplatform"), None);
        assert_eq!(parse_element("", "nx"), None);
    }

    #[test]
    fn nothing_unsafe_reaches_the_command_line() {
        for bad in ["a b", "a;b", "a&calc", "{current}", "", "a\"b", "a/b"] {
            assert!(read(bad).is_err(), "{bad}");
            assert!(write(bad, &Value::Absent).is_err(), "{bad}");
            assert!(write("nx", &Value::Text(bad.into())).is_err(), "{bad}");
        }
        assert!(write("nx", &Value::Dword(1)).is_err());
    }
}
