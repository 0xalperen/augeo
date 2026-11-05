use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use augeo_bench::{Fingerprint, SavedRun};
use augeo_catalog::Catalog;
use augeo_engine::{
    Context, Disclaimer, Engine, TweakState,
    applicability::{CpuVendor, Environment},
    disclaimer,
};
use augeo_fakes::{FakeClock, FakeStore, FakeSystem, World};
use augeo_journal::Stamp;
use augeo_ports::{Address, RegRoot, Value};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend};

use crate::app::{Button, Hit, Pane, Screen};
use crate::theme;
use crate::{App, Flow, Host, POLLING_SECONDS, PollingUpdate};

const SID: &str = "S-1-5-21-1-2-3-1001";

#[derive(Default, Debug)]
struct FakeHost {
    accepted: bool,
    elevate_answer: Option<bool>,
    elevations: Vec<String>,
    restarts: u32,
    restore_points: u32,
    capture_error: Option<String>,
    captures: u32,
    cancels: u32,
    beeps: u32,
    next_capture: VecDeque<Result<String, String>>,
    saved: Vec<SavedRun>,
    display_mode: String,
    timer_holds: Vec<bool>,
    polling_started: bool,
    polling_cancelled: bool,
    polling_update: Option<PollingUpdate>,
    logged: Vec<String>,
}

impl Host for FakeHost {
    fn elevate(&mut self, command: &str) -> Result<bool, String> {
        self.elevations.push(command.to_string());
        Ok(self.elevate_answer.unwrap_or(true))
    }
    fn create_restore_point(&mut self) -> String {
        self.restore_points += 1;
        "Windows skipped it: a recent restore point exists.".to_string()
    }
    fn restart(&mut self) -> Result<(), String> {
        self.restarts += 1;
        Ok(())
    }
    fn start_capture(&mut self, _exe: &str, _seconds: u32, _id: &str) -> Result<(), String> {
        if let Some(error) = &self.capture_error {
            return Err(error.clone());
        }
        self.captures += 1;
        Ok(())
    }
    fn poll_capture(&mut self) -> Option<Result<String, String>> {
        self.next_capture.pop_front()
    }
    fn cancel_capture(&mut self) {
        self.cancels += 1;
    }
    fn fingerprint(&self, exe: &str) -> Fingerprint {
        Fingerprint {
            exe: exe.to_string(),
            windows_build: 26100,
            driver: Some("610.88".into()),
            display_mode: Some(self.display_mode.clone()),
            power_scheme: Some("x".into()),
        }
    }
    fn save_run(&mut self, run: &SavedRun) -> Result<(), String> {
        self.saved.push(run.clone());
        Ok(())
    }
    fn list_runs(&self) -> Vec<SavedRun> {
        self.saved.clone()
    }
    fn beep(&mut self) {
        self.beeps += 1;
    }
    fn now_ms(&self) -> u64 {
        1_700_000_000_000
    }
    fn timer_report(&mut self, hold: bool) -> Vec<String> {
        self.timer_holds.push(hold);
        vec![format!(
            "Sleep(1) takes {} ms",
            if hold { "1.1" } else { "2.0" }
        )]
    }
    fn start_polling_meter(&mut self) {
        self.polling_started = true;
    }
    fn poll_polling_meter(&mut self) -> PollingUpdate {
        self.polling_update
            .take()
            .unwrap_or(PollingUpdate::NoChange)
    }
    fn cancel_polling_meter(&mut self) {
        self.polling_cancelled = true;
    }
    fn log(&mut self, line: &str) {
        self.logged.push(line.to_string());
    }
    fn read_log(&self) -> Vec<String> {
        self.logged.clone()
    }
    fn check_report(&mut self) -> Vec<String> {
        vec!["[LOOK] RAM speed (best guess)".to_string()]
    }
    fn locally_accepted(&self, _: &str) -> bool {
        self.accepted
    }
    fn remember_acceptance(&mut self, _: &str) {
        self.accepted = true;
    }
}

type TestApp = App<FakeSystem, FakeStore, FakeClock, FakeHost>;

fn tweak(id: &str, number: u32, danger: u8, name: &str, extra: &str) -> String {
    format!(
        r#"
[[tweak]]
id = "{id}"
component = "system"
number = {number}
title = "Title {id}"
summary = "Summary of {id}."
danger = {danger}
evidence = "Documented"
effect = "now"
risks = ["Risk of {id}."]
undo_note = "Undo note of {id}."
{extra}
[[tweak.sources]]
url = "https://example.com/{id}"
retrieved = "2026-11-04"
claim = "Claim of {id}."
[[tweak.rev]]
rev = 1
[[tweak.rev.resource]]
kind = "reg_value"
scope = "machine"
path = 'Software\Test'
name = "{name}"
type = "dword"
desired = 1
"#
    )
}

fn catalog() -> Catalog {
    let system = [
        tweak("safe", 1, 0, "Safe", ""),
        tweak("medium", 2, 2, "Medium", ""),
        tweak("high", 3, 3, "High", ""),
        tweak("high-two", 4, 3, "HighTwo", ""),
        tweak("expert", 5, 4, "Expert", "security_lowering = true"),
        tweak(
            "not-here",
            6,
            0,
            "NotHere",
            "[tweak.applicability]\nbuild = \">=99999\"\n",
        ),
    ]
    .concat();
    let display = r#"
[[tweak]]
id = "refresh"
component = "display"
number = 1
title = "Best refresh"
summary = "Sets the best refresh."
kind = "repair"
danger = 1
evidence = "Documented"
effect = "now"
risks = ["Screen may go dark."]
undo_note = "Puts it back."
[[tweak.sources]]
url = "https://example.com/refresh"
retrieved = "2026-11-04"
claim = "Says so."
[[tweak.rev]]
rev = 1
[[tweak.rev.resource]]
kind = "display_mode"
mode = "max_refresh"
"#;
    let myths = "[[myth]]\nname = \"Some myth\"\nreason = \"It does nothing useful at all.\"\n";
    Catalog::parse(&[("system", &system), ("display", display), ("myths", myths)]).unwrap()
}

fn context() -> Context {
    Context {
        env: Environment {
            build: 26100,
            nvidia: true,
            laptop: false,
            cpu_vendor: CpuVendor::Amd,
            dual_ccd_x3d: false,
            vrr_active: false,
        },
        stamp: Stamp {
            boot_id: "b".into(),
            os_build: 26100,
            driver: None,
            app: "t".into(),
        },
        user_sid: SID.into(),
        power_scheme: "381b4222-f694-41f0-9685-ff5bb260df2e".into(),
        adapters: vec![],
        display_device: Some("DISPLAY1".into()),
        max_display_mode: Some("1920x1080@144".into()),
        frame_cap: None,
        disclaimer: Disclaimer {
            version: disclaimer::VERSION,
            text_sha256: disclaimer::current().text_sha256,
        },
    }
}

fn engine(world: &World, writer: bool) -> Engine<FakeSystem, FakeStore, FakeClock> {
    Engine::open(
        world.system(),
        world.store(),
        world.clock(),
        catalog(),
        context(),
        writer,
    )
    .unwrap()
}

struct Rig {
    world: World,
    app: TestApp,
    terminal: Terminal<TestBackend>,
    now: Instant,
}

impl Rig {
    fn with(accepted: bool, writer: bool, command: Option<&str>, size: (u16, u16)) -> Self {
        let world = World::new();
        Self::on_world(world, accepted, writer, command, size)
    }

    fn on_world(
        world: World,
        accepted: bool,
        writer: bool,
        command: Option<&str>,
        size: (u16, u16),
    ) -> Self {
        let reopen_world = world.clone();
        let app = App::new(
            engine(&world, writer),
            Box::new(move || {
                reopen_world.restart_process();
                Engine::open(
                    reopen_world.system(),
                    reopen_world.store(),
                    reopen_world.clock(),
                    catalog(),
                    context(),
                    false,
                )
            }),
            FakeHost {
                accepted,
                ..FakeHost::default()
            },
            "RTX test | Windows test".to_string(),
            command.map(str::to_string),
        );
        let mut rig = Self {
            world,
            app,
            terminal: Terminal::new(TestBackend::new(size.0, size.1)).unwrap(),
            now: Instant::now(),
        };
        rig.draw();
        rig
    }

