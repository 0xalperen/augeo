//! Application state and event handling.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use augeo_bench::{Fingerprint, SavedRun};
use augeo_catalog::schema::{Status, Tweak, TweakKind};
use augeo_command::{
    Command, CommandError, Component, DryTarget, Purpose, Target, help_text, parse, resolve,
};
use augeo_engine::{
    Confirmation, DriftPolicy, Engine, EngineError, InDoubt, Plan, Report, TYPED_EXPERT,
    TYPED_HIGH, TweakRequest, TweakResult, TweakState, TweakStatusView, UndoPolicy, disclaimer,
};
use augeo_journal::{Decision, Op};
use augeo_ports::{Clock, Store, System};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Things outside the engine.
pub trait Host {
    /// Relaunches elevated with this command, and waits. `Ok(false)` means the user declined.
    fn elevate(&mut self, command: &str) -> Result<bool, String>;
    /// Creates a restore point and describes what actually happened.
    fn create_restore_point(&mut self) -> String;
    /// Asks Windows to restart now.
    fn restart(&mut self) -> Result<(), String>;
    /// Whether this user accepted this exact disclaimer before, without elevation.
    fn locally_accepted(&self, text_sha256: &str) -> bool;
    fn remember_acceptance(&mut self, text_sha256: &str);
    /// The read-only check report, as lines of text.
    fn check_report(&mut self) -> Vec<String>;
    /// Starts capturing a game's frames.
    fn start_capture(&mut self, exe: &str, seconds: u32, id: &str) -> Result<(), String>;
    /// `None` while capturing, otherwise the capture's CSV text, or why there isn't any.
    fn poll_capture(&mut self) -> Option<Result<String, String>>;
    fn cancel_capture(&mut self);
    /// The conditions a run happens under right now.
    fn fingerprint(&self, exe: &str) -> Fingerprint;
    fn save_run(&mut self, run: &SavedRun) -> Result<(), String>;
    fn list_runs(&self) -> Vec<SavedRun>;
    /// Signals the start or end of a run, since the user is looking at their game.
    fn beep(&mut self);
    /// The timer tool's report. With `hold`, asks for high resolution until told otherwise.
    fn timer_report(&mut self, hold: bool) -> Vec<String>;
    /// Opens the polling test's window. Testing starts when the user clicks inside it.
    fn start_polling_meter(&mut self);
    /// What the polling test has done since the last time this was asked.
    fn poll_polling_meter(&mut self) -> PollingUpdate;
    fn cancel_polling_meter(&mut self);
    /// Adds a line to the log file.
    fn log(&mut self, line: &str);
    /// The end of the log file.
    fn read_log(&self) -> Vec<String>;
    fn now_ms(&self) -> u64;
}

/// What the polling test has done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollingUpdate {
    NoChange,
    /// The user clicked, and testing began.
    Started,
    Finished(Vec<String>),
    /// The test window was closed.
    Cancelled,
}

/// How long a new display mode lasts unless the user confirms they can see it.
pub(crate) const DISPLAY_REVERT_AFTER: Duration = Duration::from_secs(15);
/// How long the polling test lasts once it starts.
pub const POLLING_SECONDS: u64 = 5;
/// How fast animations advance, and how often the screen updates while someone is looking at it.
const FRAME: Duration = Duration::from_millis(50);
/// With another window in front, such as a game, there's no reason to keep drawing quickly.
const BACKGROUND_TICK: Duration = Duration::from_millis(500);
/// A benchmark switches screens on time, without needing to look smooth.
const BENCH_TICK: Duration = Duration::from_millis(250);
/// While frames are being measured, augeo stays as close to idle as it can.
const CAPTURE_TICK: Duration = Duration::from_secs(1);

/// What the command bar can complete. Component names and tweak numbers follow the same prefix.
const COMPLETIONS: [&str; 30] = [
    "auge!help",
    "auge!check",
    "auge!status",
    "auge!myths",
    "auge!polling",
    "auge!timer",
    "auge!bench",
    "auge!game ",
    "auge!dry ",
    "auge!info ",
    "auge!undo ",
    "auge!undo-all",
    "auge!preset recommended",
    "auge!restore-point",
    "auge!elevate",
    "auge!reboot",
    "auge!log",
    "auge!quit",
    "auge!cpu",
    "auge!gpu",
    "auge!display",
    "auge!input",
    "auge!network",
    "auge!memory",
    "auge!power",
    "auge!storage",
    "auge!system",
    "auge!bench aa",
    "auge!bench ab ",
    "auge!bench start",
];

/// Opens a fresh engine, to see what an elevated copy changed.
pub type Reopen<S, T, C> = Box<dyn Fn() -> Result<Engine<S, T, C>, EngineError>>;

/// Whether to keep running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// Which list has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    Components,
    Tweaks,
    Detail,
    Bar,
}

/// The state of the confirmation screen.
#[derive(Debug, Clone)]
pub(crate) struct ConfirmState {
    pub(crate) plan: Plan,
    pub(crate) dry: bool,
    pub(crate) ticked: bool,
    pub(crate) typing: String,
    /// Instances still needing a typed word, in order.
    pub(crate) pending: Vec<(String, u8)>,
    pub(crate) typed: BTreeMap<String, String>,
    pub(crate) scroll: u16,
    pub(crate) focus_confirm: bool,
    pub(crate) drift_restore: bool,
    /// Undo only: what to redo if the drift choice changes.
    pub(crate) undo_instances: Option<Vec<String>>,
}

impl ConfirmState {
    fn new(plan: Plan, dry: bool, undo_instances: Option<Vec<String>>) -> Self {
        let pending: Vec<(String, u8)> = if plan.op == Op::Apply && !dry {
            plan.tweaks
                .iter()
                .filter(|t| t.writes() && t.danger >= 3)
                .map(|t| (t.instance.clone(), t.danger))
                .collect()
        } else {
            Vec::new()
        };
        let top = plan.tweaks.iter().map(|t| t.danger).max().unwrap_or(0);
        Self {
            plan,
            dry,
            ticked: false,
            typing: String::new(),
            pending,
            typed: BTreeMap::new(),
            scroll: 0,
            // At level 2 and up, the safe choice has focus to begin with.
            focus_confirm: top < 2,
            drift_restore: false,
            undo_instances,
        }
    }

    pub(crate) fn needs_tick(&self) -> bool {
        !self.dry
            && self.plan.op == Op::Apply
            && self.plan.tweaks.iter().any(|t| t.writes() && t.danger == 2)
    }

