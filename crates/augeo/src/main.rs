//! augeo: wires the real system to the engine and the interface.

#![forbid(unsafe_code)]

use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use augeo_bench::{Fingerprint, SavedRun};
use augeo_catalog::{Catalog, schema::Status};
use augeo_command::{Command, Component, help_text, parse};
use augeo_engine::{
    Context, Engine, EngineError,
    applicability::{CpuVendor, Environment},
    disclaimer,
};
use augeo_journal::Stamp;
use augeo_ports::{Address, Clock, System, Value};
use augeo_sys::{
    FileStore, WindowsSystem, check, detect, display,
    elevation::{self, Elevation},
    nic, nvml, presentmon, restore,
    restore::RestorePoint,
    state_dir, tools,
};
use augeo_tui::{App, Flow, Host, POLLING_SECONDS, PollingUpdate};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

const USAGE: &str = "\
augeo: measurable, reversible gaming performance and latency tweaks.

  augeo                 Open the app.
  augeo \"auge!status\"   Print a read-only command's result and exit.
  augeo --version       Print the version.
  augeo --help          Show this.

Changing anything always happens inside the app, after showing the exact changes and asking you to
confirm. There is deliberately no option to skip that.";

/// The flag an elevated copy receives with the command to open at. It only ever leads to the
/// confirmation screen.
const RUN_FLAG: &str = "--run";

#[derive(Debug, Clone, Copy)]
struct RealClock;

impl Clock for RealClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

type RealEngine = Engine<WindowsSystem, FileStore, RealClock>;

fn local_config_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("augeo"))
}

#[derive(Debug, Default)]
struct RealHost {
    capture: Option<(presentmon::Capture, String)>,
    polling: Option<tools::PollingMeter>,
}

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Elevated, the log lives in the locked-down state folder. Otherwise in this user's own folder.
fn log_path() -> Option<PathBuf> {
    let elevated = elevation::is_elevated().unwrap_or(false);
    let dir = if elevated {
        state_dir::location().ok()?.join("logs")
    } else {
        local_config_dir()?.join("logs")
    };
    Some(dir.join("augeo.log"))
}

/// Appends one line. A log failure must never get in the way of anything, so errors are ignored.
fn write_log(line: &str) {
    let Some(path) = log_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // One old file is kept, so a log can't grow forever.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{} {line}", RealClock.now_ms());
    }
}

fn runs_dir() -> Result<PathBuf, String> {
    Ok(state_dir::location()
        .map_err(|e| e.to_string())?
        .join("runs"))
}

impl Host for RealHost {
    fn start_capture(&mut self, exe: &str, seconds: u32, id: &str) -> Result<(), String> {
        let dir = state_dir::location().map_err(|e| e.to_string())?;
        let tool = presentmon::prepare(&dir)?;
        let runs = runs_dir()?;
        std::fs::create_dir_all(&runs).map_err(|e| e.to_string())?;
        let capture = presentmon::start(&tool, exe, seconds, &runs.join(format!("{id}.csv")))?;
        self.capture = Some((capture, exe.to_string()));
        Ok(())
    }

    fn poll_capture(&mut self) -> Option<Result<String, String>> {
        let (capture, exe) = self.capture.as_mut()?;
        let result = capture.poll(exe)?;
        self.capture = None;
        Some(result)
    }

    fn cancel_capture(&mut self) {
        // Dropping a capture stops PresentMon.
        self.capture = None;
    }

    fn fingerprint(&self, exe: &str) -> Fingerprint {
        let system = WindowsSystem::new();
        Fingerprint {
            exe: exe.to_string(),
            windows_build: detect::windows_build().unwrap_or(0),
            driver: nvml::query().and_then(|g| g.driver),
            display_mode: display::primary_device()
                .and_then(|device| display::current_mode(&device).ok())
                .map(display::Mode::render),
            power_scheme: match system.read(&Address::PowerActiveScheme) {
                Ok(Value::Text(scheme)) => Some(scheme),
                _ => None,
            },
        }
    }

    fn save_run(&mut self, run: &SavedRun) -> Result<(), String> {
        let runs = runs_dir()?;
        std::fs::create_dir_all(&runs).map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(run).map_err(|e| e.to_string())?;
        std::fs::write(
            runs.join(format!("{}-{}.json", run.series, run.index)),
            text,
        )
        .map_err(|e| format!("saving the run: {e}"))
    }