    /// Accepted, elevated, wide window, on the main screen.
    fn main() -> Self {
        Self::with(true, true, None, (140, 40))
    }

    fn draw(&mut self) {
        let app = &mut self.app;
        self.terminal.draw(|f| app.draw(f)).unwrap();
    }

    fn text(&mut self) -> String {
        self.draw();
        let buffer = self.terminal.backend().buffer().clone();
        let width = buffer.area.width as usize;
        buffer
            .content
            .chunks(width)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key(&mut self, code: KeyCode) -> Flow {
        self.key_with(code, KeyModifiers::NONE)
    }

    fn key_with(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Flow {
        let flow = self.app.on_event(
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            self.now,
        );
        self.draw();
        flow
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    fn command(&mut self, line: &str) -> Flow {
        self.key(KeyCode::Char('/'));
        self.typed(line);
        self.key(KeyCode::Enter)
    }

    fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) -> Flow {
        let flow = self.app.on_event(
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }),
            self.now,
        );
        self.draw();
        flow
    }

    fn click_at(&mut self, column: u16, row: u16) -> Flow {
        self.mouse(MouseEventKind::Down(MouseButton::Left), column, row);
        self.mouse(MouseEventKind::Up(MouseButton::Left), column, row)
    }

    fn where_is(&self, wanted: &Hit) -> (u16, u16) {
        // The last match is the topmost one, as with real hit testing.
        let (area, _) = self
            .app
            .hits
            .iter()
            .rev()
            .find(|(_, hit)| hit == wanted)
            .unwrap_or_else(|| panic!("{wanted:?} isn't on screen: {:?}", self.app.hits));
        // A row's first columns are its checkbox, so a click on the row itself lands past them.
        let inset = if matches!(wanted, Hit::Tweak(_)) {
            5
        } else {
            0
        };
        (area.x + inset, area.y)
    }

    fn click(&mut self, wanted: &Hit) -> Flow {
        let (x, y) = self.where_is(wanted);
        self.click_at(x, y)
    }

    fn click_button(&mut self, button: Button) -> Flow {
        self.click(&Hit::Button(button))
    }

    fn value(&self, name: &str) -> Value {
        self.world.get(&Address::Reg {
            root: RegRoot::Machine,
            path: r"Software\Test".into(),
            name: name.into(),
        })
    }

    fn screen_is_main(&self) -> bool {
        matches!(self.app.screen, Screen::Main)
    }

    fn select_system(&mut self) {
        self.command("auge!system");
    }

    fn skip_restore_offer(&mut self) {
        if matches!(self.app.screen, Screen::RestoreOffer { .. }) {
            self.key(KeyCode::Esc);
        }
    }
}

#[test]
fn the_disclaimer_comes_first_and_declining_changes_nothing() {
    let mut rig = Rig::with(false, true, None, (120, 30));
    assert!(rig.text().contains("without warranty"));
    assert!(matches!(rig.app.screen, Screen::Disclaimer { .. }));
    assert_eq!(rig.key(KeyCode::Esc), Flow::Quit);
    assert!(rig.world.writes().is_empty());
    assert!(!rig.app.host.accepted);
}

#[test]
fn the_disclaimer_can_be_accepted_by_button_or_by_typing() {
    let mut rig = Rig::with(false, true, None, (120, 30));
    rig.click_button(Button::Accept);
    assert!(rig.screen_is_main());
    assert!(rig.app.engine.disclaimer_accepted());

    let mut rig = Rig::with(false, true, None, (120, 30));
    rig.typed("nope");
    rig.key(KeyCode::Enter);
    assert!(!rig.screen_is_main());
    for _ in 0..4 {
        rig.key(KeyCode::Backspace);
    }
    rig.typed("ACCEPT");
    rig.key(KeyCode::Enter);
    assert!(rig.screen_is_main());
}

#[test]
fn nothing_else_works_before_accepting() {
    let mut rig = Rig::with(false, true, None, (120, 30));
    rig.key(KeyCode::Char('a'));
    rig.key(KeyCode::Char('q'));
    assert!(matches!(rig.app.screen, Screen::Disclaimer { .. }));
    assert!(rig.world.writes().is_empty());
}

#[test]
fn acceptance_without_elevation_carries_over_to_the_elevated_copy() {
    let rig = Rig::with(true, true, None, (120, 30));
    assert!(rig.app.engine.disclaimer_accepted());
    assert!(rig.screen_is_main());
}

#[test]
fn a_small_window_asks_to_be_resized() {
    let mut rig = Rig::with(true, true, None, (70, 20));
    assert!(rig.text().contains("at least 80x24"));
    assert!(rig.app.hits.is_empty());
}

#[test]
fn every_component_is_listed_and_navigable_by_keyboard() {
    let mut rig = Rig::main();
    let text = rig.text();
    for name in [
        "cpu", "gpu", "game", "display", "input", "network", "memory", "power", "storage", "system",
    ] {
        assert!(text.contains(name), "{name}");
    }
    rig.key(KeyCode::BackTab);
    assert_eq!(rig.app.pane, Pane::Components);
    rig.key(KeyCode::End);
    assert_eq!(rig.app.current_component().as_str(), "system");
    rig.key(KeyCode::Up);
    assert_eq!(rig.app.current_component().as_str(), "storage");
    rig.key(KeyCode::Home);
    assert_eq!(rig.app.current_component().as_str(), "cpu");
    rig.key(KeyCode::Tab);
    assert_eq!(rig.app.pane, Pane::Tweaks);
    rig.key(KeyCode::Tab);
    assert_eq!(rig.app.pane, Pane::Detail);
}

#[test]
fn the_list_shows_badges_in_words_and_the_detail_pane_shows_everything() {
    let mut rig = Rig::main();
    rig.select_system();
    let text = rig.text();
    assert!(text.contains("Title safe"));
    assert!(text.contains("SAFE"));
    assert!(text.contains("DOC"));
    assert!(text.contains("not applied"));
    assert!(text.contains("Summary of safe."));
    assert!(text.contains("Risk of safe."));
    assert!(text.contains("Undo note of safe."));
    assert!(text.contains("https://example.com/safe"));
    assert!(text.contains("Anti-cheat"));

    rig.key(KeyCode::Down);
    assert!(rig.text().contains("Summary of medium."));
}

#[test]
fn a_tweak_that_does_not_apply_stays_visible_and_says_why() {
    let mut rig = Rig::main();
    rig.select_system();
    rig.key(KeyCode::End);
    let text = rig.text();
    assert!(text.contains("Title not-here"));
    assert!(text.contains("n/a"));
    assert!(text.contains("Doesn't apply here"));
    assert!(text.contains("99999"));

    rig.key(KeyCode::Char('a'));
    assert!(rig.screen_is_main());
    assert!(
        rig.app
            .message
            .as_deref()
            .unwrap()
            .contains("doesn't apply")
    );
    assert!(rig.world.writes().is_empty());
}

#[test]
fn applying_by_keyboard_shows_exact_values_then_verifies() {
    let mut rig = Rig::main();
    rig.select_system();
    rig.key(KeyCode::Enter);

    assert!(matches!(rig.app.screen, Screen::RestoreOffer { .. }));
    assert!(rig.text().contains("restore point"));
    rig.key(KeyCode::Esc);

    let text = rig.text();
    assert!(text.contains(r"HKLM\Software\Test : Safe"));
    assert!(text.contains("(does not exist)  ->  1"));
    assert!(text.contains("Risk of safe."));
    assert!(rig.world.writes().is_empty());

    rig.key(KeyCode::Enter);
    assert_eq!(rig.value("Safe"), Value::Dword(1));
    assert!(rig.text().contains("Applied and verified"));
    rig.key(KeyCode::Esc);
    assert!(rig.text().contains("applied"));
}

#[test]
fn the_restore_point_is_offered_once() {
    let mut rig = Rig::main();
    rig.select_system();
    rig.key(KeyCode::Enter);
    assert!(matches!(rig.app.screen, Screen::RestoreOffer { .. }));
    rig.key(KeyCode::Enter);
    assert_eq!(rig.app.host.restore_points, 1);
    assert!(matches!(rig.app.screen, Screen::Confirm(_)));
    assert!(rig.text().contains("Windows skipped it"));
    rig.key(KeyCode::Esc);

    rig.key(KeyCode::Enter);
    assert!(matches!(rig.app.screen, Screen::Confirm(_)));
    assert_eq!(rig.app.host.restore_points, 1);
}

