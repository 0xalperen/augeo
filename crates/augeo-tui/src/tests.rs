use std::time::{Duration, Instant};

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
use crate::{App, Flow, Host};

const SID: &str = "S-1-5-21-1-2-3-1001";

#[derive(Default, Debug)]
struct FakeHost {
    accepted: bool,
    elevate_answer: Option<bool>,
    elevations: Vec<String>,
    restarts: u32,
    restore_points: u32,
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
    Catalog::parse(&[("system", &system)]).unwrap()
}

fn context() -> Context {
    Context {
        env: Environment {
            build: 26100,
            nvidia: true,
            laptop: false,
            cpu_vendor: CpuVendor::Amd,
            dual_ccd_x3d: false,
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
        display_device: None,
        max_display_mode: None,
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