    fn list_runs(&self) -> Vec<SavedRun> {
        let Some(entries) = runs_dir().ok().and_then(|dir| std::fs::read_dir(dir).ok()) else {
            return Vec::new();
        };
        let mut runs: Vec<SavedRun> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|text| serde_json::from_str(&text).ok())
            .collect();
        runs.sort_by_key(|r| (r.time_ms, r.index));
        runs
    }

    fn beep(&mut self) {
        detect::beep();
    }

    fn timer_report(&mut self, hold: bool) -> Vec<String> {
        const TARGET_MS: f64 = 0.5;
        let hold_result = tools::hold_timer_resolution(TARGET_MS, hold);
        let mut lines = Vec::new();
        match tools::timer_resolution() {
            Ok(r) => {
                lines.push(format!(
                    "Timer resolution now: {:.3} ms. This PC's range is {:.3} to {:.3} ms.",
                    r.current_ms, r.finest_ms, r.coarsest_ms
                ));
            }
            Err(e) => lines.push(e),
        }
        if let Err(e) = hold_result {
            lines.push(e);
        }
        lines.push(format!(
            "Sleep(1) really takes {:.3} ms on average.",
            tools::measure_sleep_ms(100)
        ));
        lines.push(String::new());
        lines.push(if hold {
            format!("Holding {TARGET_MS} ms for this program. It ends when you close this screen.")
        } else {
            "Holding is off. Windows applies a program's request only to that program.".to_string()
        });
        lines.push(String::new());
        lines.push(
            "What this shows: higher resolution makes sleeps more precise, but that hasn't been \
             shown to lower gaming latency, and Microsoft warns it costs power and can hurt \
             performance. For it to reach other programs on Windows 11, cpu#2 must be applied."
                .to_string(),
        );
        lines
    }

    fn start_polling_meter(&mut self) {
        self.polling = Some(tools::PollingMeter::start(POLLING_SECONDS));
    }

    fn cancel_polling_meter(&mut self) {
        // Dropping the meter closes its window.
        self.polling = None;
    }

    fn poll_polling_meter(&mut self) -> PollingUpdate {
        let Some(event) = self.polling.as_ref().and_then(tools::PollingMeter::poll) else {
            return PollingUpdate::NoChange;
        };
        let result = match event {
            tools::MeterEvent::Started => return PollingUpdate::Started,
            tools::MeterEvent::Cancelled => {
                self.polling = None;
                return PollingUpdate::Cancelled;
            }
            tools::MeterEvent::Finished(result) => result,
        };
        self.polling = None;
        let lines = match result.map(|times| tools::polling_from_times(&times)) {
            Ok(Some(polling)) => {
                let mut lines = vec![format!(
                    "Measured about {:.0} Hz, which is closest to the standard {} Hz, from {} reports.",
                    polling.measured_hz, polling.nearest_standard_hz, polling.events
                )];
                if polling.nearest_standard_hz < 1000 {
                    lines.push(String::new());
                    lines.push(
                        "NVIDIA says 125 Hz can add up to 3 ms over 1000 Hz. If your mouse supports \
                         more, set it in the mouse's own software."
                            .to_string(),
                    );
                }
                lines
            }
            Ok(None) => vec![
                "Not enough movement to tell. Try again, moving the mouse quickly.".to_string(),
            ],
            Err(e) => vec![e],
        };
        PollingUpdate::Finished(lines)
    }

    fn log(&mut self, line: &str) {
        write_log(line);
    }

    fn read_log(&self) -> Vec<String> {
        let path = log_path();
        let text = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(200);
        let mut out: Vec<String> = lines[start..].iter().map(|l| (*l).to_string()).collect();
        if out.is_empty() {
            out.push("Nothing logged yet.".to_string());
        }
        if let Some(path) = path {
            out.push(String::new());
            out.push(format!("From {}", path.display()));
        }
        out
    }

    fn now_ms(&self) -> u64 {
        RealClock.now_ms()
    }

    fn elevate(&mut self, command: &str) -> Result<bool, String> {
        let args: Vec<&str> = if command.is_empty() {
            Vec::new()
        } else {
            vec![RUN_FLAG, command]
        };
        match elevation::relaunch_elevated(&args) {
            Ok(Elevation::Finished(_)) => Ok(true),
            Ok(Elevation::Declined) => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    fn create_restore_point(&mut self) -> String {
        match restore::create("Before augeo changes") {
            Ok(RestorePoint::Created { sequence }) => {
                format!("Restore point {sequence} created.")
            }
            Ok(RestorePoint::SkippedRecentExists { existing_sequence }) => format!(
                "Windows skipped it because a restore point was made in the last 24 hours. \
                 Point {existing_sequence} is the latest."
            ),
            Ok(RestorePoint::ProtectionOff) => "No restore point: System Protection is off for \
                 this drive. It's in System Properties, under System Protection."
                .to_string(),
            Err(e) => format!("No restore point: {e}"),
        }
    }

    fn restart(&mut self) -> Result<(), String> {
        detect::request_restart().map_err(|e| e.to_string())
    }

    fn locally_accepted(&self, text_sha256: &str) -> bool {
        local_config_dir()
            .and_then(|dir| std::fs::read_to_string(dir.join("accepted-disclaimer.txt")).ok())
            .is_some_and(|text| text.lines().any(|l| l.trim() == text_sha256))
    }

    fn remember_acceptance(&mut self, text_sha256: &str) {
        if let Some(dir) = local_config_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("accepted-disclaimer.txt"), text_sha256);
        }
    }

    fn check_report(&mut self) -> Vec<String> {
        // The Gregorian average, which is close enough for "is this BIOS over two years old".
        let year = 1970 + (RealClock.now_ms() / 31_556_952_000) as i32;
        let mut lines = Vec::new();
        for finding in check::report(year) {
            lines.push(format!(
                "[{:^4}] {}: {}",
                finding.verdict.label(),
                finding.title,
                finding.detail
            ));
            if let Some(advice) = finding.advice {
                lines.push(format!("       {advice}"));
            }
        }
        lines
    }
}

