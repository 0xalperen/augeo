# Architecture

How augeo is built, and why. For using it, see the [README](README.md).

## Packages

| Crate | What it does | May use `unsafe`? |
|---|---|---|
| `augeo` | The binary. Wires the real system to the engine and interface | No |
| `augeo-tui` | Screens, keyboard, mouse, benchmark flow | No |
| `augeo-command` | The `auge!` tokenizer and parser. No dependencies | No |
| `augeo-engine` | Plans, applies, verifies, reverts, recovers, detects state | No |
| `augeo-journal` | The append-only record of every change | No |
| `augeo-catalog` | Tweak schema, lint rules, the stable-number lock | No |
| `augeo-bench` | PresentMon CSV parsing, metrics, comparison statistics | No |
| `augeo-ports` | The traits between the engine and the OS | No |
| `augeo-sys` | Windows implementations of the ports | **Yes, only here** |
| `augeo-fakes` | In-memory ports with fault injection, for tests | No |
| `xtask` | Repository checks, not shipped | No |

`cargo xtask check-boundaries` fails CI if any other crate uses the `windows` crate or stops forbidding `unsafe`.

## Data flow

```
keyboard / mouse / command bar
  -> augeo-command: typed intent
  -> engine.plan: reads live values, builds the exact change list and its hash
  -> confirmation screen, using the same plan object for apply and dry-run
  -> engine.execute: re-reads under the writer lock. If the hash changed, nothing is written
  -> for each step: journal the intent, flush, write, read back, journal the result
  -> state recomputed by reading the system
```

Planning can't write, so a dry run is simply a plan that's never executed. The engine checks the confirmation itself, so a UI bug can't skip one.

## The catalog

Tweaks are TOML files in `catalog/`, embedded in the binary. A tweak declares **resources and desired values, with no revert script**. Reverting means restoring the snapshot taken before the write, so apply, verify and revert can't disagree.

Resource kinds are a closed list, so a catalog entry can't run arbitrary code: `reg_value`, `reg_kv_string`, `power_index_ac`, `power_index_dc`, `power_active_scheme`, `bcd_element`, `drs_setting`, `nic_property`, `display_mode`. Adding one means adding reviewed code.

```toml
[[tweak]]
id = "system-game-mode"     # stable, never changes
component = "system"
number = 1                  # stable, never reused
title = "Game Mode on"
summary = "..."
danger = 0                  # 0 Safe, 1 Low, 2 Medium, 3 High, 4 Expert
evidence = "Documented"     # Measured, Documented, Community, Experimental
effect = "now"              # now, app, adapter, logoff, restart
risks = ["..."]
undo_note = "..."

[[tweak.sources]]
url = "https://..."
retrieved = "2026-11-04"
claim = "..."

[[tweak.rev]]
rev = 1

[[tweak.rev.resource]]
kind = "reg_value"
scope = "user"              # machine, or the interactive user's hive
path = 'Software\Microsoft\GameBar'
name = "AutoGameModeEnabled"
type = "dword"
desired = 1
```

Optional fields: `kind = "repair"`, `conflicts`, `requires`, `security_lowering`, `preset`, `[tweak.applicability]`, `[[tweak.option]]`, `[[tweak.anticheat]]`, and `measured` on a source. On a resource: `accept_absent`, plus `desired = { by_option = {...} }`, `{ absent = true }` or `{ computed = "frame_cap_from_refresh" }`.

Unknown fields are parse errors, so a typo can't silently drop a safety field.

### Lint rules

`crates/augeo-catalog/src/lint.rs` runs as a test, so a violation fails CI:

- Every tweak has risks, sources, danger, evidence, an undo note and resources.
- `security_lowering` requires danger 4, and can never be in a preset.
- A preset tweak needs danger 0 or 1, and Measured or Documented evidence.
- Measured evidence needs a source with measured results.
- A community-known NVIDIA setting ID requires Experimental evidence.
- No two active tweaks own one resource unless both declare the conflict.
- Registry paths and names are checked against a denylist: IFEO, Run keys, services, Defender, SmartScreen, UAC, firewall, Windows Update, BitLocker, Secure Boot, TPM and more. Boot settings must be on a short allowlist.

