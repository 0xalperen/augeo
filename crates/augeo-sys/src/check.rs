//! The read-only check report: things that matter a lot but can't or shouldn't be changed from here.
//! Nothing in this module writes anything.

use augeo_ports::{RegRoot, Value};
use windows::{
    Win32::{
        Storage::FileSystem::GetDiskFreeSpaceExW,
        System::SystemInformation::{
            FIRMWARE_TABLE_PROVIDER, GetSystemFirmwareTable, GetTickCount64,
        },
    },
    core::HSTRING,
};

use crate::{detect, display, nvml, registry};

/// How much a finding deserves attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Good,
    Attention,
    Info,
    Unknown,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Good => "OK",
            Verdict::Attention => "LOOK",
            Verdict::Info => "INFO",
            Verdict::Unknown => "????",
        }
    }
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub verdict: Verdict,
    pub title: String,
    pub detail: String,
    pub advice: Option<String>,
}

fn finding(verdict: Verdict, title: &str, detail: String, advice: Option<&str>) -> Finding {
    Finding {
        verdict,
        title: title.to_string(),
        detail,
        advice: advice.map(str::to_string),
    }
}

const SMBIOS_TYPE_BIOS: u8 = 0;
const SMBIOS_TYPE_MEMORY: u8 = 17;
const SMBIOS_END: u8 = 127;
const DDR4: u8 = 0x1A;
const DDR5: u8 = 0x22;

/// One populated memory module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryModule {
    pub rated_mts: u16,
    pub configured_mts: u16,
    pub kind: u8,
}

/// What the firmware reports about BIOS and memory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Firmware {
    pub bios_version: Option<String>,
    /// `MM/DD/YYYY`, as SMBIOS stores it.
    pub bios_date: Option<String>,
    pub modules: Vec<MemoryModule>,
}

fn smbios_string(strings: &[u8], index: u8) -> Option<String> {
    if index == 0 {
        return None;
    }
    strings
        .split(|b| *b == 0)
        .nth(index as usize - 1)
        .map(|s| String::from_utf8_lossy(s).trim().to_string())
}

