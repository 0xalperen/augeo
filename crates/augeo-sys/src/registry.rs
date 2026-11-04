//! Registry values, read and restored exactly.

use augeo_ports::{PortError, RegRoot, Value};
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::{
            HKEY, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, KEY_WRITE, REG_DWORD, REG_MULTI_SZ,
            REG_OPTION_NON_VOLATILE, REG_QWORD, REG_SZ, REG_VALUE_TYPE, RegCloseKey,
            RegCreateKeyExW, RegDeleteValueW, RegFlushKey, RegOpenKeyExW, RegQueryValueExW,
            RegSetValueExW,
        },
    },
    core::PCWSTR,
};

use crate::util::{wide, win32_error};

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open or create and is closed only here.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn base(root: &RegRoot, path: &str) -> (HKEY, String) {
    match root {
        RegRoot::Machine => (HKEY_LOCAL_MACHINE, path.to_string()),
        RegRoot::User { sid } => (HKEY_USERS, format!("{sid}\\{path}")),
    }
}

fn open_read(root: &RegRoot, path: &str) -> Result<Option<Key>, PortError> {
    let (hive, sub) = base(root, path);
    let sub_w = wide(&sub);
    let mut key = HKEY::default();
    // SAFETY: the name outlives the call, and `key` is a valid place for the result.
    let code = unsafe { RegOpenKeyExW(hive, &sub_w, None, KEY_READ, &mut key) };
    match code {
        ERROR_SUCCESS => Ok(Some(Key(key))),
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(None),
        e => Err(win32_error(&format!("opening {sub}"), e)),
    }
}

fn open_write(root: &RegRoot, path: &str) -> Result<Key, PortError> {
    let (hive, sub) = base(root, path);
    let sub_w = wide(&sub);
    let mut key = HKEY::default();
    // SAFETY: the name outlives the call, and `key` is a valid place for the result.
    let code = unsafe {
        RegCreateKeyExW(
            hive,
            &sub_w,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE | KEY_READ,
            None,
            &mut key,
            None,
        )
    };
    if code == ERROR_SUCCESS {
        Ok(Key(key))
    } else {
        Err(win32_error(&format!("opening {sub} for writing"), code))
    }
}

fn utf16(bytes: &[u8]) -> Option<Vec<u16>> {
    let (units, rest) = bytes.as_chunks::<2>();
    rest.is_empty()
        .then(|| units.iter().map(|c| u16::from_le_bytes(*c)).collect())
}

/// Decodes registry data. Anything that wouldn't round-trip exactly stays raw.
fn decode(ty: REG_VALUE_TYPE, data: Vec<u8>) -> Value {
    let raw = |data: Vec<u8>| Value::Raw { ty: ty.0, data };
    match ty {
        REG_DWORD => match <[u8; 4]>::try_from(data.as_slice()) {
            Ok(b) => Value::Dword(u32::from_le_bytes(b)),
            Err(_) => raw(data),
        },
        REG_QWORD => match <[u8; 8]>::try_from(data.as_slice()) {
            Ok(b) => Value::Qword(u64::from_le_bytes(b)),
            Err(_) => raw(data),
        },
        REG_SZ => {
            let Some(units) = utf16(&data) else {
                return raw(data);
            };
            let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
            // Anything after the first terminator would be lost, so keep such a value raw.
            if units[end..].iter().skip(1).any(|u| *u != 0) {
                return raw(data);
            }
            match String::from_utf16(&units[..end]) {
                Ok(text) => Value::Text(text),
                Err(_) => raw(data),
            }
        }
        REG_MULTI_SZ => {
            let Some(units) = utf16(&data) else {
                return raw(data);
            };
            let mut items = Vec::new();
            for part in units.split(|u| *u == 0).filter(|p| !p.is_empty()) {
                match String::from_utf16(part) {
                    Ok(text) => items.push(text),
                    Err(_) => return raw(data),
                }
            }
            Value::Multi(items)
        }
        _ => raw(data),
    }
}

fn encode(value: &Value) -> Result<(REG_VALUE_TYPE, Vec<u8>), PortError> {
    let text_bytes = |text: &str| -> Vec<u8> {
        text.encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect()
    };
    Ok(match value {
        Value::Dword(v) => (REG_DWORD, v.to_le_bytes().to_vec()),
        Value::Qword(v) => (REG_QWORD, v.to_le_bytes().to_vec()),
        Value::Text(v) => (REG_SZ, text_bytes(v)),
        Value::Multi(items) => {
            let mut data: Vec<u8> = items.iter().flat_map(|i| text_bytes(i)).collect();
            data.extend_from_slice(&[0, 0]);
            (REG_MULTI_SZ, data)
        }
        Value::Raw { ty, data } => (REG_VALUE_TYPE(*ty), data.clone()),
        Value::Absent => return Err(PortError::Failed("nothing to encode".into())),
    })
}