fn windows_label(build: u32) -> String {
    let name = if build >= 22000 {
        "Windows 11"
    } else {
        "Windows 10"
    };
    format!("{name} {build}")
}

/// Everything about this machine the engine needs.
fn build_context(system: &WindowsSystem, gpu: Option<&nvml::GpuInfo>) -> Result<Context, String> {
    let build = detect::windows_build().map_err(|e| e.to_string())?;
    let power_scheme = match system.read(&Address::PowerActiveScheme) {
        Ok(Value::Text(scheme)) => scheme,
        // Only tweaks that touch a power plan use this, and those report their own read errors.
        _ => "00000000-0000-0000-0000-000000000000".to_string(),
    };
    let display_device = display::primary_device();
    let best_mode = display_device
        .as_deref()
        .and_then(|device| display::best_mode(device).ok());
    let vrr_active = display_device
        .as_deref()
        .is_some_and(|device| system.vrr_active(device));
    Ok(Context {
        env: Environment {
            build,
            nvidia: system.has_nvidia_driver(),
            laptop: detect::has_battery(),
            cpu_vendor: match detect::cpu_vendor() {
                detect::Vendor::Amd => CpuVendor::Amd,
                detect::Vendor::Intel => CpuVendor::Intel,
                detect::Vendor::Other => CpuVendor::Other,
            },
            dual_ccd_x3d: detect::is_dual_ccd_x3d(),
            vrr_active,
        },
        stamp: Stamp {
            boot_id: detect::boot_id(RealClock.now_ms()),
            os_build: build,
            driver: gpu.and_then(|g| g.driver.clone()),
            app: env!("CARGO_PKG_VERSION").to_string(),
        },
        user_sid: elevation::interactive_user_sid().map_err(|e| e.to_string())?,
        power_scheme,
        gpu_scheduling: detect::gpu_scheduling(),
        adapters: nic::wired_adapters(),
        display_device,
        max_display_mode: best_mode.map(display::Mode::render),
        frame_cap: best_mode.and_then(|mode| display::frame_cap_for(mode.hz)),
        disclaimer: disclaimer::current(),
    })
}

/// Opens the engine. Elevated, it's the writer and owns a locked-down state folder. Otherwise it only
/// reads whatever is there.
fn open_engine() -> Result<RealEngine, EngineError> {
    let invalid = |reason: String| EngineError::Invalid {
        id: "startup".to_string(),
        reason,
    };
    let dir = state_dir::location().map_err(|e| invalid(e.to_string()))?;
    let elevated = elevation::is_elevated().map_err(|e| invalid(e.to_string()))?;
    let (system, want_writer) = if elevated {
        state_dir::open_for_writing(&dir).map_err(|e| invalid(e.to_string()))?;
        let stamp = RealClock.now_ms();
        let backups = dir.join("drs");
        let _ = std::fs::create_dir_all(&backups);
        (
            WindowsSystem::with_drs_backup(backups.join(format!("{stamp}.bak"))),
            true,
        )
    } else {
        (WindowsSystem::new(), false)
    };
    let gpu = nvml::query();
    let context = build_context(&system, gpu.as_ref()).map_err(invalid)?;
    let catalog = Catalog::embedded().map_err(invalid)?;
    Engine::open(
        system,
        FileStore::new(&dir),
        RealClock,
        catalog,
        context,
        want_writer,
    )
}

fn environment_line() -> String {
    let gpu = nvml::query();
    let gpu_text = match &gpu {
        Some(info) => format!(
            "{} | driver {}",
            info.name.as_deref().unwrap_or("NVIDIA GPU"),
            info.driver.as_deref().unwrap_or("unknown")
        ),
        None => "No NVIDIA driver found".to_string(),
    };
    let build = detect::windows_build().map_or("Windows".to_string(), windows_label);
    format!("{gpu_text} | {build} | {} | ? for help", detect::cpu_name())
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
}