    pub(crate) fn current_word(&self) -> Option<&'static str> {
        self.pending.first().map(|(_, danger)| {
            if *danger >= 4 {
                TYPED_EXPERT
            } else {
                TYPED_HIGH
            }
        })
    }

    /// Whether the Confirm button is live.
    pub(crate) fn ready(&self) -> bool {
        !self.dry && self.pending.is_empty() && (!self.needs_tick() || self.ticked)
    }

    fn accept_typed_word(&mut self) -> bool {
        let Some(word) = self.current_word() else {
            return false;
        };
        if self.typing != word {
            return false;
        }
        let (instance, _) = self.pending.remove(0);
        self.typed
            .insert(instance, std::mem::take(&mut self.typing));
        true
    }
}

/// What's on screen.
#[derive(Debug, Clone)]
pub(crate) enum Screen {
    Disclaimer {
        typing: String,
    },
    Recovery {
        items: Vec<InDoubt>,
        message: Option<String>,
    },
    Main,
    Confirm(Box<ConfirmState>),
    /// Asked once before the first change.
    RestoreOffer {
        then: Box<ConfirmState>,
    },
    Elevate {
        command: String,
    },
    Text {
        title: String,
        lines: Vec<String>,
        scroll: u16,
    },
    RestartPrompt,
    /// A new display mode reverts by itself unless confirmed, in case the screen went dark.
    KeepDisplay {
        instance: String,
        deadline: Instant,
    },
    Bench(Box<crate::bench::BenchSession>),
    Timer {
        holding: bool,
        lines: Vec<String>,
    },
    /// `since` is empty until the user clicks in the test window.
    PollingMeter {
        since: Option<Instant>,
    },
}

/// A clickable thing on screen, rebuilt on every draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Hit {
    Component(usize),
    Tweak(usize),
    TweakBox(usize),
    Bar,
    Button(Button),
    Tick,
}

/// A button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Button {
    Apply,
    Dry,
    Undo,
    Elevate,
    Check,
    Bench,
    Myths,
    Status,
    Help,
    Quit,
    Confirm,
    Cancel,
    Accept,
    Decline,
    CreateRestorePoint,
    SkipRestorePoint,
    RollBack,
    Keep,
    FinishRevert,
    DriftToggle,
    Restart,
    Close,
    KeepDisplay,
    RevertDisplay,
    BenchGo,
    BenchStop,
    TimerHold,
    TimerClose,
    PollingCancel,
}

/// Regions that scroll under the mouse wheel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollArea {
    Components,
    Tweaks,
    Detail,
    Body,
}

/// The application.
pub struct App<S, T, C, H> {
    pub(crate) engine: Engine<S, T, C>,
    reopen: Reopen<S, T, C>,
    pub(crate) host: H,
    pub(crate) screen: Screen,
    pub(crate) pane: Pane,
    pub(crate) component: usize,
    pub(crate) tweak: usize,
    pub(crate) tweak_scroll: usize,
    pub(crate) detail_scroll: u16,
    pub(crate) selected: BTreeMap<String, bool>,
    pub(crate) options: BTreeMap<String, String>,
    pub(crate) statuses: BTreeMap<String, Result<TweakStatusView, String>>,
    pub(crate) current_game: Option<String>,
    pub(crate) bar: String,
    pub(crate) message: Option<String>,
    pub(crate) environment_line: String,
    restore_offered: bool,
    pub(crate) hits: Vec<(Rect, Hit)>,
    pub(crate) scrolls: Vec<(Rect, ScrollArea)>,
    pressed: Option<Hit>,
    last_click: Option<(Hit, Instant)>,
    pub(crate) wide: bool,
    pending_command: Option<String>,
    confirm_was_replaced: bool,
    /// Counts up with time, not with events, so animations run at the same speed whatever happens.
    pub(crate) frame: u64,
    started: Instant,
    /// What the mouse is over.
    pub(crate) hover: Option<Hit>,
    /// The frame the tweak list last changed on, for its fade-in.
    pub(crate) list_changed_at: u64,
    /// Whether this terminal window is the one in front.
    focused: bool,
}

impl<S, T, C, H> std::fmt::Debug for App<S, T, C, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("screen", &self.screen)
            .field("pane", &self.pane)
            .finish_non_exhaustive()
    }
}

fn is_text_char(key: &KeyEvent) -> Option<char> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    // On layouts where # needs AltGr, Windows reports Ctrl+Alt with the character, which is typing.
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    (ctrl == alt).then_some(c)
}

impl<S: System, T: Store, C: Clock, H: Host> App<S, T, C, H> {
    /// `startup_command` is run once the disclaimer and any recovery are out of the way. It only ever
    /// leads to a confirmation screen.
    pub fn new(
        engine: Engine<S, T, C>,
        reopen: Reopen<S, T, C>,
        host: H,
        environment_line: String,
        startup_command: Option<String>,
    ) -> Self {
        let mut app = Self {
            engine,
            reopen,
            host,
            screen: Screen::Main,
            pane: Pane::Tweaks,
            component: 0,
            tweak: 0,
            tweak_scroll: 0,
            detail_scroll: 0,
            selected: BTreeMap::new(),
            options: BTreeMap::new(),
            statuses: BTreeMap::new(),
            current_game: None,
            bar: String::new(),
            message: None,
            environment_line,
            restore_offered: false,
            hits: Vec::new(),
            scrolls: Vec::new(),
            pressed: None,
            last_click: None,
            wide: true,
            pending_command: startup_command,
            confirm_was_replaced: false,
            frame: 0,
            started: Instant::now(),
            hover: None,
            list_changed_at: 0,
            focused: true,
        };
        app.refresh();
        app.enter_first_screen();
        app
    }

    fn accepted(&self) -> bool {
        self.engine.disclaimer_accepted()
            || self
                .host
                .locally_accepted(&disclaimer::current().text_sha256)
    }

    /// The disclaimer comes before anything else, then recovery, then any startup command.
    fn enter_first_screen(&mut self) {
        if !self.accepted() {
            self.screen = Screen::Disclaimer {
                typing: String::new(),
            };
            return;
        }
        // An elevated copy records the acceptance the user already gave without elevation.
        if self.engine.is_writer()
            && !self.engine.disclaimer_accepted()
            && let Err(e) = self.engine.accept_disclaimer()
        {
            self.message = Some(e.to_string());
        }
        if self.open_recovery_if_needed() {
            return;
        }
        self.screen = Screen::Main;
        if let Some(command) = self.pending_command.take() {
            self.run_command(&command);
        }
    }

    fn open_recovery_if_needed(&mut self) -> bool {
        match self.engine.in_doubt() {
            Ok(items) if !items.is_empty() => {
                self.screen = Screen::Recovery {
                    items,
                    message: None,
                };
                true
            }
            Ok(_) => false,
            Err(e) => {
                self.message = Some(format!("Couldn't check for interrupted changes: {e}"));
                false
            }
        }
    }

    pub(crate) fn current_component(&self) -> Component {
        Component::ALL[self.component]
    }

    /// The tweaks in the current component, in number order, without retired ones.
    pub(crate) fn tweaks(&self) -> Vec<&Tweak> {
        let component = self.current_component();
        self.engine
            .catalog()
            .tweaks
            .iter()
            .filter(|t| t.component == component && t.status == Status::Active)
            .collect()
    }