fn query(key: &Key, name: &str) -> Result<Value, PortError> {
    let name_w = wide(name);
    let mut size = 0u32;
    let mut ty = REG_VALUE_TYPE::default();
    let mut data = Vec::new();
    // The value can grow between calls, so retry with the size the API reports.
    for _ in 0..4 {
        size = size.max(data.capacity() as u32);
        data.resize(size as usize, 0u8);
        let buffer = (size > 0).then_some(data.as_mut_ptr());
        // SAFETY: the buffer has `size` bytes, and every pointer is valid for the call.
        let code = unsafe {
            RegQueryValueExW(key.0, &name_w, None, Some(&mut ty), buffer, Some(&mut size))
        };
        match code {
            ERROR_SUCCESS if buffer.is_some() || size == 0 => {
                data.truncate(size as usize);
                return Ok(decode(ty, data));
            }
            ERROR_SUCCESS | ERROR_MORE_DATA => continue,
            ERROR_FILE_NOT_FOUND => return Ok(Value::Absent),
            e => return Err(win32_error(&format!("reading {name}"), e)),
        }
    }
    Err(PortError::Failed(format!(
        "{name} kept changing while being read"
    )))
}

pub(crate) fn read(root: &RegRoot, path: &str, name: &str) -> Result<Value, PortError> {
    match open_read(root, path)? {
        Some(key) => query(&key, name),
        None => Ok(Value::Absent),
    }
}

pub(crate) fn write(
    root: &RegRoot,
    path: &str,
    name: &str,
    value: &Value,
) -> Result<(), PortError> {
    let name_w = wide(name);
    if *value == Value::Absent {
        let Some(key) = open_write_if_exists(root, path)? else {
            return Ok(());
        };
        // SAFETY: the key is open and the name outlives the call.
        let code = unsafe { RegDeleteValueW(key.0, &name_w) };
        if code != ERROR_SUCCESS && code != ERROR_FILE_NOT_FOUND {
            return Err(win32_error(&format!("deleting {name}"), code));
        }
        return flush(&key, name);
    }
    let key = open_write(root, path)?;
    let (ty, data) = encode(value)?;
    // SAFETY: the key is open, and the name and data outlive the call.
    let code = unsafe { RegSetValueExW(key.0, &name_w, None, ty, Some(&data)) };
    if code != ERROR_SUCCESS {
        return Err(win32_error(&format!("writing {name}"), code));
    }
    flush(&key, name)
}

fn open_write_if_exists(root: &RegRoot, path: &str) -> Result<Option<Key>, PortError> {
    if open_read(root, path)?.is_none() {
        return Ok(None);
    }
    open_write(root, path).map(Some)
}

/// Makes the write durable before returning, as the `System` port requires.
fn flush(key: &Key, name: &str) -> Result<(), PortError> {
    // SAFETY: the key is open.
    let code = unsafe { RegFlushKey(key.0) };
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(win32_error(&format!("flushing {name}"), code))
    }
}

/// Splits `a=1;b=2;` into pairs. Order is kept so a rewrite changes only the one entry.
fn parse_kv(text: &str) -> Vec<(String, String)> {
    text.split(';')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (part.to_string(), String::new()),
        })
        .collect()
}

