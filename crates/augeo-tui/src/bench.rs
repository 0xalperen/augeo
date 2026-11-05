//! The benchmark flow: capturing series of runs, and reporting what they do and don't show.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use augeo_bench::{Comparison, Fingerprint, SavedRun, Verdict, compare, metrics_from_csv};
use augeo_catalog::schema::Effect;
use augeo_command::{BenchArgs, Purpose, Target};
use augeo_engine::{Confirmation, TweakRequest, UndoPolicy};
use augeo_ports::{Clock, Store, System};

use crate::app::{App, Host, Screen};

pub(crate) const RUN_SECONDS: u32 = 90;
pub(crate) const COUNTDOWN: Duration = Duration::from_secs(10);
const SERIES_RUNS: usize = 5;
/// The most danger an A/B test may switch on and off by itself, since it can't stop to ask each time.
const MAX_AB_DANGER: u8 = 1;

/// What a series of runs is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BenchKind {
    Series,
    /// The same thing measured against itself, to find the noise.
    NoiseFloor,
    /// A tweak switched on and off in A-B-B-A order.
    AbTest {
        request: TweakRequest,
        instance: String,
        needs_game_restart: bool,
        title: String,
    },
}

/// One side of a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arm {
    Before,
    After,
}

/// Where a session is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BenchPhase {
    /// Waiting for the user to get to the scene.
    Prepare,
    Countdown {
        until: Instant,
    },
    Capturing {
        since: Instant,
    },
}

/// A benchmark in progress.
#[derive(Debug, Clone)]
pub(crate) struct BenchSession {
    pub(crate) kind: BenchKind,
    pub(crate) exe: String,
    pub(crate) series: String,
    pub(crate) queue: Vec<Arm>,
    pub(crate) next: usize,
    pub(crate) runs: Vec<(Arm, SavedRun)>,
    pub(crate) phase: BenchPhase,
    pub(crate) note: Option<String>,
}

impl BenchSession {
    /// A line describing what the next run is.
    pub(crate) fn run_line(&self) -> String {
        let total = self.queue.len();
        let number = self.next + 1;
        match (&self.kind, self.queue.get(self.next)) {
            (BenchKind::AbTest { title, .. }, Some(Arm::Before)) => {
                format!("Run {number} of {total}: {title} is OFF")
            }
            (BenchKind::AbTest { title, .. }, Some(Arm::After)) => {
                format!("Run {number} of {total}: {title} is ON")
            }
            _ => format!("Run {number} of {total}"),
        }
    }

    pub(crate) fn needs_game_restart(&self) -> bool {
        matches!(
            &self.kind,
            BenchKind::AbTest {
                needs_game_restart: true,
                ..
            }
        )
    }
}

/// Alternating order cancels slow drift, such as shader caches filling and the system warming up.
fn abba(pairs: usize) -> Vec<Arm> {
    let mut queue = Vec::new();
    for pair in 0..pairs {
        if pair % 2 == 0 {
            queue.extend([Arm::Before, Arm::After]);
        } else {
            queue.extend([Arm::After, Arm::Before]);
        }
    }
    queue
}

fn one_decimal(value: f64) -> String {
    format!("{value:.1}")
}