    pub(crate) fn tweak_count(&self, component: Component) -> usize {
        self.engine
            .catalog()
            .tweaks
            .iter()
            .filter(|t| t.component == component && t.status == Status::Active)
            .count()
    }

    /// How many of a component's tweaks augeo has applied.
    pub(crate) fn applied_count(&self, component: Component) -> usize {
        self.engine
            .ledger()
            .applied
            .keys()
            .filter(|instance| {
                let id = instance.split('@').next().unwrap_or(instance);
                self.engine
                    .catalog()
                    .tweaks
                    .iter()
                    .any(|t| t.id == id && t.component == component)
            })
            .count()
    }

    /// The rest of the first command that starts with what's typed so far.
    pub(crate) fn completion(&self) -> Option<String> {
        let typed = self.bar.to_lowercase();
        COMPLETIONS
            .iter()
            .find(|c| c.starts_with(&typed) && c.len() > typed.len())
            .map(|c| c[typed.len()..].to_string())
    }

    /// How long the main loop may wait before the next tick.
    #[must_use]
    pub fn tick_interval(&self) -> Duration {
        match &self.screen {
            Screen::Bench(session) => match session.phase {
                crate::bench::BenchPhase::Capturing { .. } => CAPTURE_TICK,
                _ => BENCH_TICK,
            },
            // The test window is in front during this, and this screen's timer should still run.
            Screen::PollingMeter { .. } => FRAME,
            _ if self.focused => FRAME,
            _ => BACKGROUND_TICK,
        }
    }

    pub(crate) fn highlighted(&self) -> Option<&Tweak> {
        self.tweaks().get(self.tweak).copied()
    }

    fn exe_for(&self, tweak: &Tweak) -> Option<String> {
        (tweak.component == Component::Game)
            .then(|| self.current_game.clone())
            .flatten()
    }

    pub(crate) fn status_key(tweak: &Tweak, exe: Option<&str>) -> String {
        match exe {
            Some(exe) => format!("{}@{}", tweak.id, exe.to_lowercase()),
            None => tweak.id.clone(),
        }
    }

    pub(crate) fn status_of(&self, tweak: &Tweak) -> Option<&Result<TweakStatusView, String>> {
        let exe = self.exe_for(tweak);
        self.statuses.get(&Self::status_key(tweak, exe.as_deref()))
    }

    /// Reads real state again. Some reads are slow, such as boot settings, so this isn't per frame.
    pub(crate) fn refresh(&mut self) {
        let tweaks: Vec<Tweak> = self.engine.catalog().tweaks.clone();
        self.statuses.clear();
        for tweak in tweaks.iter().filter(|t| t.status == Status::Active) {
            let exe = self.exe_for(tweak);
            if tweak.component == Component::Game && exe.is_none() {
                continue;
            }
            let status = self
                .engine
                .status(&tweak.id, exe.as_deref())
                .map_err(|e| e.to_string());
            self.statuses
                .insert(Self::status_key(tweak, exe.as_deref()), status);
        }
    }

    fn select_component(&mut self, index: usize) {
        self.component = index.min(Component::ALL.len() - 1);
        self.tweak = 0;
        self.tweak_scroll = 0;
        self.detail_scroll = 0;
        self.list_changed_at = self.frame;
    }

    fn select_tweak(&mut self, index: usize) {
        let count = self.tweaks().len();
        self.tweak = index.min(count.saturating_sub(1));
        self.detail_scroll = 0;
    }

    fn toggle_tick(&mut self, index: usize) {
        self.select_tweak(index);
        if let Some(id) = self.highlighted().map(|t| t.id.clone()) {
            let entry = self.selected.entry(id).or_insert(false);
            *entry = !*entry;
        }
    }

    pub(crate) fn is_ticked(&self, tweak: &Tweak) -> bool {
        self.selected.get(&tweak.id).copied().unwrap_or(false)
    }

    pub(crate) fn pending_restart_count(&self) -> usize {
        self.engine.pending_restarts().len()
    }

    // ------------------------------------------------------------ building requests

    fn request_for(&self, tweak: &Tweak, exe: Option<String>) -> TweakRequest {
        TweakRequest {
            id: tweak.id.clone(),
            option: self.options.get(&tweak.id).cloned(),
            exe,
        }
    }

    /// Ticked tweaks in this component, or else the highlighted one.
    fn chosen(&self) -> Vec<Tweak> {
        let tweaks = self.tweaks();
        let ticked: Vec<Tweak> = tweaks
            .iter()
            .filter(|t| self.is_ticked(t))
            .map(|t| (*t).clone())
            .collect();
        if !ticked.is_empty() {
            return ticked;
        }
        self.highlighted().cloned().into_iter().collect()
    }

    fn command_for(&self, tweaks: &[Tweak], verb: &str) -> Option<String> {
        let first = tweaks.first()?;
        let numbers: Vec<String> = tweaks.iter().map(|t| t.number.to_string()).collect();
        let suffix = self
            .exe_for(first)
            .map(|exe| format!("@{exe}"))
            .unwrap_or_default();
        Some(format!(
            "auge!{verb}{}#{}{suffix}",
            first.component,
            numbers.join(",")
        ))
    }

    fn apply_chosen(&mut self, dry: bool) {
        let tweaks = self.chosen();
        let verb = if dry { "dry " } else { "" };
        let command = self.command_for(&tweaks, verb);
        let requests: Vec<TweakRequest> = tweaks
            .iter()
            .map(|t| self.request_for(t, self.exe_for(t)))
            .collect();
        if requests.is_empty() {
            self.message = Some("Nothing to apply here.".to_string());
            return;
        }
        self.start_apply(&requests, dry, command);
    }

    fn start_apply(&mut self, requests: &[TweakRequest], dry: bool, command: Option<String>) {
        if !dry
            && !self.engine.is_writer()
            && let Some(command) = command
        {
            self.screen = Screen::Elevate { command };
            return;
        }
        match self.engine.plan_apply(requests) {
            Ok(plan) => self.show_plan(plan, dry, None),
            Err(e) => self.message = Some(e.to_string()),
        }
    }

    fn show_plan(&mut self, plan: Plan, dry: bool, undo_instances: Option<Vec<String>>) {
        if plan.tweaks.is_empty() {
            self.message = Some("Nothing to do: none of those are applied by augeo.".to_string());
            return;
        }
        let state = ConfirmState::new(plan, dry, undo_instances);
        // Offered once, before the first real change.
        let first_change = state.plan.op == Op::Apply
            && !dry
            && self.engine.ledger().applied.is_empty()
            && state
                .plan
                .tweaks
                .iter()
                .any(augeo_engine::TweakPlan::writes);
        if first_change && !self.restore_offered {
            self.restore_offered = true;
            self.screen = Screen::RestoreOffer {
                then: Box::new(state),
            };
        } else {
            self.screen = Screen::Confirm(Box::new(state));
        }
    }