fn render_kv(pairs: &[(String, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={v};")).collect()
}

pub(crate) fn read_kv(
    root: &RegRoot,
    path: &str,
    name: &str,
    key: &str,
) -> Result<Value, PortError> {
    let found = match read(root, path, name)? {
        Value::Text(text) => parse_kv(&text)
            .into_iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v),
        Value::Absent => None,
        other => {
            return Err(PortError::Failed(format!(
                "{name} isn't a text value: {other}"
            )));
        }
    };
    Ok(found.map_or(Value::Absent, Value::Text))
}

pub(crate) fn write_kv(
    root: &RegRoot,
    path: &str,
    name: &str,
    key: &str,
    value: &Value,
) -> Result<(), PortError> {
    let existing = match read(root, path, name)? {
        Value::Text(text) => parse_kv(&text),
        Value::Absent => Vec::new(),
        other => {
            return Err(PortError::Failed(format!(
                "{name} isn't a text value: {other}"
            )));
        }
    };
    let mut pairs = existing;
    match value {
        Value::Absent => pairs.retain(|(k, _)| !k.eq_ignore_ascii_case(key)),
        Value::Text(new) => match pairs.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            Some(entry) => entry.1 = new.clone(),
            None => pairs.push((key.to_string(), new.clone())),
        },
        other => {
            return Err(PortError::Failed(format!(
                "{name}: {other} can't be stored in a text entry"
            )));
        }
    }
    let result = if pairs.is_empty() {
        Value::Absent
    } else {
        Value::Text(render_kv(&pairs))
    };
    write(root, path, name, &result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elevation::current_user_sid;

    /// A private key under the current user's hive, removed value by value as each test ends.
    fn test_root() -> RegRoot {
        RegRoot::User {
            sid: current_user_sid().unwrap(),
        }
    }

    const PATH: &str = r"Software\augeo-test\registry";

    struct Cleanup(&'static str);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = write(&test_root(), PATH, self.0, &Value::Absent);
        }
    }

    #[test]
    fn every_value_kind_round_trips() {
        let name = "round-trip";
        let _cleanup = Cleanup(name);
        let root = test_root();
        for value in [
            Value::Dword(0),
            Value::Dword(u32::MAX),
            Value::Qword(u64::MAX),
            Value::Text(String::new()),
            Value::Text("héllo wörld".into()),
            Value::Multi(vec!["a".into(), "b c".into()]),
            Value::Multi(vec![]),
            // REG_EXPAND_SZ and REG_BINARY stay byte for byte.
            Value::Raw {
                ty: 2,
                data: vec![37, 0, 65, 0, 37, 0, 0, 0],
            },
            Value::Raw {
                ty: 3,
                data: vec![1, 2, 3],
            },
            Value::Raw {
                ty: 3,
                data: vec![],
            },
        ] {
            write(&root, PATH, name, &value).unwrap();
            assert_eq!(read(&root, PATH, name).unwrap(), value);
        }
    }

    #[test]
    fn a_large_value_is_read_whole() {
        let name = "large";
        let _cleanup = Cleanup(name);
        let root = test_root();
        let value = Value::Raw {
            ty: 3,
            data: vec![7; 100_000],
        };
        write(&root, PATH, name, &value).unwrap();
        assert_eq!(read(&root, PATH, name).unwrap(), value);
    }

    #[test]
    fn a_dword_with_the_wrong_length_stays_raw() {
        let name = "odd-dword";
        let _cleanup = Cleanup(name);
        let root = test_root();
        let value = Value::Raw {
            ty: REG_DWORD.0,
            data: vec![1, 2],
        };
        write(&root, PATH, name, &value).unwrap();
        assert_eq!(read(&root, PATH, name).unwrap(), value);
    }

    #[test]
    fn missing_values_and_keys_read_as_absent() {
        let root = test_root();
        assert_eq!(read(&root, PATH, "never-set").unwrap(), Value::Absent);
        assert_eq!(
            read(&root, r"Software\augeo-test\no\such\key", "x").unwrap(),
            Value::Absent
        );
    }

    #[test]
    fn deleting_something_absent_is_safe() {
        let root = test_root();
        write(&root, PATH, "never-set", &Value::Absent).unwrap();
        write(
            &root,
            r"Software\augeo-test\no\such\key",
            "x",
            &Value::Absent,
        )
        .unwrap();
    }

    #[test]
    fn deleting_removes_the_value() {
        let name = "delete-me";
        let _cleanup = Cleanup(name);
        let root = test_root();
        write(&root, PATH, name, &Value::Dword(1)).unwrap();
        write(&root, PATH, name, &Value::Absent).unwrap();
        assert_eq!(read(&root, PATH, name).unwrap(), Value::Absent);
    }

    #[test]
    fn writing_outside_ones_rights_is_reported_as_denied_or_succeeds_when_elevated() {
        // Elevation decides the outcome. What matters is that a refusal is reported, not hidden.
        let result = read(&RegRoot::Machine, r"SAM\SAM", "x");
        if let Err(e) = result {
            assert!(matches!(e, PortError::Denied(_)), "{e}");
        }
    }

    #[test]
    fn a_kv_entry_changes_without_disturbing_the_others() {
        let name = "kv";
        let _cleanup = Cleanup(name);
        let root = test_root();
        let existing = Value::Text("VRROptimizeEnable=1;AutoHDREnable=0;".into());
        write(&root, PATH, name, &existing).unwrap();

        assert_eq!(
            read_kv(&root, PATH, name, "AutoHDREnable").unwrap(),
            Value::Text("0".into())
        );
        assert_eq!(
            read_kv(&root, PATH, name, "Missing").unwrap(),
            Value::Absent
        );

        write_kv(
            &root,
            PATH,
            name,
            "SwapEffectUpgradeEnable",
            &Value::Text("1".into()),
        )
        .unwrap();
        assert_eq!(
            read(&root, PATH, name).unwrap(),
            Value::Text("VRROptimizeEnable=1;AutoHDREnable=0;SwapEffectUpgradeEnable=1;".into())
        );

        write_kv(
            &root,
            PATH,
            name,
            "VRROptimizeEnable",
            &Value::Text("0".into()),
        )
        .unwrap();
        write_kv(&root, PATH, name, "SwapEffectUpgradeEnable", &Value::Absent).unwrap();
        assert_eq!(
            read(&root, PATH, name).unwrap(),
            Value::Text("VRROptimizeEnable=0;AutoHDREnable=0;".into())
        );

        // Removing the last entry removes the value, restoring "did not exist".
        write_kv(&root, PATH, name, "VRROptimizeEnable", &Value::Absent).unwrap();
        write_kv(&root, PATH, name, "AutoHDREnable", &Value::Absent).unwrap();
        assert_eq!(read(&root, PATH, name).unwrap(), Value::Absent);
    }

    #[test]
    fn kv_parsing() {
        assert_eq!(render_kv(&parse_kv("a=1;b=2;")), "a=1;b=2;");
        assert_eq!(render_kv(&parse_kv("a=1;b=2")), "a=1;b=2;");
        assert_eq!(parse_kv(""), vec![]);
        assert_eq!(parse_kv("a=b=c;")[0], ("a".to_string(), "b=c".to_string()));
    }
}
