# Release test checklist

Every item must be signed off on real hardware or a VM before a release. CI can't cover these.

## Release blockers carried from development

- [ ] **Hard power-off during apply and undo.** Cut power to a VM mid-change, boot it, recover, then `auge!undo-all`, and confirm the registry, boot and power settings match the starting state. So far only process kills (25 kills, 15 caught mid-change) and simulated power loss in fakes have been tested. Deferred from M3 because the development PC can't run a VM: its Hyper-V feature reports enabled, but the VM service and tools are missing.
- [ ] **Boot settings on a non-English Windows.** `bcdedit` parsing has only been tested on sample English output. Check reading, removing and restoring on a translated Windows.
- [ ] **Elevated state folder on a clean machine.** Confirm `%ProgramData%\augeo` is created with protected permissions, and that a standard user can't write to it, swap it for a junction, or plant DLLs beside the exe.
- [ ] **Restore points.** Check created, skipped within 24 hours, and System Protection off, on a real machine.
- [ ] **AltGr keyboard layouts.** Type `auge!cpu#1-3,5` on a layout where `#` needs AltGr.
- [ ] **Windows 10 conhost.** Keyboard, mouse and resize. So far tested only on Windows 11.
- [ ] **The real app on both console hosts.** Input was proven with a test program, and the app's logic is tested against a simulated terminal. Nobody has yet used the finished app by keyboard, mouse and command in Windows Terminal and conhost.
- [ ] **Elevation round trip.** Apply from a read-only window, accept UAC, confirm in the new window, return, and see the state refresh. Also decline UAC. Also elevate with a different administrator's password, and confirm per-user tweaks reach the signed-in user.

- [ ] **Every tweak, for real.** Apply, read back, restart if needed, undo, and confirm the value is restored, for each of the 23 non-game tweaks and 3 game tweaks. So far the engine has only been exercised on the real system with test values. Run this in a VM where possible.
- [ ] **TRIM value.** Confirm `DisableDeleteNotification` matches `fsutil behavior query DisableDeleteNotify`. Microsoft documents the command, not the registry value.
- [ ] **HAGS value.** Confirm `HwSchMode` 2 and 1 match the Settings toggle. Only community sources give those values.
- [ ] **Windowed optimizations value.** Confirm `SwapEffectUpgradeEnable` matches the Settings toggle, which is also community-sourced.
- [ ] **Low Latency Mode.** Measure whether `PRERENDERLIMIT=1` lowers latency in a GPU-bound DX11 game. If not, it should leave the preset.
- [ ] **G-SYNC gate.** Confirm `gpu#2` is offered with G-SYNC active, and disabled without it. The development PC has it inactive and no NVIDIA Control Panel.

- [ ] **Benchmark with a real game.** So far capture was proven on desktop apps and the flow on a simulated capture. Run `auge!bench aa` and `auge!bench ab` in real games, and check the CSV columns, including `MsPCLatency` in a Reflex game, and `FrameType` with frame generation.
- [ ] **Clean baseline.** Uninstall other tuning tools before baseline runs, since their services can skew results.

## Hardware not yet covered

State these in the README until tested: Intel hybrid CPUs, AMD X3D CPUs, laptops and Optimus, real Windows 10 with an NVIDIA GPU.