    fn undo_chosen(&mut self) {
        let tweaks = self.chosen();
        let instances: Vec<String> = tweaks
            .iter()
            .map(|t| Self::status_key(t, self.exe_for(t).as_deref()))
            .collect();
        let command = self.command_for(&tweaks, "undo ");
        self.start_undo(instances, command);
    }

    fn start_undo(&mut self, instances: Vec<String>, command: Option<String>) {
        if !self.engine.is_writer()
            && let Some(command) = command
        {
            self.screen = Screen::Elevate { command };
            return;
        }
        match self.engine.plan_undo(&instances, UndoPolicy::default()) {
            Ok(plan) => self.show_plan(plan, false, Some(instances)),
            Err(e) => self.message = Some(e.to_string()),
        }
    }

    fn start_undo_all(&mut self) {
        if !self.engine.is_writer() {
            self.screen = Screen::Elevate {
                command: "auge!undo-all".to_string(),
            };
            return;
        }
        match self.engine.plan_undo_all(UndoPolicy::default()) {
            Ok(plan) => {
                let instances = plan.tweaks.iter().map(|t| t.instance.clone()).collect();
                self.show_plan(plan, false, Some(instances));
            }
            Err(e) => self.message = Some(e.to_string()),
        }
    }

    fn toggle_drift(&mut self, state: &mut ConfirmState) {
        let Some(instances) = state.undo_instances.clone() else {
            return;
        };
        state.drift_restore = !state.drift_restore;
        let policy = UndoPolicy {
            drift: if state.drift_restore {
                DriftPolicy::RestoreBaseline
            } else {
                DriftPolicy::Keep
            },
            ..UndoPolicy::default()
        };
        match self.engine.plan_undo(&instances, policy) {
            Ok(plan) => state.plan = plan,
            Err(e) => self.message = Some(e.to_string()),
        }
    }

    fn execute(&mut self, state: ConfirmState) {
        let confirmation = Confirmation {
            risks_ticked: state.ticked,
            typed: state.typed.clone(),
        };
        match self.engine.execute(&state.plan, &confirmation) {
            Ok(report) => {
                self.refresh();
                self.selected.clear();
                self.show_report(&report, state.plan.op);
                // Whatever else happened, a changed display mode must be confirmed or it goes back.
                let changed_display = state.plan.tweaks.iter().find(|t| {
                    state.plan.op == Op::Apply
                        && t.steps.iter().any(|s| {
                            s.write && matches!(s.address, augeo_ports::Address::DisplayMode { .. })
                        })
                        && report
                            .results
                            .iter()
                            .any(|(i, r)| *i == t.instance && *r == TweakResult::Done)
                });
                if let Some(tweak) = changed_display {
                    self.screen = Screen::KeepDisplay {
                        instance: tweak.instance.clone(),
                        deadline: Instant::now() + DISPLAY_REVERT_AFTER,
                    };
                }
            }
            Err(EngineError::PlanChanged(fresh)) => {
                self.message = Some(
                    "The system changed since this was shown. Here are the current values."
                        .to_string(),
                );
                self.screen = Screen::Confirm(Box::new(ConfirmState::new(
                    *fresh,
                    false,
                    state.undo_instances,
                )));
            }
            Err(e) => {
                self.refresh();
                self.screen = Screen::Main;
                self.message = Some(e.to_string());
                let _ = self.open_recovery_if_needed();
            }
        }
    }

    fn show_report(&mut self, report: &Report, op: Op) {
        let mut lines = Vec::new();
        let verb = if op == Op::Apply { "Applied" } else { "Undone" };
        for (instance, result) in &report.results {
            self.host.log(&format!("{op:?} {instance}: {result:?}"));
        }
        for (instance, result) in &report.results {
            lines.push(match result {
                TweakResult::Done => format!("{verb} and verified: {instance}"),
                TweakResult::NoChange => format!("No change was needed: {instance}"),
                TweakResult::RolledBack { error } => {
                    format!("Failed, and everything was restored: {instance}\n  {error}")
                }
                TweakResult::RollbackFailed { error } => format!(
                    "Failed, and restoring also failed: {instance}\n  {error}\n  \
                     Run undo again to retry. The original values are kept."
                ),
                TweakResult::RevertFailed { error } => {
                    format!(
                        "Undo stopped partway: {instance}\n  {error}\n  It's safe to run again."
                    )
                }
            });
        }
        for instance in &report.not_attempted {
            lines.push(format!(
                "Not attempted, because an earlier one failed: {instance}"
            ));
        }
        let pending = self.pending_restart_count();
        if pending > 0 {
            lines.push(String::new());
            lines.push(format!(
                "{pending} applied change(s) need a restart to take effect. \
                 augeo never restarts on its own: use auge!reboot when you're ready."
            ));
        }
        self.screen = Screen::Text {
            title: "Result".to_string(),
            lines,
            scroll: 0,
        };
    }

    fn show_text(&mut self, title: &str, body: String) {
        self.screen = Screen::Text {
            title: title.to_string(),
            lines: body.lines().map(str::to_string).collect(),
            scroll: 0,
        };
    }

    fn show_status(&mut self, only: Option<Component>) {
        self.refresh();
        let mut lines = Vec::new();
        for tweak in self.engine.catalog().tweaks.iter() {
            if tweak.status != Status::Active || only.is_some_and(|c| c != tweak.component) {
                continue;
            }
            let label = self
                .status_of(tweak)
                .map_or("pick a game first".to_string(), crate::view::state_label);
            lines.push(format!(
                "{}#{:<3} {:<14} {}",
                tweak.component, tweak.number, label, tweak.title
            ));
        }
        if lines.is_empty() {
            lines.push("No tweaks here yet.".to_string());
        }
        self.show_text("Status", lines.join("\n"));
    }

    fn show_check(&mut self) {
        self.refresh();
        let mut lines = self.host.check_report();
        // The audit of harmful earlier tweaks: repairs that have something to fix on this PC.
        let repairs: Vec<String> = self
            .engine
            .catalog()
            .tweaks
            .iter()
            .filter(|t| Self::is_repair(t) && t.status == Status::Active)
            .filter(|t| self.applicability(t).is_ok())
            .filter(|t| {
                matches!(
                    self.status_of(t),
                    Some(Ok(view)) if matches!(view.state, TweakState::NotApplied | TweakState::Partial)
                )
            })
            .map(|t| format!("  {}#{}  {}", t.component, t.number, t.title))
            .collect();
        lines.push(String::new());
        if repairs.is_empty() {
            lines.push(
                "[ OK ] Earlier tweaks: nothing harmful found that augeo can read.".to_string(),
            );
        } else {
            lines.push("[LOOK] Earlier tweaks: these repairs have something to fix.".to_string());
            lines.extend(repairs);
        }
        if !self.engine.is_writer() {
            lines.push("       Boot settings can only be read when elevated.".to_string());
        }
        self.show_text(
            "System check (nothing here changes anything)",
            lines.join("\n"),
        );
    }

