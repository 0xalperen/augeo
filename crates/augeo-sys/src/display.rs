//! The display mode. Used to find and set the best refresh rate at the current resolution.

use augeo_ports::{PortError, Value};
use windows::{
    Win32::Graphics::Gdi::{
        CDS_TEST, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
        DISPLAY_DEVICE_PRIMARY_DEVICE, DISPLAY_DEVICEW, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT,
        DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplayDevicesW,
        EnumDisplaySettingsW,
    },
    core::{HSTRING, PCWSTR},
};

/// One display mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

impl Mode {
    pub fn render(self) -> String {
        format!("{}x{}@{}", self.width, self.height, self.hz)
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (size, hz) = text.split_once('@')?;
        let (width, height) = size.split_once('x')?;
        Some(Self {
            width: width.parse().ok()?,
            height: height.parse().ok()?,
            hz: hz.parse().ok()?,
        })
    }
}

/// A frame cap that keeps a VRR display inside its range: at least 3 below refresh, and following
/// the formula Reflex is reported to use, which leaves more margin at high refresh rates. It's a
/// starting point shown before applying, and benchmarks can check whether it suits a given game.
pub fn frame_cap_for(hz: u32) -> Option<u32> {
    if hz < 30 {
        return None;
    }
    let reflex_style = f64::from(hz) - f64::from(hz) * f64::from(hz) / 3600.0;
    Some((reflex_style.floor() as u32).min(hz - 3))
}

fn name_of(device: &DISPLAY_DEVICEW) -> String {
    let end = device
        .DeviceName
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(device.DeviceName.len());
    String::from_utf16_lossy(&device.DeviceName[..end])
}

/// The primary display's device name, such as `\\.\DISPLAY1`.
pub fn primary_device() -> Option<String> {
    for index in 0..16 {
        let mut device = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: `device` has its size set, and is a valid place for the result.
        let found = unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) };
        if !found.as_bool() {
            return None;
        }
        if device.StateFlags.contains(DISPLAY_DEVICE_PRIMARY_DEVICE) {
            return Some(name_of(&device));
        }
    }
    None
}

fn devmode() -> DEVMODEW {
    DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    }
}

fn mode_of(dm: &DEVMODEW) -> Mode {
    Mode {
        width: dm.dmPelsWidth,
        height: dm.dmPelsHeight,
        hz: dm.dmDisplayFrequency,
    }
}

/// The mode in use now.
pub fn current_mode(device: &str) -> Result<Mode, PortError> {
    let name = HSTRING::from(device);
    let mut dm = devmode();
    // SAFETY: the name outlives the call, and `dm` has its size set.
    let ok = unsafe { EnumDisplaySettingsW(&name, ENUM_CURRENT_SETTINGS, &mut dm) };
    if ok.as_bool() {
        Ok(mode_of(&dm))
    } else {
        Err(PortError::Failed(format!(
            "couldn't read the mode of {device}"
        )))
    }
}

/// Every mode the display reports.
fn modes(device: &str) -> Vec<Mode> {
    let name = HSTRING::from(device);
    let mut out = Vec::new();
    for index in 0.. {
        let mut dm = devmode();
        // SAFETY: as above. The call fails once the index passes the last mode.
        let ok = unsafe { EnumDisplaySettingsW(&name, ENUM_DISPLAY_SETTINGS_MODE(index), &mut dm) };
        if !ok.as_bool() {
            break;
        }
        out.push(mode_of(&dm));
    }
    out
}

/// The highest refresh rate at this resolution. Nothing else about the mode changes.
fn best_of(current: Mode, modes: &[Mode]) -> Mode {
    modes
        .iter()
        .filter(|m| m.width == current.width && m.height == current.height)
        .copied()
        .max_by_key(|m| m.hz)
        .unwrap_or(current)
        .max_hz(current)
}

impl Mode {
    fn max_hz(self, other: Mode) -> Mode {
        if other.hz > self.hz { other } else { self }
    }
}

/// The best mode for this display at its current resolution.
pub fn best_mode(device: &str) -> Result<Mode, PortError> {
    Ok(best_of(current_mode(device)?, &modes(device)))
}

pub(crate) fn read(device: &str) -> Result<Value, PortError> {
    Ok(Value::Text(current_mode(device)?.render()))
}

/// Sets a mode. It's tested with the driver first, so an impossible mode changes nothing.
pub(crate) fn write(device: &str, value: &Value) -> Result<(), PortError> {
    let Value::Text(text) = value else {
        return Err(PortError::Failed(format!("{value} isn't a display mode")));
    };
    let mode = Mode::parse(text)
        .ok_or_else(|| PortError::Failed(format!("'{text}' isn't a display mode")))?;
    let name = HSTRING::from(device);
    let mut dm = devmode();
    dm.dmPelsWidth = mode.width;
    dm.dmPelsHeight = mode.height;
    dm.dmDisplayFrequency = mode.hz;
    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    // SAFETY: the name and mode outlive the call. No window handle or extra parameter is used.
    let tested = unsafe { ChangeDisplaySettingsExW(&name, Some(&dm), None, CDS_TEST, None) };
    if tested != DISP_CHANGE_SUCCESSFUL {
        return Err(PortError::Failed(format!(
            "the display driver refused {text} (code {})",
            tested.0
        )));
    }
    // SAFETY: as above.
    let changed =
        unsafe { ChangeDisplaySettingsExW(&name, Some(&dm), None, CDS_UPDATEREGISTRY, None) };
    if changed == DISP_CHANGE_SUCCESSFUL {
        Ok(())
    } else {
        Err(PortError::Failed(format!(
            "setting {text} failed (code {})",
            changed.0
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, hz: u32) -> Mode {
        Mode { width, height, hz }
    }

    #[test]
    fn the_best_mode_keeps_the_resolution() {
        let current = mode(2560, 1440, 60);
        let modes = [
            mode(1920, 1080, 240),
            mode(2560, 1440, 60),
            mode(2560, 1440, 144),
            mode(2560, 1440, 165),
        ];
        assert_eq!(best_of(current, &modes), mode(2560, 1440, 165));
        assert_eq!(best_of(current, &[]), current);
        // A list without anything better never lowers the refresh rate.
        assert_eq!(
            best_of(mode(2560, 1440, 165), &[mode(2560, 1440, 60)]).hz,
            165
        );
    }

    #[test]
    fn modes_round_trip_as_text() {
        let m = mode(2560, 1440, 165);
        assert_eq!(m.render(), "2560x1440@165");
        assert_eq!(Mode::parse("2560x1440@165"), Some(m));
        for bad in ["", "2560x1440", "x@", "2560@165", "axb@c"] {
            assert_eq!(Mode::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn frame_caps_stay_below_refresh() {
        assert_eq!(frame_cap_for(60), Some(57));
        assert_eq!(frame_cap_for(144), Some(138));
        assert_eq!(frame_cap_for(240), Some(224));
        assert_eq!(frame_cap_for(360), Some(324));
        assert_eq!(frame_cap_for(0), None);
        assert_eq!(frame_cap_for(20), None);
        for hz in 30..500 {
            let cap = frame_cap_for(hz).unwrap();
            assert!(cap + 3 <= hz, "{hz}: {cap}");
        }
    }

    #[test]
    fn reading_the_real_display_does_not_panic() {
        if let Some(device) = primary_device() {
            let current = current_mode(&device);
            if let Ok(current) = current {
                assert!(best_mode(&device).unwrap().hz >= current.hz);
            }
        }
    }
}