#[test]
fn esc_never_confirms() {
    let mut rig = Rig::main();
    rig.select_system();
    rig.key(KeyCode::Enter);
    rig.skip_restore_offer();
    rig.key(KeyCode::Esc);
    assert!(rig.screen_is_main());
    assert!(rig.world.writes().is_empty());
}

#[test]
fn medium_needs_the_tick_and_starts_on_cancel() {
    let mut rig = Rig::main();
    rig.command("auge!system#2");
    rig.skip_restore_offer();
    assert!(rig.text().contains("I read the risks"));

    // Focus starts on Cancel, so a stray Enter backs out.
    rig.key(KeyCode::Enter);
    assert!(rig.screen_is_main());
    assert!(rig.world.writes().is_empty());

    rig.command("auge!system#2");
    rig.key(KeyCode::Tab);
    rig.key(KeyCode::Enter);
    assert!(matches!(rig.app.screen, Screen::Confirm(_)));
    assert!(rig.world.writes().is_empty());

    rig.key(KeyCode::Char(' '));
    rig.key(KeyCode::Enter);
    assert_eq!(rig.value("Medium"), Value::Dword(1));
}

#[test]
fn high_needs_the_exact_word_for_each_tweak() {
    let mut rig = Rig::main();
    rig.command("auge!system#3-4");
    rig.skip_restore_offer();
    assert!(rig.text().contains("type \"apply\""));

    rig.typed("APPLY");
    rig.key(KeyCode::Enter);
    assert!(rig.text().contains("Type exactly: apply"));
    for _ in 0..5 {
        rig.key(KeyCode::Backspace);
    }
    rig.typed("apply");
    rig.key(KeyCode::Enter);
    // The second tweak asks too.
    assert!(rig.text().contains("high-two: type"));
    assert!(rig.world.writes().is_empty());

    rig.typed("apply");
    rig.key(KeyCode::Enter);
    rig.key(KeyCode::Enter);
    assert_eq!(rig.value("High"), Value::Dword(1));
    assert_eq!(rig.value("HighTwo"), Value::Dword(1));
}

#[test]
fn expert_needs_the_full_phrase() {
    let mut rig = Rig::main();
    rig.command("auge!system#5");
    rig.skip_restore_offer();
    assert!(rig.text().contains("I accept the risk"));
    rig.typed("apply");
    rig.key(KeyCode::Enter);
    assert!(rig.world.writes().is_empty());
    for _ in 0..5 {
        rig.key(KeyCode::Backspace);
    }
    rig.typed("I accept the risk");
    rig.key(KeyCode::Enter);
    rig.key(KeyCode::Enter);
    assert_eq!(rig.value("Expert"), Value::Dword(1));
}

#[test]
fn an_expert_tweak_in_a_batch_is_refused_by_the_engine() {
    let mut rig = Rig::main();
    rig.command("auge!system#1,5");
    rig.skip_restore_offer();
    rig.typed("I accept the risk");
    rig.key(KeyCode::Enter);
    rig.key(KeyCode::Enter);
    assert!(rig.world.writes().is_empty());
    assert!(rig.app.message.as_deref().unwrap().contains("on its own"));
}

#[test]
fn dry_run_shows_the_same_changes_and_cannot_write() {
    let mut rig = Rig::main();
    rig.command("auge!dry system#1");
    let text = rig.text();
    assert!(text.contains("Dry run: nothing will change"));
    assert!(text.contains(r"HKLM\Software\Test : Safe"));
    assert!(text.contains("->"));
    rig.key(KeyCode::Enter);
    assert!(rig.screen_is_main());
    assert!(rig.world.writes().is_empty());

    rig.select_system();
    rig.key(KeyCode::Char('d'));
    assert!(rig.text().contains("Dry run"));
    rig.key(KeyCode::Tab);
    rig.key(KeyCode::Enter);
    assert!(rig.world.writes().is_empty());
}

#[test]
fn ticking_applies_the_ticked_set_with_keyboard_and_mouse() {
    let mut rig = Rig::main();
    rig.select_system();
    rig.key(KeyCode::Char(' '));
    assert!(rig.text().contains("[x]"));
    rig.key(KeyCode::Down);
    rig.key(KeyCode::Down);
    rig.click(&Hit::TweakBox(2));
    rig.key(KeyCode::Char('d'));
    let text = rig.text();
    assert!(text.contains("Title safe"));
    assert!(text.contains("Title high"));
    assert!(!text.contains("Title medium"));
}

#[test]
fn undo_by_keyboard_restores_and_undo_all_works_by_command() {
    let mut rig = Rig::main();
    rig.select_system();
    rig.key(KeyCode::Enter);
    rig.skip_restore_offer();
    rig.key(KeyCode::Enter);
    rig.key(KeyCode::Esc);
    assert_eq!(rig.value("Safe"), Value::Dword(1));

    rig.key(KeyCode::Char('u'));
    assert!(rig.text().contains("Undo: these exact changes"));
    rig.key(KeyCode::Enter);
    assert_eq!(rig.value("Safe"), Value::Absent);
    rig.key(KeyCode::Esc);

    rig.command("auge!system#1");
    rig.key(KeyCode::Enter);
    rig.key(KeyCode::Esc);
    rig.command("auge!undo-all");
    rig.key(KeyCode::Enter);
    assert_eq!(rig.value("Safe"), Value::Absent);
}

#[test]
fn undo_when_nothing_is_applied_says_so() {
    let mut rig = Rig::main();
    rig.command("auge!undo system#1");
    assert!(rig.screen_is_main());
    assert!(
        rig.app
            .message
            .as_deref()
            .unwrap()
            .contains("Nothing to do")
    );
}

#[test]
fn an_external_change_is_kept_on_undo_unless_overwriting_is_chosen() {
    let mut rig = Rig::main();
    let address = Address::Reg {
        root: RegRoot::Machine,
        path: r"Software\Test".into(),
        name: "Safe".into(),
    };
    rig.command("auge!system#1");
    rig.skip_restore_offer();
    rig.key(KeyCode::Enter);
    rig.key(KeyCode::Esc);
    rig.world.set(&address, Value::Dword(9));

    rig.command("auge!undo system#1");
    assert!(rig.text().contains("Changed by something else"));
    rig.key(KeyCode::Enter);
    assert_eq!(rig.value("Safe"), Value::Dword(9));
    rig.key(KeyCode::Esc);

    rig.command("auge!system#1");
    rig.key(KeyCode::Enter);
    rig.key(KeyCode::Esc);
    // This time something else sets 7, and undo is told to overwrite it with the value from before.
    rig.world.set(&address, Value::Dword(7));
    rig.command("auge!undo system#1");
    rig.key(KeyCode::Char('x'));
    assert!(rig.text().contains("[x] Overwrite"));
    rig.key(KeyCode::Enter);
    assert_eq!(rig.value("Safe"), Value::Dword(9));
}

#[test]
fn the_screen_is_refreshed_with_new_values_if_the_system_changed() {
    let mut rig = Rig::main();
    rig.command("auge!system#1");
    rig.skip_restore_offer();
    rig.world.set(
        &Address::Reg {
            root: RegRoot::Machine,
            path: r"Software\Test".into(),
            name: "Safe".into(),
        },
        Value::Dword(5),
    );
    rig.key(KeyCode::Enter);
    let text = rig.text();
    assert!(text.contains("The system changed"));
    assert!(text.contains("5 (0x00000005)"));
    assert!(rig.world.writes().is_empty());
}