The denylists are checked again at run time in `augeo-engine/src/expand.rs`, so a bad entry is stopped twice.

### Stable numbering

`catalog/registry.lock` is append-only: `component number id rev hash`. The hash covers a revision's resources, so formatting edits don't change it but a real change does.

`cargo xtask check-numbering` walks every committed version of the lock plus every release tag, and fails if an entry was removed or changed, a number was reused, a tweak moved, or a new number isn't next in line.

- Changing what a tweak does means adding a new revision. Old revisions stay embedded forever, so a future version can still undo an old change.
- A retired tweak keeps its number and can still be undone.
- Per-game tweaks: the number belongs to the template, and an instance is `id@exe`.

## State and undo

### The journal

`%ProgramData%\augeo\journal.log`, with an identical mirror. Each record is framed as `[u32 length][u32 CRC-32][JSON]`, and flushed to both copies before the engine continues. Files only grow, apart from cutting off damage on open. There's no compaction and no renaming, which avoids the failure modes of replacing files.

Records: `TxnBegin`, `Baseline`, `Intent`, `Done`, `TxnEnd`, `Recovery`, `Accept`. Each is stamped with boot ID, Windows build, driver version and app version, which lets the UI explain drift, such as "driver A to B since this was applied".

One transaction:

```
TxnBegin
Baseline for every resource    <- durable before the first write
Intent, write, read back, Done <- repeated for each write
TxnEnd
```

### Rules

1. **Live measurement decides state.** The journal only says what augeo expects.
2. **Snapshots are per resource**, including "did not exist", and non-standard registry values stay byte for byte.
3. **Nothing is written before its baseline and intent are on disk.**
4. **The plan shown is the plan executed.**
5. **An interrupted change is never rolled forward,** because the tweak may have caused the crash.
6. **Locations come only from the embedded catalog.** A tampered journal can supply an old value, never a location.

`System::write` must be durable before returning, and the registry adapter flushes the key. An undo stops tracking a value once it commits, so a write lost after that couldn't be detected.

### State detection

| Journal says | All at desired | Mixed | All at baseline | Other value |
|---|---|---|---|---|
| Not applied by augeo | Already set | Partial | Not applied | Not applied, value shown |
| Applied by augeo | Applied | Partial | Changed externally | Changed externally |

A failed read is Unknown, never "not applied". Overlays: in doubt, restart pending, baseline lost, older revision, rollback failed.

### Failure handling

| Case | What happens |
|---|---|
| Crash before the first write | Closed automatically, as nothing changed |
| Crash mid-change | A blocking recovery screen on next start. It measures everything and defaults to rollback. A value holding a third value is left alone |
| Step fails, or a read-back doesn't match | Earlier steps are restored. If that fails too, the baseline is kept and undo retries |
| Re-apply | Zero writes if already applied, and the baseline is untouched |
| Changed externally, then undo | Keeps the newer value by default. Overwriting is an explicit choice |
| Re-apply after an external change | Opens a new baseline epoch. Undo restores the newest, and the oldest is an explicit choice |
| Journal damage | A torn tail is cut off. Damage heals from the mirror. Damage in both copies keeps what survives, reports lost history and marks affected baselines as lost. augeo never guesses an old value. Damaged files are preserved |
| Two instances | The writer holds both files without sharing write access. The second is read-only, and the kernel frees the lock if the writer dies |
| Newer journal schema | Read-only |

## Elevation and local security