    fn show_myths(&mut self) {
        let mut lines = vec![
            "Popular tweaks augeo deliberately doesn't ship, and why.".to_string(),
            String::new(),
        ];
        for myth in &self.engine.catalog().myths {
            lines.push(myth.name.clone());
            lines.push(format!("  {}", myth.reason));
            if let Some(source) = &myth.source {
                lines.push(format!("  {source}"));
            }
            lines.push(String::new());
        }
        self.show_text("Myths", lines.join("\n"));
    }

    fn describe(&self, tweak: &Tweak) -> Vec<String> {
        crate::view::detail_lines(tweak, self.status_of(tweak), self.options.get(&tweak.id))
    }

    // ------------------------------------------------------------ commands

    fn command_error(&mut self, input: &str, error: &CommandError) {
        self.message = Some(error.render(input));
    }

    fn resolved_requests(
        &mut self,
        input: &str,
        target: &Target,
        purpose: Purpose,
    ) -> Option<(Vec<Tweak>, Option<String>)> {
        let view = self.engine.catalog().view(self.current_game.clone());
        let resolved = match resolve(target, &view, purpose) {
            Ok(r) => r,
            Err(e) => {
                self.command_error(input, &e);
                return None;
            }
        };
        let tweaks: Vec<Tweak> = resolved
            .numbers
            .iter()
            .filter_map(|n| self.engine.catalog().find(resolved.component, *n).cloned())
            .collect();
        if !resolved.skipped.is_empty() {
            let skipped: Vec<String> = resolved
                .skipped
                .iter()
                .map(|(n, reason)| format!("#{n} ({reason})"))
                .collect();
            self.message = Some(format!("Skipped retired: {}", skipped.join(", ")));
        }
        // Moves the view to what the command is about.
        if let Some(index) = Component::ALL.iter().position(|c| *c == resolved.component) {
            self.select_component(index);
        }
        Some((tweaks, resolved.exe))
    }

    /// Runs one line from the command bar. It can only ever lead to a confirmation screen.
    pub(crate) fn run_command(&mut self, input: &str) {
        self.message = None;
        let command = match parse(input) {
            Ok(c) => c,
            Err(e) => {
                self.command_error(input, &e);
                return;
            }
        };
        match command {
            Command::Help(_) => self.show_text("Help", help_text()),
            Command::Quit => self.message = Some("Press q to quit.".to_string()),
            Command::Open(component) => {
                if let Some(i) = Component::ALL.iter().position(|c| *c == component) {
                    self.select_component(i);
                    self.pane = Pane::Tweaks;
                }
            }
            Command::Status(only) => self.show_status(only),
            Command::Apply(target) => {
                if let Some((tweaks, exe)) = self.resolved_requests(input, &target, Purpose::Apply)
                {
                    let requests: Vec<_> = tweaks
                        .iter()
                        .map(|t| self.request_for(t, exe.clone()))
                        .collect();
                    self.start_apply(&requests, false, Some(input.trim().to_string()));
                }
            }
            Command::Dry(DryTarget::Tweaks(target)) => {
                if let Some((tweaks, exe)) = self.resolved_requests(input, &target, Purpose::Apply)
                {
                    let requests: Vec<_> = tweaks
                        .iter()
                        .map(|t| self.request_for(t, exe.clone()))
                        .collect();
                    self.start_apply(&requests, true, None);
                }
            }
            Command::Dry(DryTarget::Preset(name)) | Command::Preset(name)
                if name != "recommended" =>
            {
                self.message = Some(format!("No preset called '{name}'. Try: recommended"));
            }
            Command::Dry(DryTarget::Preset(_)) => {
                let requests = self.engine.preset_requests();
                self.start_apply(&requests, true, None);
            }
            Command::Preset(_) => {
                let requests = self.engine.preset_requests();
                self.start_apply(&requests, false, Some(input.trim().to_string()));
            }
            Command::Undo(target) => {
                if let Some((tweaks, exe)) =
                    self.resolved_requests(input, &target, Purpose::UndoOrInspect)
                {
                    let instances = tweaks
                        .iter()
                        .map(|t| Self::status_key(t, exe.as_deref()))
                        .collect();
                    self.start_undo(instances, Some(input.trim().to_string()));
                }
            }
            Command::UndoAll => self.start_undo_all(),
            Command::Info(target) => {
                if let Some((tweaks, _)) =
                    self.resolved_requests(input, &target, Purpose::UndoOrInspect)
                {
                    let lines: Vec<String> = tweaks
                        .iter()
                        .flat_map(|t| {
                            let mut lines =
                                vec![format!("{} ({}#{})", t.title, t.component, t.number)];
                            lines.extend(self.describe(t));
                            lines.push(String::new());
                            lines
                        })
                        .collect();
                    self.show_text("Details", lines.join("\n"));
                }
            }
            Command::SelectGame(exe) => {
                self.message = Some(format!("Selected {exe}. Game tweaks now apply to it."));
                self.current_game = Some(exe);
                self.refresh();
                if let Some(i) = Component::ALL.iter().position(|c| *c == Component::Game) {
                    self.select_component(i);
                }
            }
            Command::Elevate => {
                if self.engine.is_writer() {
                    self.message = Some("Already elevated.".to_string());
                } else {
                    self.screen = Screen::Elevate {
                        command: String::new(),
                    };
                }
            }
            Command::Reboot => self.screen = Screen::RestartPrompt,
            Command::RestorePoint => {
                let text = self.host.create_restore_point();
                self.message = Some(text);
            }
            Command::Check => self.show_check(),
            Command::Myths => self.show_myths(),
            Command::Log => {
                let lines = self.host.read_log();
                self.screen = Screen::Text {
                    title: "Log".to_string(),
                    lines,
                    scroll: 0,
                };
            }
            Command::Bench(args) => self.run_bench_command(input, &args),
            Command::Timer => {
                let lines = self.host.timer_report(false);
                self.screen = Screen::Timer {
                    holding: false,
                    lines,
                };
            }
            Command::Polling => {
                self.host.start_polling_meter();
                self.screen = Screen::PollingMeter { since: None };
            }
        }
    }

    // ------------------------------------------------------------ actions

