//! Read-only GPU facts through `nvml.dll`, for the check report. NVML never changes anything here.

use std::ffi::c_void;

use windows::{
    Win32::{
        Foundation::HMODULE,
        System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
    },
    core::{HSTRING, PCSTR},
};

type Device = *mut c_void;
const SUCCESS: u32 = 0;

#[repr(C)]
#[derive(Default)]
struct Triple {
    total: u64,
    free: u64,
    used: u64,
}

/// What NVML reports about the first GPU. Any single field can be missing on some cards or drivers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GpuInfo {
    pub name: Option<String>,
    pub driver: Option<String>,
    pub vram_mib: Option<u64>,
    pub bar1_mib: Option<u64>,
    pub pcie_gen_current: Option<u32>,
    pub pcie_gen_max: Option<u32>,
    pub pcie_width_current: Option<u32>,
    pub pcie_width_max: Option<u32>,
    pub performance_state: Option<u32>,
}

impl GpuInfo {
    /// Whether Resizable BAR looks active: the BAR1 window covers all of the video memory.
    /// This is an inference from BAR1 size, not a flag the driver reports.
    pub fn resizable_bar_likely(&self) -> Option<bool> {
        Some(self.bar1_mib? >= self.vram_mib?)
    }
}

struct Nvml(HMODULE);

impl Nvml {
    fn function<T: Copy>(&self, name: &str) -> Option<T> {
        const { assert!(size_of::<T>() == size_of::<usize>()) };
        let terminated = format!("{name}\0");
        // SAFETY: the module is loaded, and the name is terminated and outlives the call.
        let pointer = unsafe { GetProcAddress(self.0, PCSTR(terminated.as_ptr())) }?;
        // SAFETY: T is always a function pointer type matching nvml.h for this name, as written by
        // the callers below, and the size was checked above.
        Some(unsafe { std::mem::transmute_copy(&pointer) })
    }

    fn device_u32(&self, device: Device, name: &str) -> Option<u32> {
        let function: unsafe extern "C" fn(Device, *mut u32) -> u32 = self.function(name)?;
        let mut value = 0;
        // SAFETY: the handle came from NVML, and `value` is a valid place for the result.
        (unsafe { function(device, &mut value) } == SUCCESS).then_some(value)
    }

    fn device_triple(&self, device: Device, name: &str) -> Option<Triple> {
        let function: unsafe extern "C" fn(Device, *mut Triple) -> u32 = self.function(name)?;
        let mut value = Triple::default();
        // SAFETY: as above.
        (unsafe { function(device, &mut value) } == SUCCESS).then_some(value)
    }
}

fn text(buffer: &[u8]) -> String {
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// Reads GPU facts. Returns `None` when there's no NVIDIA driver.
pub fn query() -> Option<GpuInfo> {
    let name = HSTRING::from("nvml.dll");
    // SAFETY: the name outlives the call, and the search is limited to System32.
    let module = unsafe { LoadLibraryExW(&name, None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.ok()?;
    let nvml = Nvml(module);

    let init: unsafe extern "C" fn() -> u32 = nvml.function("nvmlInit_v2")?;
    // SAFETY: this takes no arguments.
    if unsafe { init() } != SUCCESS {
        return None;
    }
    let mut info = GpuInfo::default();

    if let Some(driver) =
        nvml.function::<unsafe extern "C" fn(*mut u8, u32) -> u32>("nvmlSystemGetDriverVersion")
    {
        let mut buffer = [0u8; 96];
        // SAFETY: the buffer is valid for the length passed.
        if unsafe { driver(buffer.as_mut_ptr(), buffer.len() as u32) } == SUCCESS {
            info.driver = Some(text(&buffer));
        }
    }

    let by_index: Option<unsafe extern "C" fn(u32, *mut Device) -> u32> =
        nvml.function("nvmlDeviceGetHandleByIndex_v2");
    let mut device: Device = std::ptr::null_mut();
    // SAFETY: `device` is a valid place for the result.
    if by_index.is_some_and(|f| unsafe { f(0, &mut device) } == SUCCESS) {
        if let Some(get_name) =
            nvml.function::<unsafe extern "C" fn(Device, *mut u8, u32) -> u32>("nvmlDeviceGetName")
        {
            let mut buffer = [0u8; 96];
            // SAFETY: the handle came from NVML, and the buffer is valid for the length passed.
            if unsafe { get_name(device, buffer.as_mut_ptr(), buffer.len() as u32) } == SUCCESS {
                info.name = Some(text(&buffer));
            }
        }
        info.vram_mib = nvml
            .device_triple(device, "nvmlDeviceGetMemoryInfo")
            .map(|m| m.total >> 20);
        info.bar1_mib = nvml
            .device_triple(device, "nvmlDeviceGetBAR1MemoryInfo")
            .map(|m| m.total >> 20);
        info.pcie_gen_current = nvml.device_u32(device, "nvmlDeviceGetCurrPcieLinkGeneration");
        info.pcie_gen_max = nvml.device_u32(device, "nvmlDeviceGetMaxPcieLinkGeneration");
        info.pcie_width_current = nvml.device_u32(device, "nvmlDeviceGetCurrPcieLinkWidth");
        info.pcie_width_max = nvml.device_u32(device, "nvmlDeviceGetMaxPcieLinkWidth");
        info.performance_state = nvml.device_u32(device, "nvmlDeviceGetPerformanceState");
    }

    if let Some(shutdown) = nvml.function::<unsafe extern "C" fn() -> u32>("nvmlShutdown") {
        // SAFETY: this balances the successful init above.
        unsafe { shutdown() };
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizable_bar_is_inferred_only_with_both_numbers() {
        let info = |bar1, vram| GpuInfo {
            bar1_mib: bar1,
            vram_mib: vram,
            ..GpuInfo::default()
        };
        assert_eq!(
            info(Some(16384), Some(12282)).resizable_bar_likely(),
            Some(true)
        );
        assert_eq!(
            info(Some(256), Some(12282)).resizable_bar_likely(),
            Some(false)
        );
        assert_eq!(info(None, Some(12282)).resizable_bar_likely(), None);
        assert_eq!(info(Some(256), None).resizable_bar_likely(), None);
    }

    #[test]
    fn missing_or_present_nvml_is_handled_without_panicking() {
        let _ = query();
    }

    #[cfg(feature = "gpu-tests")]
    #[test]
    fn real_gpu_values_are_sensible() {
        let info = query().expect("this test needs an NVIDIA driver");
        assert!(
            info.name.as_deref().is_some_and(|n| n.contains("NVIDIA")),
            "{info:?}"
        );
        assert!(info.driver.is_some(), "{info:?}");
        assert!(info.vram_mib.is_some_and(|v| v > 1024), "{info:?}");
        assert!(info.pcie_gen_max.is_some_and(|g| g >= 1), "{info:?}");
    }
}