- **Manifest `asInvoker`.** Browsing and checks need no prompt.
- **Relaunching** uses `ShellExecuteExW("runas")`. Only a command string is passed, and the elevated copy parses, plans and confirms again. There's deliberately no `--yes`, so another program can't push a change past the confirmation.
- **State folder:** created with a protected DACL (SYSTEM and Administrators full, Users read), applied atomically at creation, since a new ProgramData folder otherwise lets any user create files. Every elevated start checks owner, permissions and reparse points. An untrusted folder is moved aside, and if it can't be, nothing is written.
- **DLL planting:** linked with `/DEPENDENTLOADFLAG:0x800`, and `SetDefaultDllDirectories` runs first in `main`. Without the flag, a planted `powrprof.dll` or `dxgi.dll` loads and runs, as tested in [`docs/decisions/0001`](docs/decisions/0001-stack-and-m0-spikes.md).
- **Processes:** absolute System32 paths, fixed arguments, no shell, no PowerShell.
- **Different admin account:** `user` scope resolves to `HKEY_USERS\<SID>` of whoever owns the shell window, so per-user tweaks reach the person at the desktop. The disclaimer gate uses the same SID.
- **No network:** `cargo xtask check-binary` rejects network DLL imports, and `cargo-deny` bans HTTP crates.
- **PresentMon:** copied to the state folder, hash-checked, held open against writes, and run by absolute path. It's never downloaded by augeo.

## NVIDIA integration

**NVAPI:** `nvapi64.dll` from System32, through `nvapi_QueryInterface` with the IDs in NVIDIA's public header. Struct layouts are `repr(C, packed(4))` with compile-time size checks, and `cargo xtask check-nvapi-layout` compares them with a real C compiler in CI, since no CI runner has a GPU. Each operation uses a fresh session, so a read-back proves the value reached the driver's database. The database is exported before the first change in each process.

Findings on driver 610.88 that shaped this:

- Writes don't need admin. augeo still applies through the elevated path, because the journal lives there.
- Community setting IDs for Ultra Low Latency, rBAR and Smooth Motion aren't listed by `EnumAvailableSettingIds`, and the driver rejects them. So every setting is checked against that list when a plan is built, and an unlisted one disables the tweak.

**NVML:** `nvml.dll` from System32, read-only, for the check report. A missing driver isn't an error, and NVIDIA tweaks are disabled with the reason.

## Adding a tweak

1. Find a primary source, and record what it really supports. Pick the evidence level honestly.
2. Add a `[[tweak]]` to `catalog/<component>.toml`, with the next free number.
3. Run `cargo xtask update-lock`, then `cargo test -p augeo-catalog`.
4. Apply, read back, restart if needed and undo on a real or virtual machine.

No Rust is needed, unless it needs a new resource kind. A popular tweak that doesn't hold up belongs in `catalog/myths.toml`.

## Testing

| Layer | How |
|---|---|
| Parser | Table tests for each form and error, and property tests for no panics |
| Catalog | Lint rules against the real catalog and against broken fixtures |
| Engine | Fakes with fault injection: **crash or fail at every port call** in apply, undo and recovery, then require exact restoration. Includes power loss, torn records and lost in-flight writes. A sweep only ends once a run completes without its fault firing |
| Journal | Torn tails, flipped bits, mirror healing, newer schema |
| Real system | The engine on the real registry and files, with a **real child process killed at random moments** |
| TUI | A simulated terminal, driven by keyboard, mouse and commands |
| Benchmark | CSV fixtures, including a real v2.6.0 capture's format, and statistics against published tables |
| Elevated (`--features admin-tests`) | State folder creation, squatting, boot settings, restore points. CI runners are elevated |
| GPU (`--features gpu-tests`) | Real DRS round trips on a throwaway profile. Needs a real card, so these don't run in CI |

CI on `windows-latest` runs format, clippy, all tests, the numbering, boundary, layout and binary checks, `cargo-deny` and `cargo-audit`.

Some things need real hardware and a person, and are tracked in [`docs/test-checklist.md`](docs/test-checklist.md).