/// Parses raw SMBIOS data. Malformed data ends the walk early rather than failing.
pub fn parse_smbios(raw: &[u8]) -> Firmware {
    let mut firmware = Firmware::default();
    // The raw table follows an 8-byte header from GetSystemFirmwareTable.
    let Some(table) = raw.get(8..) else {
        return firmware;
    };
    let mut pos = 0;
    while let Some(&[kind, length]) = table
        .get(pos..pos + 2)
        .and_then(|s| <&[u8; 2]>::try_from(s).ok())
    {
        let length = length as usize;
        if kind == SMBIOS_END || length < 4 {
            break;
        }
        let Some(body) = table.get(pos..pos + length) else {
            break;
        };
        // Strings follow the fixed part and end with two zero bytes.
        let rest = &table[pos + length..];
        let Some(end) = rest.windows(2).position(|w| w == [0, 0]) else {
            break;
        };
        let strings = &rest[..end];
        let word = |offset: usize| {
            body.get(offset..offset + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
        };
        match kind {
            SMBIOS_TYPE_BIOS => {
                firmware.bios_version = body.get(5).and_then(|i| smbios_string(strings, *i));
                firmware.bios_date = body.get(8).and_then(|i| smbios_string(strings, *i));
            }
            SMBIOS_TYPE_MEMORY => {
                let size = word(0x0C).unwrap_or(0);
                if size != 0 && size != 0xFFFF {
                    firmware.modules.push(MemoryModule {
                        rated_mts: word(0x15).unwrap_or(0),
                        configured_mts: word(0x20).unwrap_or(0),
                        kind: body.get(0x12).copied().unwrap_or(0),
                    });
                }
            }
            _ => {}
        }
        pos += length + end + 2;
    }
    firmware
}

fn read_firmware() -> Option<Firmware> {
    let provider = FIRMWARE_TABLE_PROVIDER(u32::from_be_bytes(*b"RSMB"));
    // SAFETY: a None buffer only asks for the size.
    let size = unsafe { GetSystemFirmwareTable(provider, 0, None) };
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    // SAFETY: the buffer has the size the API asked for.
    let written = unsafe { GetSystemFirmwareTable(provider, 0, Some(&mut buffer)) };
    if written == 0 {
        return None;
    }
    buffer.truncate(written as usize);
    Some(parse_smbios(&buffer))
}

/// RAM speed and channels. Firmware has no flag for EXPO or XMP, so this is a labelled guess.
pub fn memory_findings(firmware: &Firmware) -> Vec<Finding> {
    let modules = &firmware.modules;
    let Some(first) = modules.first() else {
        return vec![finding(
            Verdict::Unknown,
            "RAM speed",
            "The firmware didn't report any memory modules.".to_string(),
            None,
        )];
    };
    let mut out = Vec::new();
    let configured = first.configured_mts;
    let rated = first.rated_mts;
    let jedec_limit = match first.kind {
        DDR5 => Some(4800),
        DDR4 => Some(3200),
        _ => None,
    };
    let speed_detail = format!("Running at {configured} MT/s. The modules report {rated} MT/s.");
    let below_rated = rated > 0 && configured > 0 && configured < rated;
    let at_base_speed = jedec_limit.is_some_and(|limit| configured > 0 && configured <= limit);
    out.push(if below_rated || at_base_speed {
        finding(
            Verdict::Attention,
            "RAM speed (best guess)",
            speed_detail,
            Some(
                "This looks like base speed. If your RAM is rated faster, turn on EXPO or XMP in \
                 the BIOS. It's one of the largest gains available, and augeo can't change it. \
                 Check the rated speed on the RAM's box, because firmware has no flag for this.",
            ),
        )
    } else {
        finding(Verdict::Good, "RAM speed (best guess)", speed_detail, None)
    });
    out.push(if modules.len() == 1 {
        finding(
            Verdict::Attention,
            "RAM channels (best guess)",
            "One memory module was found.".to_string(),
            Some("A single module usually means single-channel memory, which limits CPU-bound games. A second matching module helps."),
        )
    } else {
        finding(
            Verdict::Info,
            "RAM channels (best guess)",
            format!("{} memory modules were found.", modules.len()),
            None,
        )
    });
    out
}

fn gpu_findings(info: &nvml::GpuInfo) -> Vec<Finding> {
    let mut out = Vec::new();
    out.push(finding(
        Verdict::Info,
        "NVIDIA GPU",
        format!(
            "{}, driver {}",
            info.name.as_deref().unwrap_or("unknown"),
            info.driver.as_deref().unwrap_or("unknown")
        ),
        None,
    ));
    out.push(match info.resizable_bar_likely() {
        Some(true) => finding(
            Verdict::Good,
            "Resizable BAR (inferred)",
            format!(
                "BAR1 is {} MiB and video memory is {} MiB, so it looks on.",
                info.bar1_mib.unwrap_or(0),
                info.vram_mib.unwrap_or(0)
            ),
            None,
        ),
        Some(false) => finding(
            Verdict::Attention,
            "Resizable BAR (inferred)",
            format!(
                "BAR1 is {} MiB and video memory is {} MiB, so it looks off.",
                info.bar1_mib.unwrap_or(0),
                info.vram_mib.unwrap_or(0)
            ),
            Some("It's a BIOS setting, usually called Above 4G Decoding and Resizable BAR."),
        ),
        None => finding(
            Verdict::Unknown,
            "Resizable BAR (inferred)",
            "The driver didn't report the numbers needed.".to_string(),
            None,
        ),
    });
    if let (Some(width), Some(max_width)) = (info.pcie_width_current, info.pcie_width_max) {
        let gen_text = format!(
            "PCIe link x{width} of x{max_width}, generation {} of {}.",
            info.pcie_gen_current.unwrap_or(0),
            info.pcie_gen_max.unwrap_or(0)
        );
        out.push(if width < max_width {
            finding(
                Verdict::Attention,
                "GPU PCIe link",
                gen_text,
                Some("The link is narrower than the card supports. Check the card is in the top slot and fully seated."),
            )
        } else {
            // The generation drops at idle to save power, so only the width means anything here.
            finding(
                Verdict::Good,
                "GPU PCIe link",
                format!("{gen_text} A lower generation is normal at idle."),
                None,
            )
        });
    }
    out
}

fn display_findings() -> Vec<Finding> {
    let Some(device) = display::primary_device() else {
        return Vec::new();
    };
    let (Ok(current), Ok(best)) = (display::current_mode(&device), display::best_mode(&device))
    else {
        return Vec::new();
    };
    vec![if best.hz > current.hz {
        finding(
            Verdict::Attention,
            "Refresh rate",
            format!(
                "Running at {} Hz. This display offers {} Hz at this resolution.",
                current.hz, best.hz
            ),
            Some(
                "Higher refresh lowers latency. The display tweak can set this, with an automatic revert if the screen goes dark.",
            ),
        )
    } else {
        finding(
            Verdict::Good,
            "Refresh rate",
            format!(
                "Running at the best the display offers here: {} Hz.",
                current.hz
            ),
            None,
        )
    }]
}

fn machine_dword(path: &str, name: &str) -> Option<u32> {
    match registry::read(&RegRoot::Machine, path, name) {
        Ok(Value::Dword(v)) => Some(v),
        _ => None,
    }
}

/// A custom pagefile is fine, so this only speaks up when there's none at all.
fn pagefile_findings() -> Vec<Finding> {
    let value = registry::read(
        &RegRoot::Machine,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management",
        "PagingFiles",
    );
    let disabled = match &value {
        Ok(Value::Multi(items)) => items.iter().all(|i| i.trim().is_empty()),
        Ok(Value::Absent) => true,
        _ => false,
    };
    if disabled {
        vec![finding(
            Verdict::Attention,
            "Pagefile",
            "No pagefile is set.".to_string(),
            Some(
                "Microsoft warns that reaching the commit limit can cause freezes and crashes. Let Windows manage it, in System Properties, under Advanced, Performance.",
            ),
        )]
    } else {
        vec![finding(
            Verdict::Good,
            "Pagefile",
            "A pagefile is set.".to_string(),
            None,
        )]
    }
}

fn security_findings(build: u32) -> Vec<Finding> {
    let mut out = Vec::new();
    let hvci = machine_dword(
        r"SYSTEM\CurrentControlSet\Control\DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity",
        "Enabled",
    );
    out.push(match hvci {
        Some(1) => finding(
            Verdict::Info,
            "Memory Integrity",
            "On. It protects against kernel attacks, and costs about 6% average FPS in one 14-game test at 720p.".to_string(),
            Some("Vanguard and FACEIT may need it on. Windows updates may also turn it on. It's a level 4 tweak here, and never part of a preset."),
        ),
        Some(0) => finding(
            Verdict::Info,
            "Memory Integrity",
            "Off. Some anti-cheats, such as Vanguard and FACEIT, may refuse to run without it.".to_string(),
            None,
        ),
        _ => finding(
            Verdict::Info,
            "Memory Integrity",
            "No setting was found, so Windows' default applies.".to_string(),
            None,
        ),
    });
    let secure_boot = machine_dword(
        r"SYSTEM\CurrentControlSet\Control\SecureBoot\State",
        "UEFISecureBootEnabled",
    );
    out.push(match secure_boot {
        Some(1) => finding(Verdict::Good, "Secure Boot", "On.".to_string(), None),
        Some(_) => finding(
            Verdict::Attention,
            "Secure Boot",
            "Off.".to_string(),
            Some("Vanguard and FACEIT may refuse to run without it. It's a BIOS setting, and augeo never changes it."),
        ),
        None => finding(Verdict::Unknown, "Secure Boot", "Couldn't be read.".to_string(), None),
    });

    let services = [
        ("vgk", "Riot Vanguard"),
        ("FACEIT", "FACEIT Anti-Cheat"),
        ("EasyAntiCheat", "Easy Anti-Cheat"),
        ("EasyAntiCheat_EOS", "Easy Anti-Cheat"),
        ("BEService", "BattlEye"),
    ];
    let mut installed: Vec<&str> = services
        .iter()
        .filter(|(name, _)| {
            let path = format!(r"SYSTEM\CurrentControlSet\Services\{name}");
            matches!(
                registry::read(&RegRoot::Machine, &path, "ImagePath"),
                Ok(v) if v != Value::Absent
            )
        })
        .map(|(_, label)| *label)
        .collect();
    installed.dedup();
    out.push(finding(
        Verdict::Info,
        "Anti-cheat software",
        if installed.is_empty() {
            "None of the common ones were found.".to_string()
        } else {
            format!(
                "Found: {}. Each tweak says what is known about it.",
                installed.join(", ")
            )
        },
        None,
    ));

    out.push(if build < 22631 {
        finding(
            Verdict::Attention,
            "Windows version",
            format!("Build {build}."),
            Some("Newer Windows 11 builds include scheduler and branch prediction work that helps Ryzen CPUs in particular."),
        )
    } else {
        finding(Verdict::Good, "Windows version", format!("Build {build}."), None)
    });
    out
}

fn cpu_findings() -> Vec<Finding> {
    let name = detect::cpu_name();
    if detect::is_dual_ccd_x3d() {
        return vec![finding(
            Verdict::Info,
            "CPU layout",
            format!("{name}: an X3D chip with two core dies."),
            Some(
                "Keep the Balanced power plan, an up-to-date AMD chipset driver and Xbox Game Bar. Core parking relies on them to keep games on the right die.",
            ),
        )];
    }
    vec![finding(Verdict::Info, "CPU", name, None)]
}

fn bios_findings(firmware: &Firmware, year_now: i32) -> Vec<Finding> {
    let Some(date) = &firmware.bios_date else {
        return Vec::new();
    };
    let version = firmware.bios_version.as_deref().unwrap_or("unknown");
    let year = date.rsplit('/').next().and_then(|y| y.parse::<i32>().ok());
    let detail = format!("Version {version}, dated {date}.");
    vec![match year {
        Some(y) if year_now - y >= 2 => finding(
            Verdict::Attention,
            "BIOS age",
            detail,
            Some(
                "BIOS updates can bring memory compatibility and CPU fixes, such as the AMD fTPM stutter fix. Check your board maker's site.",
            ),
        ),
        _ => finding(Verdict::Info, "BIOS age", detail, None),
    }]
}

fn disk_findings() -> Vec<Finding> {
    let root = HSTRING::from("C:\\");
    let mut free = 0u64;
    let mut total = 0u64;
    // SAFETY: the path outlives the call, and the out values are valid places for the results.
    if unsafe { GetDiskFreeSpaceExW(&root, None, Some(&mut total), Some(&mut free)) }.is_err() {
        return Vec::new();
    }
    let free_gb = free >> 30;
    let percent = free.saturating_mul(100).checked_div(total).unwrap_or(0);
    let detail = format!("{free_gb} GB free on C: ({percent}%).");
    vec![if percent < 10 {
        finding(
            Verdict::Attention,
            "Disk space",
            detail,
            Some(
                "SSDs slow down when nearly full, and Windows needs room for updates and the pagefile.",
            ),
        )
    } else {
        finding(Verdict::Good, "Disk space", detail, None)
    }]
}

fn uptime_findings() -> Vec<Finding> {
    // SAFETY: this takes no arguments.
    let days = unsafe { GetTickCount64() } / 86_400_000;
    let detail = format!("{days} day(s) since the last full restart.");
    vec![if days >= 14 {
        finding(
            Verdict::Info,
            "Uptime",
            detail,
            Some(
                "Fast Startup means shutting down doesn't fully restart Windows. Restart before benchmarking, so runs compare fairly.",
            ),
        )
    } else {
        finding(Verdict::Info, "Uptime", detail, None)
    }]
}

/// Gathers the report. `year_now` is passed in so the age checks are testable.
pub fn report(year_now: i32) -> Vec<Finding> {
    let mut out = Vec::new();
    let firmware = read_firmware().unwrap_or_default();
    out.extend(memory_findings(&firmware));
    match nvml::query() {
        Some(info) => out.extend(gpu_findings(&info)),
        None => out.push(finding(
            Verdict::Attention,
            "NVIDIA GPU",
            "No NVIDIA driver was found.".to_string(),
            Some("augeo's GPU tweaks need one."),
        )),
    }
    out.extend(display_findings());
    out.extend(cpu_findings());
    let build = detect::windows_build().unwrap_or(0);
    out.extend(security_findings(build));
    out.extend(bios_findings(&firmware, year_now));
    out.extend(disk_findings());
    out.extend(pagefile_findings());
    out.extend(uptime_findings());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one SMBIOS structure with its strings.
    fn structure(kind: u8, length: usize, set: &[(usize, u8)], strings: &[&str]) -> Vec<u8> {
        let mut body = vec![0u8; length];
        body[0] = kind;
        body[1] = length as u8;
        for (offset, value) in set {
            body[*offset] = *value;
        }
        for s in strings {
            body.extend_from_slice(s.as_bytes());
            body.push(0);
        }
        if strings.is_empty() {
            body.push(0);
        }
        body.push(0);
        body
    }

    fn memory(rated: u16, configured: u16, size: u16, kind: u8) -> Vec<u8> {
        let [r0, r1] = rated.to_le_bytes();
        let [c0, c1] = configured.to_le_bytes();
        let [s0, s1] = size.to_le_bytes();
        structure(
            SMBIOS_TYPE_MEMORY,
            0x22,
            &[
                (0x0C, s0),
                (0x0D, s1),
                (0x12, kind),
                (0x15, r0),
                (0x16, r1),
                (0x20, c0),
                (0x21, c1),
            ],
            &[],
        )
    }

    fn table(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut raw = vec![0u8; 8];
        for part in parts {
            raw.extend_from_slice(part);
        }
        raw.extend_from_slice(&structure(SMBIOS_END, 4, &[], &[]));
        raw
    }

    #[test]
    fn smbios_finds_bios_and_populated_modules() {
        let raw = table(&[
            structure(
                SMBIOS_TYPE_BIOS,
                0x18,
                &[(5, 2), (8, 3)],
                &["Vendor", "F20", "03/15/2024"],
            ),
            memory(6000, 6000, 16384, DDR5),
            // An empty slot reports size 0, and must not count.
            memory(0, 0, 0, 0),
            memory(6000, 6000, 16384, DDR5),
        ]);
        let firmware = parse_smbios(&raw);
        assert_eq!(firmware.bios_version.as_deref(), Some("F20"));
        assert_eq!(firmware.bios_date.as_deref(), Some("03/15/2024"));
        assert_eq!(firmware.modules.len(), 2);
        assert_eq!(firmware.modules[0].configured_mts, 6000);
    }

    #[test]
    fn malformed_smbios_ends_quietly() {
        assert_eq!(parse_smbios(&[]), Firmware::default());
        assert_eq!(parse_smbios(&[0; 4]), Firmware::default());
        // A length running past the end, and no string terminator.
        assert_eq!(
            parse_smbios(&[0, 0, 0, 0, 0, 0, 0, 0, 17, 200, 1]),
            Firmware::default()
        );
        assert_eq!(
            parse_smbios(&[0, 0, 0, 0, 0, 0, 0, 0, 17, 4, 0, 0, 65]),
            Firmware::default()
        );
    }

    fn firmware(modules: &[(u16, u16, u8)]) -> Firmware {
        Firmware {
            modules: modules
                .iter()
                .map(|(rated, configured, kind)| MemoryModule {
                    rated_mts: *rated,
                    configured_mts: *configured,
                    kind: *kind,
                })
                .collect(),
            ..Firmware::default()
        }
    }

    #[test]
    fn ram_verdicts() {
        let speed = |modules: &[(u16, u16, u8)]| memory_findings(&firmware(modules))[0].verdict;
        // EXPO on.
        assert_eq!(
            speed(&[(6000, 6000, DDR5), (6000, 6000, DDR5)]),
            Verdict::Good
        );
        // Rated faster than running.
        assert_eq!(
            speed(&[(6000, 4800, DDR5), (6000, 4800, DDR5)]),
            Verdict::Attention
        );
        // Base speed, even if the modules only report their base speed.
        assert_eq!(
            speed(&[(4800, 4800, DDR5), (4800, 4800, DDR5)]),
            Verdict::Attention
        );
        assert_eq!(
            speed(&[(3200, 3200, DDR4), (3200, 3200, DDR4)]),
            Verdict::Attention
        );
        assert_eq!(
            speed(&[(3600, 3600, DDR4), (3600, 3600, DDR4)]),
            Verdict::Good
        );
        // No data at all isn't a problem to report as fact.
        assert_eq!(memory_findings(&firmware(&[]))[0].verdict, Verdict::Unknown);
    }

    #[test]
    fn a_single_module_is_flagged() {
        let one = memory_findings(&firmware(&[(6000, 6000, DDR5)]));
        assert_eq!(one[1].verdict, Verdict::Attention);
        let two = memory_findings(&firmware(&[(6000, 6000, DDR5), (6000, 6000, DDR5)]));
        assert_eq!(two[1].verdict, Verdict::Info);
    }

    #[test]
    fn gpu_verdicts() {
        let info = nvml::GpuInfo {
            bar1_mib: Some(256),
            vram_mib: Some(12282),
            pcie_width_current: Some(8),
            pcie_width_max: Some(16),
            pcie_gen_current: Some(1),
            pcie_gen_max: Some(4),
            ..nvml::GpuInfo::default()
        };
        let found = gpu_findings(&info);
        assert!(
            found
                .iter()
                .any(|f| f.title.starts_with("Resizable") && f.verdict == Verdict::Attention)
        );
        assert!(
            found
                .iter()
                .any(|f| f.title == "GPU PCIe link" && f.verdict == Verdict::Attention)
        );

        // A lower generation at idle is normal, and only the width counts.
        let idle = nvml::GpuInfo {
            pcie_width_current: Some(16),
            ..info
        };
        assert!(
            gpu_findings(&idle)
                .iter()
                .any(|f| f.title == "GPU PCIe link" && f.verdict == Verdict::Good)
        );
    }

    #[test]
    fn an_old_bios_is_flagged() {
        let with_date = |date: &str| Firmware {
            bios_date: Some(date.to_string()),
            ..Firmware::default()
        };
        assert_eq!(
            bios_findings(&with_date("01/01/2023"), 2026)[0].verdict,
            Verdict::Attention
        );
        assert_eq!(
            bios_findings(&with_date("01/01/2026"), 2026)[0].verdict,
            Verdict::Info
        );
        assert_eq!(
            bios_findings(&with_date("nonsense"), 2026)[0].verdict,
            Verdict::Info
        );
        assert!(bios_findings(&Firmware::default(), 2026).is_empty());
    }

    #[test]
    fn the_real_report_runs_and_writes_nothing() {
        let report = report(2026);
        assert!(report.len() >= 5, "{report:?}");
        assert!(report.iter().any(|f| f.title.starts_with("RAM")));
    }
}