/// Turns a comparison into lines of text.
pub(crate) fn report_lines(comparison: &Comparison) -> Vec<String> {
    match comparison {
        Comparison::NotComparable { reasons } => {
            let mut lines = vec![
                "These runs can't be fairly compared, so no conclusion is drawn.".to_string(),
                String::new(),
            ];
            lines.extend(reasons.iter().map(|r| format!("  - {r}")));
            lines
        }
        Comparison::Compared {
            findings,
            saturated,
            warnings,
        } => {
            let mut lines = Vec::new();
            if *saturated {
                lines.push(
                    "FPS is pinned at a cap or your refresh rate, so frame rate can't show a gain. \
                     Remove the cap, or use a heavier scene."
                        .to_string(),
                );
                lines.push(String::new());
            }
            for finding in findings {
                let verdict = match &finding.verdict {
                    Verdict::Improved => "IMPROVED".to_string(),
                    Verdict::Worse => "WORSE".to_string(),
                    Verdict::NotEnoughRuns => "not enough runs to say".to_string(),
                    Verdict::NoDetectableEffect {
                        could_detect_percent: Some(percent),
                    } => format!("no detectable effect (could detect about {percent:.1}% or more)"),
                    Verdict::NoDetectableEffect {
                        could_detect_percent: None,
                    } => "no detectable effect".to_string(),
                };
                lines.push(format!(
                    "{:<31} {:>8} -> {:<8} {:>+6.1}%  {verdict}",
                    finding.metric.label(),
                    one_decimal(finding.before),
                    one_decimal(finding.after),
                    finding.change_percent,
                ));
            }
            lines.push(String::new());
            lines.push(
                "\"No detectable effect\" doesn't prove there's none, only that this test couldn't \
                 see one. More runs can see smaller effects."
                    .to_string(),
            );
            if !warnings.is_empty() {
                lines.push(String::new());
                lines.push("Warnings:".to_string());
                lines.extend(warnings.iter().map(|w| format!("  - {w}")));
            }
            lines
        }
    }
}

fn split_arms(runs: &[(Arm, SavedRun)]) -> [Vec<(Fingerprint, augeo_bench::RunMetrics)>; 2] {
    let side = |wanted: Arm| {
        runs.iter()
            .filter(|(arm, _)| *arm == wanted)
            .map(|(_, r)| (r.fingerprint.clone(), r.metrics.clone()))
            .collect()
    };
    [side(Arm::Before), side(Arm::After)]
}

impl<S: System, T: Store, C: Clock, H: Host> App<S, T, C, H> {
    fn series_id(&self) -> String {
        format!("{}", self.host.now_ms() / 1000)
    }

    /// Stops a benchmark from starting when its result couldn't be trusted, or capture can't work.
    fn bench_blocker(&self, exe: Option<&String>) -> Option<String> {
        if exe.is_none() {
            return Some("Pick a game first, with auge!game cs2.exe, then try again.".to_string());
        }
        let pending = self.engine.pending_restarts();
        if !pending.is_empty() {
            return Some(format!(
                "Restart first: {} changed setting(s) haven't taken effect, so runs wouldn't reflect them.",
                pending.len()
            ));
        }
        None
    }

    pub(crate) fn run_bench_command(&mut self, input: &str, args: &BenchArgs) {
        match args {
            BenchArgs::Open | BenchArgs::List => self.show_bench_history(),
            BenchArgs::Compare(first, second) => self.show_bench_compare(first, second),
            BenchArgs::Start | BenchArgs::NoiseFloor | BenchArgs::AbTest(_) => {
                if !self.engine.is_writer() {
                    self.screen = Screen::Elevate {
                        command: input.trim().to_string(),
                    };
                    return;
                }
                let exe = self.current_game.clone();
                if let Some(reason) = self.bench_blocker(exe.as_ref()) {
                    self.message = Some(reason);
                    return;
                }
                let Some(exe) = exe else {
                    return;
                };
                let session = match args {
                    BenchArgs::NoiseFloor => Some((BenchKind::NoiseFloor, abba(3))),
                    BenchArgs::AbTest(target) => self.ab_session(input, target, &exe),
                    _ => Some((BenchKind::Series, vec![Arm::Before; SERIES_RUNS])),
                };
                let Some((kind, queue)) = session else {
                    return;
                };
                let session = BenchSession {
                    kind,
                    exe,
                    series: self.series_id(),
                    queue,
                    next: 0,
                    runs: Vec::new(),
                    phase: BenchPhase::Prepare,
                    note: None,
                };
                self.enter_prepare(session);
            }
        }
    }