fn run_app(startup_command: Option<String>) -> Result<(), String> {
    let engine = open_engine().map_err(|e| e.to_string())?;
    let mut app = App::new(
        engine,
        Box::new(open_engine),
        RealHost::default(),
        environment_line(),
        startup_command,
    );

    // A crash must never leave the console in raw mode. The hook runs before aborting.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        write_log(&format!("crash: {info}"));
        default_hook(info);
    }));
    write_log(&format!("started, version {}", env!("CARGO_PKG_VERSION")));

    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture).map_err(|e| e.to_string())?;
    let result = (|| -> io::Result<()> {
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        loop {
            app.on_tick(Instant::now());
            terminal.draw(|frame| app.draw(frame))?;
            if event::poll(app.tick_interval())? {
                // Everything waiting is taken at once, so moving the mouse can't make drawing lag.
                loop {
                    let ev = event::read()?;
                    if app.on_event(ev, Instant::now()) == Flow::Quit {
                        return Ok(());
                    }
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
        }
    })();
    restore_terminal();
    result.map_err(|e| e.to_string())
}

/// Read-only commands work from the command line. Anything else belongs in the app.
fn run_headless(line: &str) -> Result<(), String> {
    let command = parse(line).map_err(|e| e.render(line))?;
    match command {
        Command::Help(_) => println!("{}", help_text()),
        Command::Check => {
            for line in RealHost::default().check_report() {
                println!("{line}");
            }
        }
        Command::Status(only) => {
            let dir = state_dir::location().map_err(|e| e.to_string())?;
            let system = WindowsSystem::new();
            let gpu = nvml::query();
            let context = build_context(&system, gpu.as_ref())?;
            let catalog = Catalog::embedded()?;
            let engine = Engine::open(
                system,
                FileStore::new(&dir),
                RealClock,
                catalog,
                context,
                false,
            )
            .map_err(|e| e.to_string())?;
            for tweak in engine
                .catalog()
                .tweaks
                .iter()
                .filter(|t| t.status == Status::Active)
                .filter(|t| only.is_none_or(|c| c == t.component))
                .filter(|t| t.component != Component::Game)
            {
                let state = match engine.applicability(tweak) {
                    Err(reason) => format!("doesn't apply: {reason}"),
                    Ok(()) => engine.status(&tweak.id, None).map_or_else(
                        |e| e.to_string(),
                        |s| match s.not_in_effect {
                            Some(note) => format!("not in effect: {note}"),
                            None => format!("{:?}", s.state),
                        },
                    ),
                };
                println!(
                    "{}#{:<3} {:<40} {state}",
                    tweak.component, tweak.number, tweak.title
                );
            }
        }
        _ => {
            return Err(format!(
                "'{}' needs the app, so its changes can be shown and confirmed. Run augeo with no \
                 arguments.",
                line.trim()
            ));
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    // First, before anything can load a DLL.
    if let Err(e) = augeo_sys::hardening::restrict_dll_search() {
        eprintln!("augeo can't start safely: {e}");
        return ExitCode::FAILURE;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => run_app(None),
        ["--version"] => {
            println!("augeo {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        [flag, command] if *flag == RUN_FLAG => run_app(Some((*command).to_string())),
        [line] => run_headless(line),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            // A window opened for elevation would vanish at once, so leave the message readable.
            if elevation::is_elevated().unwrap_or(false)
                && args.first().is_some_and(|a| a == RUN_FLAG)
            {
                eprintln!("Press Enter to close.");
                let _ = io::stdin().read_line(&mut String::new());
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_is_labelled_by_build() {
        assert_eq!(windows_label(19045), "Windows 10 19045");
        assert_eq!(windows_label(22000), "Windows 11 22000");
        assert_eq!(windows_label(26100), "Windows 11 26100");
    }

    #[test]
    fn commands_that_change_things_are_refused_on_the_command_line() {
        for line in [
            "auge!cpu#1",
            "auge!undo-all",
            "auge!preset recommended",
            "auge!reboot",
        ] {
            let error = run_headless(line).unwrap_err();
            assert!(error.contains("needs the app"), "{line}: {error}");
        }
    }

    #[test]
    fn help_and_bad_input_work_on_the_command_line() {
        assert!(run_headless("auge!help").is_ok());
        assert!(run_headless("auge!cpuu").unwrap_err().contains('^'));
    }

    #[test]
    fn the_real_catalog_opens_against_the_real_system() {
        let system = WindowsSystem::new();
        let context = build_context(&system, None).unwrap();
        assert!(context.env.build >= 10240);
        assert!(context.user_sid.starts_with("S-1-"));
    }
}
