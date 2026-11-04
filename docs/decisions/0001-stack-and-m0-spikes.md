# 0001: Stack decision and M0 spike results

**Status:** Accepted. All spikes complete.

**Tested on:** Windows 11 build 26100, Ryzen 5 7600X, RTX 4070 SUPER with driver 610.88, integrated GPU active. Neither NVIDIA Control Panel nor NVIDIA App is installed on this machine.

## Decision

Use Rust with ratatui, crossterm and the `windows` crate. Neither condition for switching to Go was met: DLL planting can be stopped, and crossterm input passed on both console hosts. No Go build was made, because nothing required the comparison.

Go was the other candidate. The main reasons for Rust:

- `unsafe` can be confined to one crate, enforced by the compiler.
- `repr(C, packed(4))` expresses the NVAPI layouts directly, on a code path CI can't test.
- Exhaustive matching suits the apply and recovery state machine.

The costs are slower builds and fewer potential contributors to the engine. Tweaks are TOML data, so adding one needs no Rust.

## Spike results

| Spike | Question | Result |
|---|---|---|
| (a) Input | Do keyboard, mouse and resize work on Windows Terminal and conhost? | **Pass** |
| (b) DLL planting | Can DLLs planted beside the exe be stopped from loading? | **Pass** |
| (c) NVIDIA | Do DRS and NVML work from Rust on real hardware? | **Pass, with catalog changes** |
| (d) PresentMon | Does capture and parsing work? | **Pass** |
| (e) No network | Can the binary avoid network DLLs? | **Pass** |

### (a) Input

All 19 checks passed by hand on Windows Terminal and on classic conhost, on Windows 11 only: clicks, press and release on a button, right click, wheel, a click past column 95, resize, navigation and function keys, and typing `auge!cpu#1-3,5`.

**Not covered:**

- On the tested layout, `#` and `!` are Shift combinations, so AltGr layouts were not exercised. The parser input must treat Ctrl+Alt with a character as typed text, and that needs a unit test plus a check on an AltGr layout.
- Windows 10 conhost, which is left to the VM tests.

### (b) DLL planting

Harmless canary DLLs were planted beside a test exe, with a separate build for each linker setting.

| Build | `version`, `powrprof`, `dxgi` | `bcryptprimitives` |
|---|---|---|
| Plain | Loaded from the exe's folder, and the canary ran | System32 |
| `/DEPENDENTLOADFLAG:0x800` | System32, and the canary did not run | System32 |

- The flag works on an EXE, so it is required on the real binary. Without it, the DLLs augeo will import can be planted.
- `bcryptprimitives.dll` was not plantable on this build, even though the `KnownDLLs` registry value doesn't list it.
- This ran unelevated. M3 repeats it elevated against the real binary.

### (c) NVIDIA

| Finding | Evidence | Consequence |
|---|---|---|
| Struct layouts are correct | Setting 12,320 bytes, profile 4,116, application 20,492, all accepted by the driver | M4 still adds the C `sizeof` check |
| **DRS writes do not need admin** on this driver | Create, save, read back in a fresh session and delete all succeeded unelevated | augeo still applies through the elevated path because the journal lives there. Other non-admin software can also change these values, so drift detection matters |
| **The driver rejects community IDs** | `SetSetting` returns `-160` for Ultra Low Latency (`0x10835000`, `0x0005F543`). Those and rBAR (`0x000F00BA`, `0x000BFA21`) and Smooth Motion (`0xB0D384C0`) are all absent from `EnumAvailableSettingIds` (129 IDs) | See below |
| **The driver won't name those IDs** | `GetSettingNameFromId` returns `-160` | The planned runtime name match can't work |
| The global profile can be empty | `EnumSettings` returns end-of-list, so every setting is at driver default | "Did not exist" is the normal snapshot; undo deletes the setting |
| rBAR detection works on GeForce | NVML BAR1 16,384 MiB vs VRAM 12,282 MiB | Check report can use it. Confirmed on this one card only |
| PCIe reads Gen 1 at idle, max Gen 4 | NVML | The check must sample under load |

**Catalog consequences:**

1. Each `drs_setting` is checked against `EnumAvailableSettingIds` when a plan is built. If the driver doesn't list the ID, the tweak is disabled with the reason shown. This replaces the name match and is stronger, because it asks the driver directly.
2. Low Latency Mode uses only the public `PRERENDERLIMIT` (`0x007BA09E`). **Ultra is not offered.** Whether this value alone delivers the expected latency gain is unmeasured until M10.
3. Force rBAR and Smooth Motion stay out of scope, and the driver wouldn't accept them here anyway.

These results cover driver 610.88 only. What NVIDIA Control Panel itself writes for Low Latency Mode is unknown, because it isn't installed on this machine.

### (d) PresentMon

`PresentMon-2.6.0-x64.exe`, 980,320 bytes, valid Intel Corporation signature.

SHA-256: `B2A706BC6AD475749E3B7E3409263AA1E6906D45BDCF993F6DBC0F660188F1AF`

- Unelevated, it exits with code 6 and "access denied", as documented.
- Elevated, a 20-second capture wrote 219 rows from 4 applications.
- Unavailable values read `NA`, so the parser must not treat them as numbers.
- Columns vary by capture. `MsPCLatency` and `FrameType` were absent here, while `MsAllInputToPhotonLatency` and `MsClickToPhotonLatency` were present but `NA`. A real game capture in M7 must confirm these.

### (e) No network

Imports of a ratatui and crossterm release build: `api-ms-win-core-synch-l1-2-0`, `kernel32`, `ntdll`, `user32`. No `ws2_32`, `winhttp` or `wininet`, so the CI import check is feasible.