    fn ab_session(
        &mut self,
        input: &str,
        target: &Target,
        exe: &str,
    ) -> Option<(BenchKind, Vec<Arm>)> {
        let view = self.engine.catalog().view(Some(exe.to_string()));
        let resolved = match augeo_command::resolve(target, &view, Purpose::Apply) {
            Ok(r) => r,
            Err(e) => {
                self.message = Some(e.render(input));
                return None;
            }
        };
        let [number] = resolved.numbers[..] else {
            self.message = Some("An A/B test takes one tweak at a time.".to_string());
            return None;
        };
        let tweak = self
            .engine
            .catalog()
            .find(resolved.component, number)?
            .clone();
        if tweak.danger > MAX_AB_DANGER {
            self.message = Some(format!(
                "{} is danger level {}. An A/B test switches a tweak on and off by itself, so it only \
                 takes level {MAX_AB_DANGER} or below. Use auge!bench start before and after instead.",
                tweak.title, tweak.danger
            ));
            return None;
        }
        if matches!(tweak.effect, Effect::Restart | Effect::Logoff) {
            self.message = Some(format!(
                "{} needs a restart or sign-out to take effect, so it can't be switched between \
                 runs. Use auge!bench start before and after instead, and treat that as weaker \
                 evidence, because a restart changes other things too.",
                tweak.title
            ));
            return None;
        }
        let request = TweakRequest {
            id: tweak.id.clone(),
            option: None,
            exe: resolved.exe.clone(),
        };
        let instance = match &resolved.exe {
            Some(game) => format!("{}@{}", tweak.id, game.to_lowercase()),
            None => tweak.id.clone(),
        };
        if self.engine.ledger().applied.contains_key(&instance) {
            self.message = Some(format!(
                "{} is already applied. Undo it first, so the test starts from where you began.",
                tweak.title
            ));
            return None;
        }
        Some((
            BenchKind::AbTest {
                request,
                instance,
                needs_game_restart: tweak.effect == Effect::App,
                title: tweak.title.clone(),
            },
            abba(4),
        ))
    }

    /// Puts the tweak under test into the state the next run needs.
    fn switch_arm(&mut self, session: &BenchSession) -> Result<(), String> {
        let BenchKind::AbTest {
            request, instance, ..
        } = &session.kind
        else {
            return Ok(());
        };
        let applied = self.engine.ledger().applied.contains_key(instance);
        match session.queue.get(session.next) {
            Some(Arm::After) if !applied => {
                let plan = self
                    .engine
                    .plan_apply(std::slice::from_ref(request))
                    .map_err(|e| e.to_string())?;
                self.engine
                    .execute(&plan, &Confirmation::default())
                    .map_err(|e| e.to_string())
                    .map(|_| ())
            }
            Some(Arm::Before) if applied => self.undo_tweak_under_test(instance),
            _ => Ok(()),
        }
    }

    fn undo_tweak_under_test(&mut self, instance: &str) -> Result<(), String> {
        let plan = self
            .engine
            .plan_undo(&[instance.to_string()], UndoPolicy::default())
            .map_err(|e| e.to_string())?;
        self.engine
            .execute(&plan, &Confirmation::default())
            .map_err(|e| e.to_string())
            .map(|_| ())
    }

    fn enter_prepare(&mut self, mut session: BenchSession) {
        if let Err(e) = self.switch_arm(&session) {
            self.abandon_bench(&session, format!("Switching the tweak failed: {e}"));
            return;
        }
        session.phase = BenchPhase::Prepare;
        self.screen = Screen::Bench(Box::new(session));
    }

    /// Ends a session early, leaving things as they were at the start.
    fn abandon_bench(&mut self, session: &BenchSession, note: String) {
        self.host.cancel_capture();
        let mut note = note;
        if let BenchKind::AbTest { instance, .. } = &session.kind
            && self.engine.ledger().applied.contains_key(instance)
            && let Err(e) = self.undo_tweak_under_test(instance)
        {
            note.push_str(&format!(" Undoing the tweak also failed: {e}"));
        }
        self.refresh();
        self.screen = Screen::Main;
        self.message = Some(note);
    }