    fn press_button(&mut self, button: Button) -> Flow {
        match button {
            Button::Apply => self.apply_chosen(false),
            Button::Dry => self.apply_chosen(true),
            Button::Undo => self.undo_chosen(),
            Button::Elevate => {
                self.screen = Screen::Elevate {
                    command: String::new(),
                };
            }
            Button::Check => self.run_command("auge!check"),
            Button::Bench => self.run_command("auge!bench"),
            Button::Myths => self.run_command("auge!myths"),
            Button::Status => self.show_status(None),
            Button::Help => self.show_text("Help", help_text()),
            Button::Quit => return Flow::Quit,
            Button::Confirm => self.confirm_pressed(),
            Button::Cancel | Button::Close => {
                self.screen = Screen::Main;
            }
            Button::Accept => self.accept_disclaimer(),
            Button::Decline => return Flow::Quit,
            Button::CreateRestorePoint => {
                let text = self.host.create_restore_point();
                self.message = Some(text);
                self.leave_restore_offer();
            }
            Button::SkipRestorePoint => self.leave_restore_offer(),
            Button::RollBack => self.resolve_first(Decision::RollBack),
            Button::Keep => self.resolve_first(Decision::Keep),
            Button::FinishRevert => self.resolve_first(Decision::FinishRevert),
            Button::DriftToggle => {
                if let Screen::Confirm(state) = &self.screen {
                    let mut state = state.clone();
                    self.toggle_drift(&mut state);
                    self.screen = Screen::Confirm(state);
                }
            }
            Button::TimerHold => {
                if let Screen::Timer { holding, .. } = self.screen {
                    let holding = !holding;
                    let lines = self.host.timer_report(holding);
                    self.screen = Screen::Timer { holding, lines };
                }
            }
            Button::TimerClose => {
                if let Screen::Timer { holding: true, .. } = self.screen {
                    let _ = self.host.timer_report(false);
                }
                self.screen = Screen::Main;
            }
            Button::PollingCancel => {
                self.host.cancel_polling_meter();
                self.screen = Screen::Main;
                self.message = Some("The polling test was cancelled.".to_string());
            }
            Button::BenchGo => self.bench_start_countdown(Instant::now()),
            Button::BenchStop => self.bench_cancel(),
            Button::KeepDisplay => {
                self.screen = Screen::Main;
                self.message = Some("Keeping the new display mode.".to_string());
            }
            Button::RevertDisplay => {
                if let Screen::KeepDisplay { instance, .. } = &self.screen {
                    let instance = instance.clone();
                    self.revert_display(&instance, "The display mode was put back.");
                }
            }
            Button::Restart => match self.host.restart() {
                Ok(()) => return Flow::Quit,
                Err(e) => {
                    self.screen = Screen::Main;
                    self.message = Some(e);
                }
            },
        }
        Flow::Continue
    }

    /// Lets time-based things happen. Call this regularly.
    pub fn on_tick(&mut self, now: Instant) {
        self.frame =
            (now.saturating_duration_since(self.started).as_millis() / FRAME.as_millis()) as u64;
        self.bench_tick(now);
        if matches!(self.screen, Screen::PollingMeter { .. }) {
            match self.host.poll_polling_meter() {
                PollingUpdate::NoChange => {}
                PollingUpdate::Started => {
                    self.screen = Screen::PollingMeter { since: Some(now) };
                }
                PollingUpdate::Finished(lines) => {
                    self.screen = Screen::Text {
                        title: "Mouse polling rate".to_string(),
                        lines,
                        scroll: 0,
                    };
                }
                PollingUpdate::Cancelled => {
                    self.screen = Screen::Main;
                    self.message = Some("The polling test was cancelled.".to_string());
                }
            }
        }
        if let Screen::KeepDisplay { instance, deadline } = &self.screen
            && now >= *deadline
        {
            let instance = instance.clone();
            self.revert_display(
                &instance,
                "No answer in time, so the display mode was put back.",
            );
        }
    }

    fn revert_display(&mut self, instance: &str, note: &str) {
        let outcome = self
            .engine
            .plan_undo(&[instance.to_string()], UndoPolicy::default())
            .and_then(|plan| self.engine.execute(&plan, &Confirmation::default()));
        self.refresh();
        self.screen = Screen::Main;
        self.message = Some(match outcome {
            Ok(_) => note.to_string(),
            Err(e) => format!("Putting the display mode back failed: {e}"),
        });
    }

    fn leave_restore_offer(&mut self) {
        if let Screen::RestoreOffer { then } = &self.screen {
            self.screen = Screen::Confirm(then.clone());
        }
    }

    fn confirm_pressed(&mut self) {
        let Screen::Confirm(state) = &self.screen else {
            return;
        };
        if state.dry {
            self.screen = Screen::Main;
            return;
        }
        if !state.ready() {
            return;
        }
        let state = (**state).clone();
        self.execute(state);
    }

    fn accept_disclaimer(&mut self) {
        let hash = disclaimer::current().text_sha256;
        self.host.remember_acceptance(&hash);
        self.enter_first_screen();
    }