#[test]
fn clicking_works_for_tabs_rows_buttons_and_the_bar() {
    let mut rig = Rig::main();
    rig.click(&Hit::Component(9));
    assert_eq!(rig.app.current_component().as_str(), "system");
    rig.click(&Hit::Tweak(1));
    assert_eq!(rig.app.tweak, 1);
    assert!(rig.text().contains("Summary of medium."));

    rig.click(&Hit::Bar);
    assert_eq!(rig.app.pane, Pane::Bar);
    rig.typed("auge!system");
    rig.key(KeyCode::Enter);

    rig.click(&Hit::Tweak(0));
    rig.click_button(Button::Dry);
    assert!(rig.text().contains("Dry run"));
    rig.click_button(Button::Cancel);
    assert!(rig.screen_is_main());

    rig.click_button(Button::Help);
    assert!(rig.text().contains("auge!undo-all"));
    rig.click_button(Button::Close);
    assert!(rig.screen_is_main());
    assert_eq!(rig.click_button(Button::Quit), Flow::Quit);
}

#[test]
fn applying_by_mouse_alone_works_from_start_to_finish() {
    let mut rig = Rig::main();
    rig.click(&Hit::Component(9));
    rig.click(&Hit::Tweak(1));
    rig.click_button(Button::Apply);
    rig.click_button(Button::SkipRestorePoint);
    assert!(
        rig.app
            .hits
            .iter()
            .all(|(_, h)| *h != Hit::Button(Button::Confirm))
    );
    rig.click(&Hit::Tick);
    rig.click_button(Button::Confirm);
    assert_eq!(rig.value("Medium"), Value::Dword(1));
    rig.click_button(Button::Close);
    rig.click_button(Button::Undo);
    rig.click_button(Button::Confirm);
    assert_eq!(rig.value("Medium"), Value::Absent);
}

#[test]
fn a_disabled_confirm_button_cannot_be_clicked() {
    let mut rig = Rig::main();
    rig.command("auge!system#3");
    rig.skip_restore_offer();
    assert!(rig.text().contains("not yet"));
    assert!(
        rig.app
            .hits
            .iter()
            .all(|(_, h)| *h != Hit::Button(Button::Confirm))
    );
}

#[test]
fn a_button_fires_on_release_over_the_same_target_only() {
    let mut rig = Rig::main();
    let (x, y) = rig.where_is(&Hit::Button(Button::Quit));
    rig.mouse(MouseEventKind::Down(MouseButton::Left), x, y);
    assert_eq!(
        rig.mouse(MouseEventKind::Up(MouseButton::Left), 0, 0),
        Flow::Continue
    );
    assert_eq!(rig.click_at(x, y), Flow::Quit);
}

#[test]
fn double_clicking_a_tweak_opens_its_changes_but_slow_clicks_do_not() {
    let mut rig = Rig::main();
    rig.select_system();
    rig.click(&Hit::Tweak(0));
    rig.now += Duration::from_secs(5);
    rig.click(&Hit::Tweak(0));
    assert!(rig.screen_is_main());

    rig.now += Duration::from_millis(100);
    rig.click(&Hit::Tweak(0));
    assert!(matches!(
        rig.app.screen,
        Screen::RestoreOffer { .. } | Screen::Confirm(_)
    ));
    assert!(rig.world.writes().is_empty());
}

#[test]
fn the_wheel_scrolls_whatever_is_under_the_pointer() {
    let mut rig = Rig::main();
    rig.select_system();
    let (x, y) = rig.where_is(&Hit::Tweak(0));
    rig.mouse(MouseEventKind::ScrollDown, x, y);
    assert_eq!(rig.app.tweak, 1);
    rig.mouse(MouseEventKind::ScrollUp, x, y);
    assert_eq!(rig.app.tweak, 0);

    let (x, y) = rig.where_is(&Hit::Component(0));
    rig.mouse(MouseEventKind::ScrollUp, x, y);
    assert_eq!(rig.app.current_component().as_str(), "storage");
    rig.select_system();

    // The detail pane scrolls without moving the focus.
    rig.app.pane = Pane::Tweaks;
    let detail = rig
        .app
        .scrolls
        .iter()
        .find(|(_, a)| *a == crate::app::ScrollArea::Detail)
        .map(|(r, _)| *r)
        .unwrap();
    rig.mouse(MouseEventKind::ScrollDown, detail.x + 1, detail.y + 1);
    assert_eq!(rig.app.detail_scroll, 3);
    assert_eq!(rig.app.pane, Pane::Tweaks);
}

#[test]
fn commands_navigate_and_report_errors_with_a_caret() {
    let mut rig = Rig::main();
    rig.command("auge!system");
    assert_eq!(rig.app.current_component().as_str(), "system");

    rig.command("auge!cpuu#1");
    let text = rig.text();
    assert!(text.contains("Did you mean 'cpu'?"));
    assert!(text.contains('^'));

    rig.command("auge!system#99");
    assert!(
        rig.text()
            .contains("tweak #99 doesn't exist in system (it has 1-6)")
    );

    rig.command("system#1");
    assert!(rig.text().contains("Did you mean auge!system#1?"));

    assert_eq!(rig.command("auge!quit"), Flow::Quit);
}