    pub(crate) fn bench_start_countdown(&mut self, now: Instant) {
        if let Screen::Bench(session) = &mut self.screen
            && session.phase == BenchPhase::Prepare
        {
            session.phase = BenchPhase::Countdown {
                until: now + COUNTDOWN,
            };
        }
    }

    pub(crate) fn bench_cancel(&mut self) {
        if let Screen::Bench(session) = &self.screen {
            let session = (**session).clone();
            self.abandon_bench(&session, "The benchmark was stopped.".to_string());
        }
    }

    /// Moves a benchmark along. Called on every tick.
    pub(crate) fn bench_tick(&mut self, now: Instant) {
        let Screen::Bench(session) = &self.screen else {
            return;
        };
        let session = (**session).clone();
        match session.phase {
            BenchPhase::Prepare => {}
            BenchPhase::Countdown { until } if now >= until => {
                let id = format!("{}-{}", session.series, session.next);
                match self.host.start_capture(&session.exe, RUN_SECONDS, &id) {
                    Ok(()) => {
                        self.host.beep();
                        if let Screen::Bench(s) = &mut self.screen {
                            s.phase = BenchPhase::Capturing { since: now };
                        }
                    }
                    Err(e) => self.abandon_bench(&session, e),
                }
            }
            BenchPhase::Countdown { .. } => {}
            BenchPhase::Capturing { .. } => {
                let Some(result) = self.host.poll_capture() else {
                    return;
                };
                self.host.beep();
                self.finish_run(session, result);
            }
        }
    }

    fn finish_run(&mut self, mut session: BenchSession, result: Result<String, String>) {
        let metrics = result.and_then(|csv| metrics_from_csv(&csv, &session.exe));
        let metrics = match metrics {
            Ok(m) => m,
            Err(e) => {
                // A bad run is reported and repeated, rather than quietly thrown into the average.
                session.note = Some(format!("That run didn't count: {e}"));
                self.enter_prepare(session);
                return;
            }
        };
        let arm = session.queue[session.next];
        let label = match (&session.kind, arm) {
            (BenchKind::AbTest { .. }, Arm::Before) => "tweak off",
            (BenchKind::AbTest { .. }, Arm::After) => "tweak on",
            (BenchKind::NoiseFloor, _) => "noise floor",
            _ => "series",
        };
        let mut fingerprint = self.host.fingerprint(&session.exe);
        // The tweak under test may be what changes these, and that's the point of the test.
        if matches!(session.kind, BenchKind::AbTest { .. }) {
            fingerprint.power_scheme = None;
            fingerprint.display_mode = None;
        }
        let saved = SavedRun {
            series: session.series.clone(),
            index: session.next as u32,
            label: label.to_string(),
            time_ms: self.host.now_ms(),
            fingerprint,
            metrics,
        };
        if let Err(e) = self.host.save_run(&saved) {
            session.note = Some(format!("Saving this run failed: {e}"));
        } else {
            session.note = None;
        }
        session.runs.push((arm, saved));
        session.next += 1;
        if session.next < session.queue.len() {
            self.enter_prepare(session);
        } else {
            self.finish_session(&session);
        }
    }

