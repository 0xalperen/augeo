//! Facts about this machine. Everything here is read-only and works without elevation.

use std::arch::x86_64::{__cpuid, __cpuid_count};

use augeo_ports::{PortError, RegRoot, Value};
use windows::Win32::System::{
    Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
    SystemInformation::GetTickCount64,
};

use crate::registry;

const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
const BATTERY_NONE: u8 = 128;
const BATTERY_UNKNOWN: u8 = 255;

/// The CPU maker, from CPUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Amd,
    Intel,
    Other,
}

/// Windows' build number, such as 26100.
pub fn windows_build() -> Result<u32, PortError> {
    match registry::read(&RegRoot::Machine, CURRENT_VERSION, "CurrentBuildNumber")? {
        Value::Text(text) => text
            .trim()
            .parse()
            .map_err(|_| PortError::Failed(format!("odd Windows build number '{text}'"))),
        other => Err(PortError::Failed(format!(
            "odd Windows build number {other}"
        ))),
    }
}

pub fn cpu_vendor() -> Vendor {
    let id = __cpuid(0);
    let mut name = [0u8; 12];
    name[..4].copy_from_slice(&id.ebx.to_le_bytes());
    name[4..8].copy_from_slice(&id.edx.to_le_bytes());
    name[8..].copy_from_slice(&id.ecx.to_le_bytes());
    match &name {
        b"AuthenticAMD" => Vendor::Amd,
        b"GenuineIntel" => Vendor::Intel,
        _ => Vendor::Other,
    }
}

/// The CPU's marketing name, such as "AMD Ryzen 5 7600X 6-Core Processor".
pub fn cpu_name() -> String {
    if __cpuid(0x8000_0000).eax < 0x8000_0004 {
        return String::new();
    }
    let mut bytes = Vec::with_capacity(48);
    for leaf in 0x8000_0002..=0x8000_0004u32 {
        let r = __cpuid(leaf);
        for word in [r.eax, r.ebx, r.ecx, r.edx] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    }
    String::from_utf8_lossy(&bytes)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string()
}

/// Whether this is an AMD X3D chip with two core dies, like the 7950X3D or 9950X3D. On these, core
/// parking steers games onto the right die, so anything disabling it would hurt.
pub fn is_dual_ccd_x3d() -> bool {
    if cpu_vendor() != Vendor::Amd || !cpu_name().contains("X3D") {
        return false;
    }
    // CPUID leaf 0x8000001D gives the threads sharing an L3 cache. With two dies, fewer threads share
    // one L3 than the CPU has in total.
    let total_threads = (__cpuid(1).ebx >> 16) & 0xFF;
    (0..8)
        .map(|sub| __cpuid_count(0x8000_001D, sub))
        .find(|r| (r.eax & 0x1F) != 0 && ((r.eax >> 5) & 0x7) == 3)
        .is_some_and(|l3| {
            let sharing = ((l3.eax >> 14) & 0xFFF) + 1;
            sharing < total_threads
        })
}

/// Whether this PC has a battery. Unknown counts as no, since only a real report should gate a tweak.
pub fn has_battery() -> bool {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid place for the result.
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return false;
    }
    status.BatteryFlag & BATTERY_NONE == 0 && status.BatteryFlag != BATTERY_UNKNOWN
}

/// Asks Windows to restart now, using `shutdown.exe` from System32 with fixed arguments. Only ever
/// called after the user explicitly said yes.
pub fn request_restart() -> Result<(), PortError> {
    let system32 = crate::util::system_directory()?;
    let status = std::process::Command::new(system32.join("shutdown.exe"))
        .args(["/r", "/t", "0"])
        .current_dir(&system32)
        .status()
        .map_err(|e| PortError::Failed(format!("couldn't run shutdown: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(PortError::Failed(format!("shutdown exited with {status}")))
    }
}

/// Identifies the current boot, from when the kernel started.
///
/// Fast Startup keeps the kernel session across a shutdown, and uptime keeps counting across it, so
/// this only changes on a real restart, which is what a tweak needing a restart wants. The value is
/// rounded so that small clock corrections don't look like a reboot.
pub fn boot_id(now_ms: u64) -> String {
    // SAFETY: this takes no arguments.
    let uptime = unsafe { GetTickCount64() };
    format!("{}", now_ms.saturating_sub(uptime) / 300_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_is_a_real_windows_build() {
        assert!(windows_build().unwrap() >= 10240);
    }

    #[test]
    fn cpu_facts_can_be_read() {
        let _ = cpu_vendor();
        let _ = is_dual_ccd_x3d();
        assert!(!cpu_name().is_empty());
    }

    #[test]
    fn the_boot_id_is_stable_within_a_boot() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // The rounding can only straddle a boundary if the boot happened right at one.
        let a: u64 = boot_id(now).parse().unwrap();
        let b: u64 = boot_id(now + 1000).parse().unwrap();
        assert!(a.abs_diff(b) <= 1);
    }

    #[test]
    fn battery_detection_runs() {
        let _ = has_battery();
    }
}