#[test]
fn altgr_characters_count_as_typing_but_ctrl_shortcuts_do_not() {
    let mut rig = Rig::main();
    rig.key(KeyCode::Char('/'));
    rig.key_with(
        KeyCode::Char('#'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert_eq!(rig.app.bar, "#");
    rig.key_with(KeyCode::Char('c'), KeyModifiers::CONTROL);
    rig.key_with(KeyCode::Char('x'), KeyModifiers::ALT);
    assert_eq!(rig.app.bar, "#");
    rig.key_with(KeyCode::Char('!'), KeyModifiers::SHIFT);
    assert_eq!(rig.app.bar, "#!");
}

#[test]
fn help_lists_commands_and_status_lists_states() {
    let mut rig = Rig::main();
    rig.command("auge!help");
    let text = rig.text();
    assert!(text.contains("auge!bench"));
    assert!(text.contains("system"));
    rig.key(KeyCode::Esc);

    rig.command("auge!status system");
    let text = rig.text();
    assert!(text.contains("Status"));
    assert!(text.contains("system#1"));
    assert!(text.contains("not applied"));
}

#[test]
fn a_restart_needs_an_explicit_yes() {
    let mut rig = Rig::main();
    rig.command("auge!reboot");
    assert!(rig.text().contains("Restart now?"));
    rig.key(KeyCode::Enter);
    assert_eq!(rig.app.host.restarts, 0);
    assert!(rig.screen_is_main());

    rig.command("auge!reboot");
    rig.key(KeyCode::Esc);
    assert_eq!(rig.app.host.restarts, 0);

    rig.command("auge!reboot");
    assert_eq!(rig.key(KeyCode::Char('y')), Flow::Quit);
    assert_eq!(rig.app.host.restarts, 1);
}

#[test]
fn read_only_explains_elevation_and_passes_only_the_command() {
    let mut rig = Rig::with(true, false, None, (140, 40));
    assert!(rig.text().contains("READ-ONLY"));
    rig.command("auge!system#1,2");
    assert!(rig.text().contains("Elevation needed"));
    assert!(rig.text().contains("auge!system#1,2"));
    assert!(rig.app.host.elevations.is_empty());

    rig.key(KeyCode::Esc);
    assert!(rig.app.host.elevations.is_empty());

    rig.command("auge!system#1,2");
    rig.key(KeyCode::Enter);
    assert_eq!(rig.app.host.elevations, vec!["auge!system#1,2".to_string()]);
    assert!(rig.screen_is_main());
    assert!(rig.world.writes().is_empty());
}

#[test]
fn declining_elevation_changes_nothing_and_says_so() {
    let mut rig = Rig::with(true, false, None, (140, 40));
    rig.app.host.elevate_answer = Some(false);
    rig.command("auge!undo-all");
    rig.key(KeyCode::Enter);
    assert!(rig.text().contains("declined"));
    assert!(rig.world.writes().is_empty());
}

#[test]
fn a_read_only_dry_run_needs_no_elevation() {
    let mut rig = Rig::with(true, false, None, (140, 40));
    rig.command("auge!dry system#1");
    assert!(rig.text().contains("Dry run"));
}

#[test]
fn a_startup_command_only_reaches_the_confirmation_screen() {
    let mut rig = Rig::with(true, true, Some("auge!system#1"), (140, 40));
    assert!(matches!(rig.app.screen, Screen::RestoreOffer { .. }));
    rig.key(KeyCode::Esc);
    assert!(matches!(rig.app.screen, Screen::Confirm(_)));
    assert!(rig.world.writes().is_empty());
}

#[test]
fn a_startup_command_waits_for_the_disclaimer() {
    let mut rig = Rig::with(false, true, Some("auge!system#1"), (140, 40));
    assert!(matches!(rig.app.screen, Screen::Disclaimer { .. }));
    rig.click_button(Button::Accept);
    assert!(matches!(rig.app.screen, Screen::RestoreOffer { .. }));
    assert!(rig.world.writes().is_empty());
}

fn interrupted_world() -> World {
    let world = World::new();
    {
        let mut e = engine(&world, true);
        e.accept_disclaimer().unwrap();
        let plan = e
            .plan_apply(&[augeo_engine::TweakRequest::new("safe")])
            .unwrap();
        // Find a crash point that leaves the write done but the change unfinished.
        for calls in 0.. {
            let probe = World::new();
            let mut p = engine(&probe, true);
            p.accept_disclaimer().unwrap();
            let probe_plan = p
                .plan_apply(&[augeo_engine::TweakRequest::new("safe")])
                .unwrap();
            probe.crash_after(calls);
            let _ = p.execute(&probe_plan, &augeo_engine::Confirmation::default());
            drop(p);
            probe.restart_process();
            let reopened = engine(&probe, true);
            let in_doubt = !reopened.in_doubt().unwrap().is_empty();
            let done = reopened.status("safe", None).unwrap().state == TweakState::NotApplied;
            drop(reopened);
            probe.restart_process();
            if in_doubt && !done {
                world.crash_after(calls);
                let _ = e.execute(&plan, &augeo_engine::Confirmation::default());
                break;
            }
            assert!(calls < 200, "no crash point found");
        }
    }
    world.restart_process();
    world
}

#[test]
fn recovery_blocks_everything_until_resolved_and_defaults_to_rollback() {
    let world = interrupted_world();
    let mut rig = Rig::on_world(world, true, true, Some("auge!system#2"), (140, 40));
    assert!(matches!(rig.app.screen, Screen::Recovery { .. }));
    let text = rig.text();
    assert!(text.contains("interrupted"));
    assert!(text.contains("Title safe"));
    assert!(text.contains("before: (does not exist)"));

    rig.key(KeyCode::Char('a'));
    rig.key(KeyCode::Esc);
    assert!(matches!(rig.app.screen, Screen::Recovery { .. }));

    rig.key(KeyCode::Enter);
    assert_eq!(rig.value("Safe"), Value::Absent);
    // The startup command runs only now, and only reaches the steps before confirming.
    assert!(matches!(
        rig.app.screen,
        Screen::RestoreOffer { .. } | Screen::Confirm(_)
    ));
    assert_eq!(rig.value("Medium"), Value::Absent);
}

#[test]
fn recovery_can_keep_what_was_done_by_mouse() {
    let world = interrupted_world();
    let mut rig = Rig::on_world(world, true, true, None, (140, 40));
    rig.click_button(Button::Keep);
    assert!(rig.screen_is_main());
    assert_eq!(rig.value("Safe"), Value::Dword(1));
}

fn display_address() -> Address {
    Address::DisplayMode {
        device: "DISPLAY1".into(),
    }
}

fn rig_at_keep_display() -> Rig {
    let world = World::new();
    world.set(&display_address(), Value::Text("1920x1080@60".into()));
    let mut rig = Rig::on_world(world, true, true, None, (140, 40));
    rig.command("auge!display#1");
    rig.skip_restore_offer();
    assert!(rig.text().contains("1920x1080@144"));
    rig.key(KeyCode::Enter);
    assert!(rig.text().contains("Keep this display mode?"));
    assert_eq!(
        rig.world.get(&display_address()),
        Value::Text("1920x1080@144".into())
    );
    rig
}

#[test]
fn a_new_display_mode_reverts_by_itself_with_no_answer() {
    let mut rig = rig_at_keep_display();
    // Not yet.
    rig.app.on_tick(Instant::now() + Duration::from_secs(5));
    assert!(matches!(rig.app.screen, Screen::KeepDisplay { .. }));
    rig.app.on_tick(Instant::now() + Duration::from_secs(16));
    assert!(rig.screen_is_main());
    assert_eq!(
        rig.world.get(&display_address()),
        Value::Text("1920x1080@60".into())
    );
    assert!(rig.text().contains("put back"));
}

#[test]
fn a_new_display_mode_can_be_kept_or_put_back_now() {
    let mut rig = rig_at_keep_display();
    rig.key(KeyCode::Enter);
    rig.app.on_tick(Instant::now() + Duration::from_secs(60));
    assert_eq!(
        rig.world.get(&display_address()),
        Value::Text("1920x1080@144".into())
    );

    let mut rig = rig_at_keep_display();
    rig.click_button(Button::RevertDisplay);
    assert_eq!(
        rig.world.get(&display_address()),
        Value::Text("1920x1080@60".into())
    );
}

#[test]
fn the_check_report_includes_the_audit_of_earlier_tweaks() {
    let world = World::new();
    world.set(&display_address(), Value::Text("1920x1080@60".into()));
    let mut rig = Rig::on_world(world, true, true, None, (140, 40));
    rig.command("auge!check");
    let text = rig.text();
    assert!(text.contains("RAM speed"));
    assert!(text.contains("nothing here changes anything"));
    assert!(text.contains("these repairs have something to fix"));
    assert!(text.contains("display#1"));
    assert!(rig.world.writes().is_empty());
}

#[test]
fn the_myths_screen_lists_reasons() {
    let mut rig = Rig::main();
    rig.click_button(Button::Myths);
    let text = rig.text();
    assert!(text.contains("Some myth"));
    assert!(text.contains("It does nothing useful at all."));
}

/// A capture at a steady frame time, with a little per-run spread from `ms`.
fn capture_csv(exe: &str, ms: f64) -> String {
    let mut text = String::from("Application,TimeInMs,MsBetweenPresents,PresentMode\n");
    text.push_str(&format!("{exe},0,{ms},Hardware: Independent Flip\n"));
    for i in 0..1000 {
        // Some frames are slower, so a run isn't pinned flat.
        let frame = if i % 20 == 0 { ms * 2.0 } else { ms };
        text.push_str(&format!(
            "{exe},{},{frame},Hardware: Independent Flip\n",
            6000 + i
        ));
    }
    text
}

impl Rig {
    fn bench_rig() -> Self {
        let mut rig = Self::main();
        rig.app.host.display_mode = "2560x1440@240".into();
        rig.command("auge!game game.exe");
        rig
    }

    /// Plays one whole run: start it, let the countdown pass, then finish capture with this result.
    fn play_run(&mut self, result: Result<String, String>) {
        self.key(KeyCode::Enter);
        let later = Instant::now() + Duration::from_secs(11);
        self.app.on_tick(later);
        self.app.host.next_capture.push_back(result);
        self.app.on_tick(later);
        self.draw();
    }
}

#[test]
fn a_benchmark_needs_a_game_elevation_and_no_pending_restart() {
    let mut rig = Rig::main();
    rig.command("auge!bench start");
    assert!(rig.text().contains("Pick a game first"));
    assert!(rig.screen_is_main());

    let mut rig = Rig::with(true, false, None, (140, 40));
    rig.command("auge!bench start");
    assert!(rig.text().contains("Elevation needed"));
}

#[test]
fn a_series_records_its_runs_with_beeps_and_saves_them() {
    let mut rig = Rig::bench_rig();
    rig.command("auge!bench start");
    assert!(rig.text().contains("Run 1 of 5"));
    assert!(rig.text().contains("repeatable scene"));
    for _ in 0..5 {
        rig.play_run(Ok(capture_csv("game.exe", 10.0)));
    }
    assert_eq!(rig.app.host.saved.len(), 5);
    assert_eq!(rig.app.host.captures, 5);
    assert_eq!(rig.app.host.beeps, 10);
    let text = rig.text();
    assert!(text.contains("Saved as series"), "{text}");
    assert!(text.contains("Run 5:"), "{text}");
}

#[test]
fn a_bad_run_is_repeated_rather_than_counted() {
    let mut rig = Rig::bench_rig();
    rig.command("auge!bench start");
    rig.play_run(Err(
        "No frames arrived in time. Was game.exe running?".into()
    ));
    assert!(rig.text().contains("That run didn't count"));
    assert!(rig.text().contains("Run 1 of 5"));
    assert!(rig.app.host.saved.is_empty());

    rig.play_run(Ok(String::from("Application\nnothing")));
    assert!(rig.text().contains("That run didn't count"));
    assert!(rig.app.host.saved.is_empty());
}

#[test]
fn a_missing_presentmon_stops_cleanly_with_the_reason() {
    let mut rig = Rig::bench_rig();
    rig.app.host.capture_error = Some("PresentMon wasn't found.".into());
    rig.command("auge!bench start");
    rig.key(KeyCode::Enter);
    rig.app.on_tick(Instant::now() + Duration::from_secs(11));
    assert!(rig.screen_is_main());
    assert!(rig.text().contains("PresentMon wasn't found."));
}

#[test]
fn the_noise_floor_changes_nothing_and_says_what_it_means() {
    let mut rig = Rig::bench_rig();
    rig.command("auge!bench aa");
    assert!(rig.text().contains("Run 1 of 6"));
    for _ in 0..6 {
        rig.play_run(Ok(capture_csv("game.exe", 10.0)));
    }
    let text = rig.text();
    assert!(text.contains("any difference is noise"), "{text}");
    assert!(text.contains("no detectable effect"), "{text}");
    assert!(!text.contains("IMPROVED"), "{text}");
    assert!(rig.world.writes().is_empty());
}

#[test]
fn an_ab_test_switches_the_tweak_in_abba_order_and_ends_where_it_began() {
    let mut rig = Rig::bench_rig();
    rig.command("auge!bench ab system#1");
    assert!(rig.text().contains("is OFF"));
    assert_eq!(rig.value("Safe"), Value::Absent);

    // A B B A A B B A, where "on" really is faster.
    let order = [false, true, true, false, false, true, true, false];
    for (i, on) in order.iter().enumerate() {
        assert_eq!(rig.value("Safe") == Value::Dword(1), *on, "run {i}");
        let spread = i as f64 * 0.001;
        let ms = if *on { 10.0 } else { 12.0 } + spread;
        rig.play_run(Ok(capture_csv("game.exe", ms)));
    }
    let text = rig.text();
    assert!(text.contains("off vs on"), "{text}");
    assert!(text.contains("IMPROVED"), "{text}");
    assert!(text.contains("The tweak is off again"), "{text}");
    assert_eq!(rig.value("Safe"), Value::Absent);
    assert!(rig.app.engine.ledger().applied.is_empty());
    assert_eq!(rig.app.host.saved.len(), 8);
}

#[test]
fn stopping_an_ab_test_puts_the_tweak_back() {
    let mut rig = Rig::bench_rig();
    rig.command("auge!bench ab system#1");
    rig.play_run(Ok(capture_csv("game.exe", 10.0)));
    assert_eq!(rig.value("Safe"), Value::Dword(1));
    rig.key(KeyCode::Esc);
    assert!(rig.screen_is_main());
    assert_eq!(rig.value("Safe"), Value::Absent);
    assert_eq!(rig.app.host.cancels, 1);
}

#[test]
fn risky_tweaks_cannot_be_ab_tested_automatically() {
    let mut rig = Rig::bench_rig();
    rig.command("auge!bench ab system#2");
    assert!(rig.text().contains("only takes level 1"));
    assert!(rig.screen_is_main());
    assert!(rig.world.writes().is_empty());

    rig.command("auge!bench ab system#1,2");
    assert!(rig.text().contains("one tweak at a time"));
}

#[test]
fn comparing_series_refuses_different_conditions() {
    let mut rig = Rig::bench_rig();
    rig.command("auge!bench start");
    for _ in 0..5 {
        rig.play_run(Ok(capture_csv("game.exe", 10.0)));
    }
    rig.key(KeyCode::Esc);
    let first = rig.app.host.saved[0].series.clone();
    assert!(rig.text().contains("Press / to type"));

    // The same series ID can't be used twice in this fake, so rewrite one for the second series.
    let mut other = rig.app.host.saved.clone();
    for run in &mut other {
        run.series = "second".into();
        run.fingerprint.display_mode = Some("1920x1080@60".into());
    }
    rig.app.host.saved.extend(other);

    rig.command(&format!("auge!bench compare {first} second"));
    let text = rig.text();
    assert!(text.contains("can't be fairly compared"), "{text}");
    assert!(text.contains("display mode"), "{text}");
    rig.key(KeyCode::Esc);

    rig.command(&format!("auge!bench compare {first} nope"));
    assert!(rig.text().contains("No saved series called nope"));

    rig.command("auge!bench list");
    assert!(rig.text().contains(&first));
}

#[test]
fn the_timer_hold_ends_when_the_screen_closes() {
    let mut rig = Rig::main();
    rig.command("auge!timer");
    assert!(rig.text().contains("Sleep(1) takes 2.0 ms"));
    rig.key(KeyCode::Char('h'));
    assert!(rig.text().contains("Sleep(1) takes 1.1 ms"));
    assert!(rig.text().contains("Stop holding"));
    rig.key(KeyCode::Esc);
    assert!(rig.screen_is_main());
    assert_eq!(rig.app.host.timer_holds, vec![false, true, false]);
    assert!(rig.world.writes().is_empty());
}

#[test]
fn the_polling_meter_waits_for_a_click_shows_a_timer_then_reports() {
    let mut rig = Rig::main();
    rig.command("auge!polling");
    assert!(rig.app.host.polling_started);
    assert!(rig.text().contains("Waiting for you"));
    rig.app.on_tick(Instant::now());
    assert!(matches!(
        rig.app.screen,
        Screen::PollingMeter { since: None }
    ));
    assert!(rig.text().contains("Waiting for you"));

    rig.app.host.polling_update = Some(PollingUpdate::Started);
    rig.app.on_tick(Instant::now());
    let text = rig.text();
    assert!(text.contains("Testing"), "{text}");
    assert!(text.contains(&format!("{POLLING_SECONDS}s left")), "{text}");

    rig.app.host.polling_update = Some(PollingUpdate::Finished(vec!["About 1000 Hz".into()]));
    rig.app.on_tick(Instant::now());
    assert!(rig.text().contains("About 1000 Hz"));
}

#[test]
fn the_polling_test_can_be_cancelled_from_either_window() {
    let mut rig = Rig::main();
    rig.command("auge!polling");
    rig.key(KeyCode::Esc);
    assert!(rig.screen_is_main());
    assert!(rig.app.host.polling_cancelled);

    let mut rig = Rig::main();
    rig.command("auge!polling");
    rig.click_button(Button::PollingCancel);
    assert!(rig.screen_is_main());
    assert!(rig.app.host.polling_cancelled);

    let mut rig = Rig::main();
    rig.command("auge!polling");
    rig.app.host.polling_update = Some(PollingUpdate::Cancelled);
    rig.app.on_tick(Instant::now());
    assert!(rig.screen_is_main());
    assert!(rig.text().contains("cancelled"));
}

/// Cells of a row, other than ones that carry their own colour on purpose, such as the danger pill.
fn row_backgrounds(rig: &mut Rig, y: u16, x: u16, width: u16) -> Vec<ratatui::style::Color> {
    rig.draw();
    let buffer = rig.terminal.backend().buffer();
    (x..x + width)
        .map(|column| buffer[(column, y)].bg)
        .collect()
}

fn hit_rect(rig: &Rig, wanted: &Hit) -> ratatui::layout::Rect {
    rig.app
        .hits
        .iter()
        .find(|(_, hit)| hit == wanted)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("{wanted:?} isn't on screen"))
}

fn assert_filled(rig: &mut Rig, wanted: &Hit, color: ratatui::style::Color, context: &str) {
    let rect = hit_rect(rig, wanted);
    let backgrounds = row_backgrounds(rig, rect.y, rect.x, rect.width);
    assert!(
        backgrounds.iter().all(|bg| *bg == color),
        "{context}: {wanted:?} should be filled with {color:?}, got {backgrounds:?}"
    );
    // A tweak's second line is filled too, apart from its danger pill.
    if rect.height > 1 {
        let pills: Vec<_> = (0..5).map(theme::danger_color).collect();
        let backgrounds = row_backgrounds(rig, rect.y + 1, rect.x, rect.width);
        assert!(
            backgrounds
                .iter()
                .all(|bg| *bg == color || pills.contains(bg)),
            "{context}: the second line of {wanted:?} has a gap: {backgrounds:?}"
        );
    }
    // Reverse video looked different from terminal to terminal, so it must not be back.
    let buffer = rig.terminal.backend().buffer();
    for column in rect.x..rect.x + rect.width {
        assert!(
            !buffer[(column, rect.y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED),
            "{context}: {wanted:?} uses reverse video"
        );
    }
}

/// The user reported rows that didn't highlight, such as gpu#2, depending on how they got there.
#[test]
fn every_selected_row_is_filled_edge_to_edge_however_it_was_reached() {
    let mut rig = Rig::main();
    rig.select_system();
    let count = rig.app.tweaks().len();
    assert!(count > 1);

    // By keyboard, with the list focused.
    for index in 0..count {
        rig.app.pane = Pane::Tweaks;
        rig.key(KeyCode::Home);
        for _ in 0..index {
            rig.key(KeyCode::Down);
        }
        assert_eq!(rig.app.tweak, index);
        assert_filled(&mut rig, &Hit::Tweak(index), theme::IRIS, "keyboard");
        for other in (0..count).filter(|o| *o != index) {
            let rect = hit_rect(&rig, &Hit::Tweak(other));
            let backgrounds = row_backgrounds(&mut rig, rect.y, rect.x, rect.width);
            assert!(
                !backgrounds.contains(&theme::IRIS),
                "row {other} is lit while {index} is selected"
            );
        }
    }

    // By mouse, which also moves focus to the list.
    for index in (0..count).rev() {
        rig.click(&Hit::Tweak(index));
        assert_filled(&mut rig, &Hit::Tweak(index), theme::IRIS, "mouse");
    }

    // With focus elsewhere it stays visible, just quieter.
    for pane_key in [KeyCode::BackTab, KeyCode::BackTab] {
        rig.key(pane_key);
        assert_filled(&mut rig, &Hit::Tweak(0), theme::SURFACE_HIGH, "unfocused");
    }
}

#[test]
fn the_selected_component_is_always_filled_too() {
    let mut rig = Rig::main();
    rig.key(KeyCode::BackTab);
    assert_eq!(rig.app.pane, Pane::Components);
    for index in 0..10 {
        rig.key(KeyCode::Home);
        for _ in 0..index {
            rig.key(KeyCode::Down);
        }
        assert_filled(&mut rig, &Hit::Component(index), theme::IRIS, "keyboard");
    }
    // Focus moves to the list, and the menu's choice stays lit.
    rig.key(KeyCode::Tab);
    assert_filled(
        &mut rig,
        &Hit::Component(9),
        theme::SURFACE_HIGH,
        "unfocused",
    );

    for index in [3, 0, 7] {
        rig.click(&Hit::Component(index));
        assert_filled(&mut rig, &Hit::Component(index), theme::IRIS, "mouse");
    }
}

#[test]
fn hovering_lights_up_what_is_under_the_mouse_without_doing_anything() {
    let mut rig = Rig::main();
    rig.select_system();
    let target = Hit::Tweak(2);
    let (x, y) = rig.where_is(&target);
    rig.mouse(MouseEventKind::Moved, x, y);
    assert_eq!(rig.app.hover, Some(target.clone()));
    assert_filled(&mut rig, &target, theme::SURFACE, "hover");
    assert_eq!(rig.app.tweak, 0, "moving the mouse must not select");
    assert!(rig.screen_is_main());

    let (x, y) = rig.where_is(&Hit::Button(Button::Quit));
    assert_eq!(rig.mouse(MouseEventKind::Moved, x, y), Flow::Continue);
    assert_eq!(rig.app.hover, Some(Hit::Button(Button::Quit)));
}

#[test]
fn drawing_slows_down_when_nobody_is_looking_or_while_measuring() {
    let mut rig = Rig::bench_rig();
    let fast = rig.app.tick_interval();
    assert!(fast <= Duration::from_millis(100));

    // Behind a game, for example.
    rig.app.on_event(Event::FocusLost, rig.now);
    let background = rig.app.tick_interval();
    assert!(background > fast);
    rig.app.on_event(Event::FocusGained, rig.now);
    assert_eq!(rig.app.tick_interval(), fast);

    // A click counts as being looked at, in case a focus event was missed.
    rig.app.on_event(Event::FocusLost, rig.now);
    rig.mouse(MouseEventKind::Moved, 1, 1);
    assert_eq!(rig.app.tick_interval(), fast);

    rig.command("auge!bench start");
    rig.key(KeyCode::Enter);
    rig.app.on_tick(Instant::now() + Duration::from_secs(11));
    assert!(rig.text().contains("Capturing"));
    assert_eq!(rig.app.tick_interval(), Duration::from_secs(1));

    // The polling test's window is in front, but its timer here should still move.
    let mut rig = Rig::main();
    rig.command("auge!polling");
    rig.app.on_event(Event::FocusLost, rig.now);
    assert_eq!(rig.app.tick_interval(), fast);
}

#[test]
fn animation_follows_time_rather_than_events_and_changes_no_words() {
    let mut rig = Rig::main();
    rig.select_system();
    let before = rig.text();
    let start = Instant::now();
    rig.app.on_tick(start + Duration::from_secs(2));
    let frame = rig.app.frame;
    assert!(frame >= 30, "{frame}");
    rig.key(KeyCode::Char('r'));
    assert_eq!(rig.app.frame, frame, "a key press isn't a frame");
    rig.key(KeyCode::Esc);
    // Colours move, but the text on screen doesn't.
    let after: String = rig.text().replace("State read again.", "");
    let strip = |text: &str| -> Vec<String> {
        text.lines()
            .map(|l| l.chars().filter(|c| c.is_alphanumeric()).collect())
            .collect()
    };
    let (before_rows, after_rows) = (strip(&before), strip(&after));
    assert!(before_rows.iter().any(|r| r.contains("Titlesafe")));
    assert!(after_rows.iter().any(|r| r.contains("Titlesafe")));
}

#[test]
fn the_command_bar_completes_with_tab_and_shows_what_it_would_add() {
    let mut rig = Rig::main();
    rig.key(KeyCode::Char('/'));
    rig.typed("auge!pol");
    assert_eq!(rig.app.completion().as_deref(), Some("ling"));
    assert!(rig.text().contains("auge!polling"));
    rig.key(KeyCode::Tab);
    assert_eq!(rig.app.bar, "auge!polling");
    assert_eq!(rig.app.completion(), None);

    // Nothing to complete means nothing changes.
    rig.key(KeyCode::Esc);
    rig.key(KeyCode::Char('/'));
    rig.typed("nonsense");
    rig.key(KeyCode::Tab);
    assert_eq!(rig.app.bar, "nonsense");
}

/// The cell the command bar's cursor should be on, after this many typed characters.
fn bar_cell(rig: &mut Rig, typed: u16) -> (String, ratatui::style::Color) {
    rig.draw();
    let bar = hit_rect(rig, &Hit::Bar);
    let cell = &rig.terminal.backend().buffer()[(bar.x + 1 + typed, bar.y + 1)];
    (cell.symbol().to_string(), cell.bg)
}

/// The cursor used to sit after the suggestion, away from where typing would really go.
#[test]
fn the_command_cursor_stays_right_after_the_typed_text() {
    let mut rig = Rig::main();
    rig.key(KeyCode::Char('/'));
    assert_eq!(bar_cell(&mut rig, 0), ("a".to_string(), theme::IRIS));

    // Over the suggestion's next character, which doesn't move.
    rig.typed("auge!d");
    assert_eq!(rig.app.completion().as_deref(), Some("ry "));
    assert_eq!(bar_cell(&mut rig, 6), ("r".to_string(), theme::IRIS));
    let (next, background) = bar_cell(&mut rig, 7);
    assert_eq!(next, "y");
    assert_ne!(background, theme::IRIS);

    // Deleting brings it back with the text.
    rig.key(KeyCode::Backspace);
    let expected = rig.app.completion().unwrap();
    let first = expected.chars().next().unwrap().to_string();
    assert_eq!(bar_cell(&mut rig, 5), (first, theme::IRIS));

    // Past anything that could be suggested, it follows the text.
    rig.typed("zzz");
    assert_eq!(rig.app.completion(), None);
    assert_eq!(bar_cell(&mut rig, 8), (" ".to_string(), theme::IRIS));

    // When it blinks off, the suggestion still shows, and nothing moves.
    rig.key(KeyCode::Esc);
    rig.key(KeyCode::Char('/'));
    rig.typed("auge!d");
    rig.app.frame = 12;
    let (under, background) = bar_cell(&mut rig, 6);
    assert_eq!(under, "r");
    assert_ne!(background, theme::IRIS);
}

#[test]
fn counts_of_applied_tweaks_appear_in_the_menu() {
    let mut rig = Rig::main();
    rig.command("auge!system#1");
    rig.skip_restore_offer();
    rig.key(KeyCode::Enter);
    rig.key(KeyCode::Esc);
    assert!(rig.text().contains("1/6"));
    assert!(rig.text().contains("1 applied"));
}

#[test]
fn changes_are_logged_and_the_log_can_be_read() {
    let mut rig = Rig::main();
    rig.command("auge!system#1");
    rig.skip_restore_offer();
    rig.key(KeyCode::Enter);
    assert!(
        rig.app.host.logged.iter().any(|l| l.contains("safe")),
        "{:?}",
        rig.app.host.logged
    );
    rig.key(KeyCode::Esc);
    rig.command("auge!log");
    assert!(rig.text().contains("safe"));
}

#[test]
fn narrow_windows_open_details_instead_of_side_by_side() {
    let mut rig = Rig::with(true, true, None, (90, 30));
    rig.select_system();
    assert!(!rig.text().contains("Summary of safe."));
    rig.key(KeyCode::Enter);
    assert!(rig.text().contains("Summary of safe."));
    assert!(rig.world.writes().is_empty());
}

#[test]
fn every_screen_fits_the_minimum_size_without_panicking() {
    let mut rig = Rig::with(false, true, None, (80, 24));
    let _ = rig.text();
    rig.click_button(Button::Accept);
    let _ = rig.text();
    rig.command("auge!system");
    rig.key(KeyCode::Char('d'));
    let _ = rig.text();
    rig.key(KeyCode::Esc);
    rig.command("auge!help");
    let _ = rig.text();
    rig.key(KeyCode::Esc);
    rig.command("auge!reboot");
    let _ = rig.text();
    rig.key(KeyCode::Esc);
    rig.command("auge!system#3");
    let _ = rig.text();
    rig.key(KeyCode::Esc);
    let _ = rig.text();
    assert!(rig.world.writes().is_empty());
}

/// Writes screens to HTML with their real colours, for looking at without a terminal. It only runs
/// when asked: `AUGEO_RENDER_DIR=<folder> cargo test -p augeo-tui render_screens -- --ignored`.
#[test]
#[ignore = "writes files for a person to look at"]
fn render_screens_for_review() {
    use ratatui::style::{Color, Modifier};

    fn css(color: Color, fallback: &str) -> String {
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => fallback.to_string(),
        }
    }

    fn html(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut out = String::from("<pre>");
        for y in 0..buffer.area.height {
            let mut run = String::new();
            let mut style = None;
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                let bold = cell.modifier.contains(Modifier::BOLD);
                let current = (cell.fg, cell.bg, bold);
                if style != Some(current) {
                    if let Some((fg, bg, bold)) = style {
                        let weight = if bold { "700" } else { "400" };
                        out.push_str(&format!(
                            "<span style=\"color:{};background:{};font-weight:{weight}\">{run}</span>",
                            css(fg, "#e0def4"),
                            css(bg, "#191724")
                        ));
                    }
                    run.clear();
                    style = Some(current);
                }
                run.push_str(
                    &cell
                        .symbol()
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;"),
                );
            }
            if let Some((fg, bg, bold)) = style {
                let weight = if bold { "700" } else { "400" };
                out.push_str(&format!(
                    "<span style=\"color:{};background:{};font-weight:{weight}\">{run}</span>",
                    css(fg, "#e0def4"),
                    css(bg, "#191724")
                ));
            }
            out.push('\n');
        }
        out.push_str("</pre>");
        out
    }

    let Some(dir) = std::env::var_os("AUGEO_RENDER_DIR") else {
        return;
    };

    fn real_rig(size: (u16, u16), accepted: bool, writer: bool) -> Rig {
        let world = World::new();
        let app = App::new(
            Engine::open(
                world.system(),
                world.store(),
                world.clock(),
                Catalog::embedded().unwrap(),
                context(),
                writer,
            )
            .unwrap(),
            Box::new(|| panic!("not used here")),
            FakeHost {
                accepted,
                ..FakeHost::default()
            },
            "NVIDIA GeForce RTX 4070 SUPER | driver 610.88 | Windows 11 26100 | ? for help"
                .to_string(),
            None,
        );
        let mut rig = Rig {
            world,
            app,
            terminal: Terminal::new(TestBackend::new(size.0, size.1)).unwrap(),
            now: Instant::now(),
        };
        rig.draw();
        rig
    }

    let mut page = String::from(
        "<!doctype html><meta charset=utf-8><title>augeo screens</title><style>\
         body{background:#0d0c14;color:#ccc;font-family:sans-serif;margin:24px}\
         h2{font-size:14px;margin:24px 0 8px}\
         pre{font-family:'Cascadia Mono',Consolas,monospace;font-size:13px;line-height:1.15;\
         display:inline-block;margin:0;background:#191724}\
         </style>",
    );
    let mut add = |name: &str, rig: &mut Rig| {
        rig.draw();
        page.push_str(&format!("<h2>{name}</h2>{}", html(&rig.terminal)));
    };

    let mut rig = real_rig((120, 30), false, true);
    add("First run, 120x30", &mut rig);

    let mut rig = real_rig((120, 30), true, false);
    rig.command("auge!gpu");
    rig.key(KeyCode::Down);
    add(
        "Main, read-only, gpu#2 selected with the list focused",
        &mut rig,
    );
    rig.key(KeyCode::BackTab);
    add("The same, with the menu focused", &mut rig);
    let (x, y) = rig.where_is(&Hit::Button(Button::Check));
    rig.mouse(MouseEventKind::Moved, x, y);
    add("Hovering a button", &mut rig);
    rig.key(KeyCode::Char('/'));
    rig.typed("auge!pol");
    add("Typing a command, with completion", &mut rig);

    let mut rig = real_rig((80, 24), true, true);
    rig.command("auge!system");
    rig.key(KeyCode::Down);
    add("Main at the minimum size, 80x24", &mut rig);

    let mut rig = real_rig((120, 30), true, true);
    rig.command("auge!dry system#1,2");
    add("Dry run", &mut rig);

    let mut rig = real_rig((120, 30), true, true);
    rig.command("auge!system#4");
    rig.skip_restore_offer();
    rig.typed("I acc");
    add("Confirming an expert tweak", &mut rig);

    let mut rig = real_rig((120, 30), true, true);
    rig.command("auge!system#1");
    add("Restore point offer", &mut rig);

    let mut rig = real_rig((120, 30), true, true);
    rig.app.host.polling_update = None;
    rig.command("auge!polling");
    add("Polling: waiting for the click", &mut rig);
    rig.app.host.polling_update = Some(PollingUpdate::Started);
    rig.app.on_tick(Instant::now() - Duration::from_secs(2));
    rig.app.on_tick(Instant::now());
    add("Polling: testing", &mut rig);

    let mut rig = real_rig((120, 30), true, true);
    rig.command("auge!check");
    add("Check report", &mut rig);

    let mut rig = real_rig((120, 30), true, true);
    rig.command("auge!myths");
    add("Myths", &mut rig);

    let mut rig = real_rig((120, 30), true, true);
    rig.command("auge!game game.exe");
    rig.command("auge!bench start");
    add("Benchmark: prepare", &mut rig);
    rig.key(KeyCode::Enter);
    add("Benchmark: countdown", &mut rig);
    rig.app.on_tick(Instant::now() + Duration::from_secs(11));
    add("Benchmark: capturing", &mut rig);

    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("screens.html"), page).unwrap();
}