    fn finish_session(&mut self, session: &BenchSession) {
        let mut lines = Vec::new();
        match &session.kind {
            BenchKind::Series => {
                lines.push(format!(
                    "Saved as series {}. Compare it with another using auge!bench compare.",
                    session.series
                ));
                lines.push(String::new());
                lines.extend(session.runs.iter().map(|(_, r)| run_line(r)));
            }
            BenchKind::NoiseFloor => {
                lines
                    .push("Nothing changed between these runs, so any difference is noise.".into());
                lines.push(
                    "An effect smaller than these numbers can't be told apart from it.".into(),
                );
                lines.push(String::new());
                let [before, after] = split_arms(&session.runs);
                lines.extend(report_lines(&compare(&before, &after, true)));
            }
            BenchKind::AbTest {
                instance, title, ..
            } => {
                lines.push(format!("{title}: off vs on, in A-B-B-A order."));
                lines.push(String::new());
                let [before, after] = split_arms(&session.runs);
                lines.extend(report_lines(&compare(&before, &after, true)));
                // Back to how things were. The user can then apply it if the result is worth it.
                if self.engine.ledger().applied.contains_key(instance)
                    && let Err(e) = self.undo_tweak_under_test(instance)
                {
                    lines.push(format!("Undoing the tweak failed: {e}"));
                }
                lines.push(String::new());
                lines.push("The tweak is off again, as it was when this test began.".to_string());
            }
        }
        self.refresh();
        self.screen = Screen::Text {
            title: "Benchmark result".to_string(),
            lines,
            scroll: 0,
        };
    }

    fn show_bench_history(&mut self) {
        let mut series: BTreeMap<String, Vec<SavedRun>> = BTreeMap::new();
        for run in self.host.list_runs() {
            series.entry(run.series.clone()).or_default().push(run);
        }
        let mut lines = vec![
            "1. Pick a game:             auge!game cs2.exe".to_string(),
            "2. Find your noise floor:   auge!bench aa      (6 runs, nothing changes)".to_string(),
            "3. Test one tweak:          auge!bench ab gpu#3   (8 runs, off and on)".to_string(),
            format!("   Or record {SERIES_RUNS} runs:          auge!bench start"),
            "   Then compare two series: auge!bench compare 12 13".to_string(),
            String::new(),
            "Use a repeatable scene or a built-in benchmark, the same every run. Each run lasts \
             90 seconds, and a beep marks its start and end."
                .to_string(),
            String::new(),
        ];
        if series.is_empty() {
            lines.push("No runs saved yet.".to_string());
        } else {
            lines.push("Saved series:".to_string());
            for (id, runs) in series.iter().rev() {
                let exe = runs.first().map_or("", |r| r.fingerprint.exe.as_str());
                lines.push(format!("  {id}  {exe}  {} run(s)", runs.len()));
            }
        }
        self.screen = Screen::Text {
            title: "Benchmark".to_string(),
            lines,
            scroll: 0,
        };
    }

    fn show_bench_compare(&mut self, first: &str, second: &str) {
        let runs = self.host.list_runs();
        let pick = |id: &str| -> Vec<(Fingerprint, augeo_bench::RunMetrics)> {
            runs.iter()
                .filter(|r| r.series == id)
                .map(|r| (r.fingerprint.clone(), r.metrics.clone()))
                .collect()
        };
        let (a, b) = (pick(first), pick(second));
        if a.is_empty() || b.is_empty() {
            let missing = if a.is_empty() { first } else { second };
            self.message = Some(format!(
                "No saved series called {missing}. See auge!bench list."
            ));
            return;
        }
        let mut lines = vec![format!("Series {first} vs series {second}."), String::new()];
        lines.extend(report_lines(&compare(&a, &b, false)));
        lines.push(String::new());
        lines.push(
            "Series captured at different times can differ for other reasons, too. An A/B test is \
             stronger evidence where a tweak allows one."
                .to_string(),
        );
        self.screen = Screen::Text {
            title: "Benchmark comparison".to_string(),
            lines,
            scroll: 0,
        };
    }
}

fn run_line(run: &SavedRun) -> String {
    let m = &run.metrics;
    format!(
        "Run {}: {:.1} FPS average, {:.1} 1% low, {:.2} ms p99, {} frames, {}",
        run.index + 1,
        m.avg_fps,
        m.low_1_percentile_fps,
        m.p99_ms,
        m.frames,
        m.present_mode.as_deref().unwrap_or("unknown present mode"),
    )
}