    fn resolve_first(&mut self, decision: Decision) {
        let Screen::Recovery { items, .. } = &self.screen else {
            return;
        };
        let Some(item) = items.first() else {
            return;
        };
        let txn = item.txn;
        if !self.engine.is_writer() {
            self.screen = Screen::Elevate {
                command: String::new(),
            };
            return;
        }
        let note = match self.engine.resolve(txn, decision) {
            Ok(report) => {
                let mut parts = Vec::new();
                if !report.left_alone.is_empty() {
                    parts.push(format!(
                        "Left alone, because something else changed them: {}",
                        report
                            .left_alone
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
                parts.extend(report.errors);
                (!parts.is_empty()).then(|| parts.join("\n"))
            }
            Err(e) => Some(e.to_string()),
        };
        self.refresh();
        match self.engine.in_doubt() {
            Ok(items) if !items.is_empty() => {
                self.screen = Screen::Recovery {
                    items,
                    message: note,
                };
            }
            _ => {
                self.message = note;
                self.enter_first_screen();
            }
        }
    }

    fn do_elevate(&mut self, command: String) {
        let result = self.host.elevate(&command);
        self.screen = Screen::Main;
        match result {
            Ok(true) => match (self.reopen)() {
                Ok(engine) => {
                    self.engine = engine;
                    self.refresh();
                    self.message = Some("Back from the elevated window. State refreshed.".into());
                    let _ = self.open_recovery_if_needed();
                }
                Err(e) => self.message = Some(e.to_string()),
            },
            Ok(false) => {
                self.message = Some("Elevation was declined. Nothing was changed.".to_string());
            }
            Err(e) => self.message = Some(e),
        }
    }

    // ------------------------------------------------------------ input

    /// Handles one terminal event.
    pub fn on_event(&mut self, event: Event, now: Instant) -> Flow {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.focused = true;
                self.on_key(key)
            }
            Event::Mouse(mouse) => {
                self.focused = true;
                self.on_mouse(mouse, now)
            }
            Event::FocusGained => {
                self.focused = true;
                Flow::Continue
            }
            Event::FocusLost => {
                self.focused = false;
                self.hover = None;
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Flow {
        let screen = self.screen.clone();
        match screen {
            Screen::Disclaimer { mut typing } => {
                match key.code {
                    KeyCode::Esc => return Flow::Quit,
                    KeyCode::Enter if typing.trim().eq_ignore_ascii_case("accept") => {
                        self.accept_disclaimer();
                        return Flow::Continue;
                    }
                    KeyCode::Backspace => {
                        typing.pop();
                    }
                    _ => {
                        if let Some(c) = is_text_char(&key) {
                            typing.push(c);
                        }
                    }
                }
                self.screen = Screen::Disclaimer { typing };
                Flow::Continue
            }
            Screen::Recovery { items, .. } => {
                match key.code {
                    KeyCode::Enter => {
                        if let Some(item) = items.first() {
                            self.resolve_first(item.default_decision());
                        }
                    }
                    KeyCode::Char('k' | 'K')
                        if items.first().is_some_and(|i| i.op == Op::Apply) =>
                    {
                        self.resolve_first(Decision::Keep);
                    }
                    _ => {}
                }
                Flow::Continue
            }
            Screen::Main => self.on_main_key(key),
            Screen::Confirm(mut state) => {
                self.on_confirm_key(key, &mut state);
                // Executing or cancelling replaces the screen, so only store edits otherwise.
                if matches!(self.screen, Screen::Confirm(_)) && !self.confirm_was_replaced {
                    self.screen = Screen::Confirm(state);
                }
                self.confirm_was_replaced = false;
                Flow::Continue
            }
            Screen::RestoreOffer { .. } => {
                match key.code {
                    KeyCode::Enter | KeyCode::Char('c' | 'C') => {
                        return self.press_button(Button::CreateRestorePoint);
                    }
                    KeyCode::Esc | KeyCode::Char('s' | 'S') => self.leave_restore_offer(),
                    _ => {}
                }
                Flow::Continue
            }
            Screen::Elevate { command } => {
                match key.code {
                    KeyCode::Enter => self.do_elevate(command),
                    KeyCode::Esc => self.screen = Screen::Main,
                    _ => {}
                }
                Flow::Continue
            }
            Screen::Text {
                title,
                lines,
                scroll,
            } => {
                let scroll = match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                        self.screen = Screen::Main;
                        return Flow::Continue;
                    }
                    KeyCode::Down => scroll.saturating_add(1),
                    KeyCode::Up => scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll.saturating_add(10),
                    KeyCode::PageUp => scroll.saturating_sub(10),
                    KeyCode::Home => 0,
                    _ => scroll,
                };
                self.screen = Screen::Text {
                    title,
                    lines,
                    scroll,
                };
                Flow::Continue
            }
            Screen::Timer { .. } => {
                match key.code {
                    KeyCode::Char('h' | 'H') => return self.press_button(Button::TimerHold),
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                        return self.press_button(Button::TimerClose);
                    }
                    _ => {}
                }
                Flow::Continue
            }
            Screen::PollingMeter { .. } => {
                if key.code == KeyCode::Esc {
                    return self.press_button(Button::PollingCancel);
                }
                Flow::Continue
            }
            Screen::Bench(_) => {
                match key.code {
                    KeyCode::Enter => return self.press_button(Button::BenchGo),
                    KeyCode::Esc => return self.press_button(Button::BenchStop),
                    _ => {}
                }
                Flow::Continue
            }
            Screen::KeepDisplay { .. } => {
                match key.code {
                    KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                        return self.press_button(Button::KeepDisplay);
                    }
                    KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                        return self.press_button(Button::RevertDisplay);
                    }
                    _ => {}
                }
                Flow::Continue
            }
            Screen::RestartPrompt => {
                match key.code {
                    KeyCode::Char('y' | 'Y') => return self.press_button(Button::Restart),
                    KeyCode::Esc | KeyCode::Char('n' | 'N') | KeyCode::Enter => {
                        self.screen = Screen::Main;
                    }
                    _ => {}
                }
                Flow::Continue
            }
        }
    }

    fn on_main_key(&mut self, key: KeyEvent) -> Flow {
        if self.pane == Pane::Bar {
            match key.code {
                KeyCode::Enter => {
                    let line = std::mem::take(&mut self.bar);
                    if !line.trim().is_empty() {
                        // Quit is the one command that ends the loop, so it's handled here.
                        if matches!(parse(&line), Ok(Command::Quit)) {
                            return Flow::Quit;
                        }
                        self.run_command(&line);
                    }
                    self.pane = Pane::Tweaks;
                }
                KeyCode::Esc => {
                    self.bar.clear();
                    self.pane = Pane::Tweaks;
                }
                KeyCode::Backspace => {
                    self.bar.pop();
                }
                KeyCode::Tab => {
                    if let Some(rest) = self.completion() {
                        self.bar.push_str(&rest);
                    }
                }
                _ => {
                    if let Some(c) = is_text_char(&key) {
                        self.bar.push(c);
                    }
                }
            }
            return Flow::Continue;
        }

        self.message = None;
        match key.code {
            KeyCode::Char('q') => return Flow::Quit,
            KeyCode::Char('/' | ':') => self.pane = Pane::Bar,
            KeyCode::Char('?') | KeyCode::F(1) => self.show_text("Help", help_text()),
            KeyCode::Tab | KeyCode::Right => {
                self.pane = match self.pane {
                    Pane::Components => Pane::Tweaks,
                    Pane::Tweaks => Pane::Detail,
                    Pane::Detail | Pane::Bar => Pane::Components,
                };
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.pane = match self.pane {
                    Pane::Components => Pane::Detail,
                    Pane::Tweaks => Pane::Components,
                    Pane::Detail | Pane::Bar => Pane::Tweaks,
                };
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home => self.move_selection(-1000),
            KeyCode::End => self.move_selection(1000),
            KeyCode::Char(' ') => {
                if self.pane == Pane::Tweaks {
                    self.toggle_tick(self.tweak);
                }
            }
            KeyCode::Enter => match self.pane {
                Pane::Components => self.pane = Pane::Tweaks,
                Pane::Tweaks | Pane::Detail => {
                    if self.wide {
                        self.apply_chosen(false);
                    } else if let Some(tweak) = self.highlighted().cloned() {
                        // On a narrow window, details open first instead of being side by side.
                        let body = self.describe(&tweak).join("\n");
                        self.show_text(&tweak.title, body);
                    }
                }
                Pane::Bar => {}
            },
            KeyCode::Char('a') => self.apply_chosen(false),
            KeyCode::Char('d') => self.apply_chosen(true),
            KeyCode::Char('u') => self.undo_chosen(),
            KeyCode::Char('r') => {
                self.refresh();
                self.message = Some("State read again.".to_string());
            }
            KeyCode::Char('o') => self.cycle_option(),
            KeyCode::Char('e') if !self.engine.is_writer() => {
                self.screen = Screen::Elevate {
                    command: String::new(),
                };
            }
            _ => {}
        }
        Flow::Continue
    }

    fn cycle_option(&mut self) {
        let Some(tweak) = self.highlighted().cloned() else {
            return;
        };
        if tweak.option.is_empty() {
            return;
        }
        let current = self.options.get(&tweak.id).cloned().or_else(|| {
            tweak
                .option
                .iter()
                .find(|o| o.recommended)
                .map(|o| o.key.clone())
        });
        let index = tweak
            .option
            .iter()
            .position(|o| Some(&o.key) == current.as_ref())
            .unwrap_or(0);
        let next = &tweak.option[(index + 1) % tweak.option.len()];
        self.message = Some(format!("Option: {}", next.name));
        self.options.insert(tweak.id.clone(), next.key.clone());
    }

    fn move_selection(&mut self, delta: i32) {
        let moved = |current: usize, count: usize| -> usize {
            let last = count.saturating_sub(1) as i32;
            (current as i32 + delta).clamp(0, last) as usize
        };
        match self.pane {
            Pane::Components => self.select_component(moved(self.component, Component::ALL.len())),
            Pane::Tweaks => self.select_tweak(moved(self.tweak, self.tweaks().len())),
            Pane::Detail => {
                self.detail_scroll = (i32::from(self.detail_scroll) + delta).clamp(0, 5000) as u16;
            }
            Pane::Bar => {}
        }
    }

    fn on_confirm_key(&mut self, key: KeyEvent, state: &mut ConfirmState) {
        if state.current_word().is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.screen = Screen::Main;
                    self.confirm_was_replaced = true;
                }
                KeyCode::Enter => {
                    if !state.accept_typed_word() {
                        self.message = Some(format!(
                            "Type exactly: {}",
                            state.current_word().unwrap_or_default()
                        ));
                    } else {
                        self.message = None;
                        state.focus_confirm = state.pending.is_empty();
                    }
                }
                KeyCode::Backspace => {
                    state.typing.pop();
                }
                KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
                KeyCode::Down => state.scroll = state.scroll.saturating_add(1),
                KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
                KeyCode::PageDown => state.scroll = state.scroll.saturating_add(10),
                _ => {
                    if let Some(c) = is_text_char(&key) {
                        state.typing.push(c);
                    }
                }
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.screen = Screen::Main;
                self.confirm_was_replaced = true;
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                state.focus_confirm = !state.focus_confirm;
            }
            KeyCode::Char(' ') => state.ticked = !state.ticked,
            KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
            KeyCode::Down => state.scroll = state.scroll.saturating_add(1),
            KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
            KeyCode::PageDown => state.scroll = state.scroll.saturating_add(10),
            KeyCode::Char('x') if state.undo_instances.is_some() => self.toggle_drift(state),
            KeyCode::Enter => {
                if state.dry || !state.focus_confirm {
                    self.screen = Screen::Main;
                    self.confirm_was_replaced = true;
                } else if state.ready() {
                    self.confirm_was_replaced = true;
                    self.execute((*state).clone());
                }
            }
            _ => {}
        }
    }

    fn hit_at(&self, column: u16, row: u16) -> Option<Hit> {
        let at = Position::new(column, row);
        self.hits
            .iter()
            .rev()
            .find(|(area, _)| area.contains(at))
            .map(|(_, hit)| hit.clone())
    }

    fn on_mouse(&mut self, mouse: MouseEvent, now: Instant) -> Flow {
        match mouse.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.hover = self.hit_at(mouse.column, mouse.row);
                Flow::Continue
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.pressed = self.hit_at(mouse.column, mouse.row);
                Flow::Continue
            }
            // A button fires on release over the same target, so sliding off cancels.
            MouseEventKind::Up(MouseButton::Left) => {
                let released = self.hit_at(mouse.column, mouse.row);
                let pressed = self.pressed.take();
                match (pressed, released) {
                    (Some(a), Some(b)) if a == b => self.on_click(a, now),
                    _ => Flow::Continue,
                }
            }
            MouseEventKind::ScrollUp => {
                self.on_wheel(mouse.column, mouse.row, -3);
                Flow::Continue
            }
            MouseEventKind::ScrollDown => {
                self.on_wheel(mouse.column, mouse.row, 3);
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    fn on_click(&mut self, hit: Hit, now: Instant) -> Flow {
        let double = self
            .last_click
            .as_ref()
            .is_some_and(|(last, at)| *last == hit && now.duration_since(*at) <= DOUBLE_CLICK);
        self.last_click = Some((hit.clone(), now));
        self.message = None;
        match hit {
            Hit::Component(i) => {
                self.select_component(i);
                self.pane = Pane::Components;
                Flow::Continue
            }
            Hit::Tweak(i) => {
                self.select_tweak(i);
                self.pane = Pane::Tweaks;
                if double {
                    self.last_click = None;
                    self.apply_chosen(false);
                }
                Flow::Continue
            }
            Hit::TweakBox(i) => {
                self.toggle_tick(i);
                self.pane = Pane::Tweaks;
                Flow::Continue
            }
            Hit::Bar => {
                self.pane = Pane::Bar;
                Flow::Continue
            }
            Hit::Tick => {
                if let Screen::Confirm(state) = &mut self.screen {
                    state.ticked = !state.ticked;
                }
                Flow::Continue
            }
            Hit::Button(button) => self.press_button(button),
        }
    }

    /// Scrolls whatever is under the pointer, not whatever has focus.
    fn on_wheel(&mut self, column: u16, row: u16, delta: i32) {
        let at = Position::new(column, row);
        let Some(area) = self
            .scrolls
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(at))
            .map(|(_, area)| *area)
        else {
            return;
        };
        match area {
            ScrollArea::Components => {
                let step = delta.signum();
                let next = (self.component as i32 + step).clamp(0, Component::ALL.len() as i32 - 1)
                    as usize;
                self.select_component(next);
            }
            ScrollArea::Tweaks => {
                let step = delta.signum();
                let count = self.tweaks().len() as i32;
                let next = (self.tweak as i32 + step).clamp(0, (count - 1).max(0)) as usize;
                self.select_tweak(next);
            }
            ScrollArea::Detail => {
                self.detail_scroll = (i32::from(self.detail_scroll) + delta).clamp(0, 5000) as u16;
            }
            ScrollArea::Body => match &mut self.screen {
                Screen::Confirm(state) => {
                    state.scroll = (i32::from(state.scroll) + delta).clamp(0, 5000) as u16;
                }
                Screen::Text { scroll, .. } => {
                    *scroll = (i32::from(*scroll) + delta).clamp(0, 5000) as u16;
                }
                _ => {}
            },
        }
    }

    pub(crate) fn applicability(&self, tweak: &Tweak) -> Result<(), String> {
        self.engine.applicability(tweak)
    }

    pub(crate) fn is_repair(tweak: &Tweak) -> bool {
        tweak.kind == TweakKind::Repair
    }
}
