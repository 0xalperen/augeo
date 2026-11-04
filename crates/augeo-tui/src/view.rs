//! Drawing. Every clickable thing is recorded here, so clicks always match what's on screen.

use augeo_catalog::schema::{AntiCheatStance, AntiCheatSystem, Effect, Evidence, Tweak};
use augeo_command::Component;
use augeo_engine::{TweakState, TweakStatusView, disclaimer};
use augeo_journal::Op;
use augeo_ports::{Clock, Store, System};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::{App, Button, ConfirmState, Hit, Host, Pane, Screen, ScrollArea};

const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 24;
const WIDE: u16 = 110;

fn danger_badge(danger: u8) -> (&'static str, Color) {
    match danger {
        0 => ("SAFE", Color::Green),
        1 => ("LOW", Color::Cyan),
        2 => ("MEDIUM", Color::Yellow),
        3 => ("HIGH", Color::LightRed),
        _ => ("EXPERT", Color::Red),
    }
}

fn evidence_short(evidence: Evidence) -> &'static str {
    match evidence {
        Evidence::Measured => "MEAS",
        Evidence::Documented => "DOC",
        Evidence::Community => "COMM",
        Evidence::Experimental => "EXP",
    }
}

fn evidence_long(evidence: Evidence) -> &'static str {
    match evidence {
        Evidence::Measured => "Measured: numbers from a credible test",
        Evidence::Documented => "Documented: the vendor documents this behaviour",
        Evidence::Community => "Community-reported: no controlled test",
        Evidence::Experimental => "Experimental: unverified, so measure it yourself",
    }
}

fn effect_text(effect: Effect) -> &'static str {
    match effect {
        Effect::Now => "takes effect now",
        Effect::App => "restart the game",
        Effect::Adapter => "the network adapter resets",
        Effect::Logoff => "needs signing out",
        Effect::Restart => "needs a restart",
    }
}

fn state_text(state: &TweakState) -> &'static str {
    match state {
        TweakState::NotApplied => "not applied",
        TweakState::Applied => "applied",
        TweakState::AlreadySet => "already set",
        TweakState::Partial => "partial",
        TweakState::ChangedExternally => "changed",
        TweakState::Unknown { .. } => "unknown",
    }
}

/// A short label for a tweak's state.
pub(crate) fn state_label(status: &Result<TweakStatusView, String>) -> String {
    match status {
        Ok(view) if view.in_doubt => "interrupted".to_string(),
        Ok(view) if view.restart_pending => format!("{}, restart", state_text(&view.state)),
        Ok(view) => state_text(&view.state).to_string(),
        Err(_) => "n/a".to_string(),
    }
}

fn system_name(system: AntiCheatSystem) -> &'static str {
    match system {
        AntiCheatSystem::Vanguard => "Vanguard",
        AntiCheatSystem::Faceit => "FACEIT",
        AntiCheatSystem::Eac => "Easy Anti-Cheat",
        AntiCheatSystem::Battleye => "BattlEye",
    }
}

/// The detail pane's text. Also used by `auge!info`.
pub(crate) fn detail_lines(
    tweak: &Tweak,
    status: Option<&Result<TweakStatusView, String>>,
    option: Option<&String>,
) -> Vec<String> {
    let (danger, _) = danger_badge(tweak.danger);
    let mut lines = vec![
        format!("Danger: {danger}"),
        format!("Evidence: {}", evidence_long(tweak.evidence)),
        format!("When: {}", effect_text(tweak.effect)),
    ];
    match status {
        Some(Ok(view)) => {
            lines.push(format!("State: {}", state_text(&view.state)));
            if let TweakState::Unknown { reason } = &view.state {
                lines.push(format!("  Couldn't read it: {reason}"));
            }
            if view.in_doubt {
                lines.push("  An interrupted change needs resolving.".to_string());
            }
            if view.restart_pending {
                lines.push("  A restart is needed for this to take effect.".to_string());
            }
            if view.older_revision {
                lines.push("  Applied by an older definition. Undo it to use the new one.".into());
            }
            if view.baseline_lost {
                lines.push("  The original value was lost when the journal was damaged.".into());
            }
            if view.rollback_failed {
                lines.push("  A rollback failed. Run undo to retry.".to_string());
            }
            if let Some(note) = &view.drift_note {
                lines.push(format!("  {note}"));
            }
        }
        Some(Err(reason)) => lines.push(format!("State: not available. {reason}")),
        None => lines.push("State: pick a game first, with auge!game cs2.exe".to_string()),
    }
    if !tweak.option.is_empty() {
        let chosen = option.map(String::as_str).or_else(|| {
            tweak
                .option
                .iter()
                .find(|o| o.recommended)
                .map(|o| o.key.as_str())
        });
        let names: Vec<String> = tweak
            .option
            .iter()
            .map(|o| {
                if Some(o.key.as_str()) == chosen {
                    format!("[{}]", o.name)
                } else {
                    o.name.clone()
                }
            })
            .collect();
        lines.push(format!("Option (press o): {}", names.join(" ")));
    }
    lines.push(String::new());
    lines.push(tweak.summary.clone());
    lines.push(String::new());
    lines.push("Risks:".to_string());
    lines.extend(tweak.risks.iter().map(|r| format!("  - {r}")));
    lines.push(String::new());
    lines.push(format!("Undo: {}", tweak.undo_note));
    lines.push(String::new());
    lines.push("Anti-cheat: untested unless listed here. Nothing here is promised safe.".into());
    for note in &tweak.anticheat {
        let stance = match note.stance {
            AntiCheatStance::Untested => "untested",
            AntiCheatStance::KnownIssue => "known issue",
            AntiCheatStance::RequiredByVendor => "the vendor requires the default",
        };
        lines.push(format!(
            "  - {}: {stance}. {}",
            system_name(note.system),
            note.note
        ));
    }
    lines.push(String::new());
    lines.push("Sources:".to_string());
    for source in &tweak.sources {
        lines.push(format!("  - {}", source.claim));
        lines.push(format!("    {}", source.url));
        if let Some(measured) = &source.measured {
            lines.push(format!(
                "    Measured: {} ({}, n={})",
                measured.effect, measured.hw, measured.n
            ));
        }
    }
    lines
}

fn block(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(title)
}

/// Lays buttons out left to right, recording where each one landed.
struct ButtonRow {
    spans: Vec<Span<'static>>,
    x: u16,
    y: u16,
    limit: u16,
    hits: Vec<(Rect, Hit)>,
}

impl ButtonRow {
    fn new(area: Rect) -> Self {
        Self {
            spans: Vec::new(),
            x: area.x,
            y: area.y,
            limit: area.x + area.width,
            hits: Vec::new(),
        }
    }

    fn add(&mut self, label: &str, button: Button, style: Style) {
        let text = format!("[{label}]");
        let width = text.chars().count() as u16;
        if self.x + width > self.limit {
            return;
        }
        self.hits
            .push((Rect::new(self.x, self.y, width, 1), Hit::Button(button)));
        self.spans.push(Span::styled(text, style));
        self.spans.push(Span::raw(" "));
        self.x += width + 1;
    }

    fn finish(self, frame: &mut Frame<'_>, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
        frame.render_widget(Paragraph::new(Line::from(self.spans)), area);
        hits.extend(self.hits);
    }
}

impl<S: System, T: Store, C: Clock, H: Host> App<S, T, C, H> {
    /// Draws the current screen, and records every clickable and scrollable region.
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        self.hits.clear();
        self.scrolls.clear();
        let area = frame.area();
        frame.render_widget(Clear, area);
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            frame.render_widget(
                Paragraph::new(format!(
                    "Please make this window at least {MIN_WIDTH}x{MIN_HEIGHT}. It is {}x{}.",
                    area.width, area.height
                ))
                .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        self.wide = area.width >= WIDE;
        match self.screen.clone() {
            Screen::Disclaimer { typing } => self.draw_disclaimer(frame, area, &typing),
            Screen::Recovery { items, message } => self.draw_recovery(frame, area, &items, message),
            Screen::Main => self.draw_main(frame, area),
            Screen::Confirm(state) => self.draw_confirm(frame, area, &state),
            Screen::RestoreOffer { .. } => self.draw_restore_offer(frame, area),
            Screen::Elevate { command } => self.draw_elevate(frame, area, &command),
            Screen::Text {
                title,
                lines,
                scroll,
            } => {
                self.draw_text(frame, area, &title, &lines, scroll);
            }
            Screen::RestartPrompt => self.draw_restart(frame, area),
            Screen::KeepDisplay { deadline, .. } => self.draw_keep_display(frame, area, deadline),
        }
    }

    fn draw_keep_display(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        deadline: std::time::Instant,
    ) {
        let [body, buttons] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(area);
        let left = deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_secs();
        frame.render_widget(
            Paragraph::new(format!(
                "Can you see this clearly? The display mode goes back by itself in {left} seconds \
                 unless you keep it."
            ))
            .wrap(Wrap { trim: true })
            .block(block("Keep this display mode?", true)),
            body,
        );
        let mut row = ButtonRow::new(buttons);
        row.add(
            "Keep it (Enter)",
            Button::KeepDisplay,
            Style::default().fg(Color::Green),
        );
        row.add("Put it back (Esc)", Button::RevertDisplay, Style::default());
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_disclaimer(&mut self, frame: &mut Frame<'_>, area: Rect, typing: &str) {
        let [body, input, buttons] = Layout::vertical([
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(area);
        let text = vec![
            Line::from("Please read this before using augeo."),
            Line::from(""),
            Line::from(disclaimer::TEXT),
        ];
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: true })
                .block(block("Disclaimer", true)),
            body,
        );
        frame.render_widget(
            Paragraph::new(typing.to_string()).block(block("Type accept and press Enter", false)),
            input,
        );
        let mut row = ButtonRow::new(buttons);
        row.add(
            "I accept",
            Button::Accept,
            Style::default().fg(Color::Green),
        );
        row.add("Decline and quit", Button::Decline, Style::default());
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_recovery(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        items: &[augeo_engine::InDoubt],
        message: Option<String>,
    ) {
        let [body, buttons] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(area);
        let mut lines = vec![
            Line::from("augeo was interrupted while changing your system. Nothing else can run"),
            Line::from("until this is resolved. These values were read just now:"),
            Line::from(""),
        ];
        let mut op = Op::Apply;
        if let Some(item) = items.first() {
            op = item.op;
            let verb = if item.op == Op::Apply {
                "Applying"
            } else {
                "Undoing"
            };
            lines.push(Line::from(format!("{verb}: {}", item.title)));
            for r in &item.resources {
                let live = r
                    .live
                    .as_ref()
                    .map_or("couldn't read".to_string(), ToString::to_string);
                lines.push(Line::from(format!("  {}", r.address)));
                lines.push(Line::from(format!(
                    "    before: {}   augeo's: {}   now: {live}",
                    r.baseline, r.desired
                )));
            }
        }
        if let Some(message) = message {
            lines.push(Line::from(""));
            lines.extend(message.lines().map(|l| Line::from(l.to_string())));
        }
        if !self.engine.is_writer() {
            lines.push(Line::from(""));
            lines.push(Line::from("Resolving this needs elevation."));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(block("Interrupted change", true)),
            body,
        );
        let mut row = ButtonRow::new(buttons);
        if op == Op::Apply {
            row.add(
                "Roll back (Enter)",
                Button::RollBack,
                Style::default().fg(Color::Green),
            );
            row.add("Keep what was done (k)", Button::Keep, Style::default());
        } else {
            row.add(
                "Finish the undo (Enter)",
                Button::FinishRevert,
                Style::default().fg(Color::Green),
            );
        }
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_main(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let message_height = self
            .message
            .as_ref()
            .map_or(0, |m| m.lines().count().min(4) as u16);
        let [header, body, buttons, message, bar, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(1),
            Constraint::Length(message_height),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(area);

        self.draw_header(frame, header);

        let (lists, detail) = if self.wide {
            let [l, d] =
                Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
                    .areas(body);
            (l, Some(d))
        } else {
            (body, None)
        };
        let [components, tweaks] =
            Layout::horizontal([Constraint::Length(16), Constraint::Min(30)]).areas(lists);
        self.draw_components(frame, components);
        self.draw_tweaks(frame, tweaks);
        if let Some(detail) = detail {
            self.draw_detail(frame, detail);
        }

        let mut row = ButtonRow::new(buttons);
        let plain = Style::default();
        row.add("Apply a", Button::Apply, Style::default().fg(Color::Green));
        row.add("Dry run d", Button::Dry, plain);
        row.add("Undo u", Button::Undo, plain);
        row.add("Status", Button::Status, plain);
        row.add("Check", Button::Check, plain);
        row.add("Bench", Button::Bench, plain);
        row.add("Myths", Button::Myths, plain);
        row.add("Help ?", Button::Help, plain);
        row.add("Quit q", Button::Quit, plain);
        row.finish(frame, buttons, &mut self.hits);

        if let Some(text) = &self.message {
            frame.render_widget(
                Paragraph::new(text.clone()).style(Style::default().fg(Color::Yellow)),
                message,
            );
        }

        let bar_focused = self.pane == Pane::Bar;
        let content = if self.bar.is_empty() && !bar_focused {
            Span::styled(
                "Press / to type a command, like auge!cpu#1",
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::raw(format!(
                "{}{}",
                self.bar,
                if bar_focused { "_" } else { "" }
            ))
        };
        frame.render_widget(
            Paragraph::new(Line::from(content)).block(block("Command", bar_focused)),
            bar,
        );
        self.hits.push((bar, Hit::Bar));

        frame.render_widget(Paragraph::new(self.environment_line.clone()), status);
    }

    fn draw_header(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let mut spans = vec![Span::styled(
            format!("augeo {} ", env!("CARGO_PKG_VERSION")),
            Style::default().add_modifier(Modifier::BOLD),
        )];
        let mut x = area.x + spans[0].width() as u16;
        if self.engine.is_writer() {
            spans.push(Span::styled(
                "ELEVATED ",
                Style::default().fg(Color::Yellow),
            ));
        } else {
            let label = "READ-ONLY [Elevate e]";
            spans.push(Span::styled(label, Style::default().fg(Color::Cyan)));
            self.hits.push((
                Rect::new(x, area.y, label.len() as u16, 1),
                Hit::Button(Button::Elevate),
            ));
            spans.push(Span::raw(" "));
            x += label.len() as u16 + 1;
        }
        let _ = x;
        if self.engine.open_report().history_lost {
            spans.push(Span::styled(
                "JOURNAL DAMAGED: some history was lost ",
                Style::default().fg(Color::Red),
            ));
        }
        let pending = self.pending_restart_count();
        if pending > 0 {
            spans.push(Span::styled(
                format!("{pending} change(s) need a restart"),
                Style::default().fg(Color::Yellow),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_components(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let focused = self.pane == Pane::Components;
        let outer = block("Components", focused);
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        self.scrolls.push((area, ScrollArea::Components));
        let mut lines = Vec::new();
        for (i, component) in Component::ALL.iter().enumerate() {
            let mark = if i == self.component { ">" } else { " " };
            let text = format!(
                "{mark}{:<9}{:>3}",
                component.as_str(),
                self.tweak_count(*component)
            );
            let style = if i == self.component {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(text, style)));
            if (i as u16) < inner.height {
                self.hits.push((
                    Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                    Hit::Component(i),
                ));
            }
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_tweaks(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let focused = self.pane == Pane::Tweaks;
        let title = match (self.current_component(), &self.current_game) {
            (Component::Game, Some(exe)) => format!("game: {exe}"),
            (component, _) => component.to_string(),
        };
        let outer = block(&title, focused);
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        self.scrolls.push((area, ScrollArea::Tweaks));

        let tweaks: Vec<Tweak> = self.tweaks().into_iter().cloned().collect();
        if tweaks.is_empty() {
            frame.render_widget(
                Paragraph::new("No tweaks here yet. Dropped ones are on the Myths screen.")
                    .wrap(Wrap { trim: true }),
                inner,
            );
            return;
        }
        let height = inner.height as usize;
        if self.tweak < self.tweak_scroll {
            self.tweak_scroll = self.tweak;
        } else if height > 0 && self.tweak >= self.tweak_scroll + height {
            self.tweak_scroll = self.tweak + 1 - height;
        }
        let mut lines = Vec::new();
        for (row, (index, tweak)) in tweaks
            .iter()
            .enumerate()
            .skip(self.tweak_scroll)
            .take(height)
            .enumerate()
        {
            let tick = if self.is_ticked(tweak) { "[x]" } else { "[ ]" };
            let (danger, color) = danger_badge(tweak.danger);
            let repair = if Self::is_repair(tweak) { "R" } else { " " };
            let applicable = self.applicability(tweak);
            let state = match &applicable {
                Err(_) => "n/a".to_string(),
                Ok(()) => self
                    .status_of(tweak)
                    .map_or("pick game".to_string(), state_label),
            };
            let right = format!(
                " {repair} {danger:<6} {:<4} {state:<13}",
                evidence_short(tweak.evidence)
            );
            let left_width = (inner.width as usize)
                .saturating_sub(right.chars().count())
                .saturating_sub(tick.len() + 5);
            let title: String = tweak.title.chars().take(left_width).collect();
            let highlight = index == self.tweak;
            let base = if highlight {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if applicable.is_err() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{tick} {:>3} {title:<left_width$}", tweak.number),
                    base,
                ),
                Span::styled(
                    right,
                    base.fg(if applicable.is_err() {
                        Color::DarkGray
                    } else {
                        color
                    }),
                ),
            ]));
            let y = inner.y + row as u16;
            self.hits
                .push((Rect::new(inner.x, y, inner.width, 1), Hit::Tweak(index)));
            self.hits
                .push((Rect::new(inner.x, y, 3, 1), Hit::TweakBox(index)));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_detail(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let focused = self.pane == Pane::Detail;
        self.scrolls.push((area, ScrollArea::Detail));
        let Some(tweak) = self.highlighted().cloned() else {
            frame.render_widget(block("Details", focused), area);
            return;
        };
        let mut lines: Vec<String> = Vec::new();
        if let Err(reason) = self.applicability(&tweak) {
            lines.push(format!("Doesn't apply here: {reason}"));
            lines.push(String::new());
        }
        lines.extend(detail_lines(
            &tweak,
            self.status_of(&tweak),
            self.options.get(&tweak.id),
        ));
        let text: Vec<Line> = lines.into_iter().map(Line::from).collect();
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((self.detail_scroll, 0))
                .block(block(&tweak.title, focused)),
            area,
        );
    }

    fn plan_lines(&self, state: &ConfirmState) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for planned in &state.plan.tweaks {
            let (danger, color) = danger_badge(planned.danger);
            lines.push(Line::from(vec![
                Span::styled(
                    planned.title.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(danger, Style::default().fg(color)),
                Span::raw(format!("  {}", effect_text(planned.effect))),
            ]));
            for step in &planned.steps {
                lines.push(Line::from(format!("  {}", step.address)));
                let note = if step.baseline_lost {
                    "    The original value was lost, so this stays as it is.".to_string()
                } else if step.drift && !step.write {
                    format!(
                        "    Changed by something else to {}. Kept as it is.",
                        step.current
                    )
                } else if step.write {
                    format!("    {}  ->  {}", step.current, step.target)
                } else {
                    format!("    {}  (no change needed)", step.current)
                };
                lines.push(Line::from(note));
            }
            if state.plan.op == Op::Apply {
                let tweak = self
                    .engine
                    .catalog()
                    .tweaks
                    .iter()
                    .find(|t| t.id == planned.tweak_id);
                if let Some(tweak) = tweak {
                    lines.push(Line::from(format!(
                        "  Evidence: {}",
                        evidence_long(tweak.evidence)
                    )));
                    lines.push(Line::from("  Risks:"));
                    for risk in &tweak.risks {
                        lines.push(Line::from(format!("    - {risk}")));
                    }
                    for note in &tweak.anticheat {
                        lines.push(Line::from(format!(
                            "  {}: {}",
                            system_name(note.system),
                            note.note
                        )));
                    }
                    lines.push(Line::from(format!("  Undo: {}", tweak.undo_note)));
                }
            }
            if !planned.writes() {
                lines.push(Line::from("  Nothing to change."));
            }
            lines.push(Line::from(""));
        }
        lines
    }

    fn draw_confirm(&mut self, frame: &mut Frame<'_>, area: Rect, state: &ConfirmState) {
        let needs_input = state.current_word().is_some();
        let needs_tick = state.needs_tick();
        let has_drift = state.undo_instances.is_some()
            && state
                .plan
                .tweaks
                .iter()
                .flat_map(|t| &t.steps)
                .any(|s| s.drift);
        let [body, message, input, tick, drift, buttons] = Layout::vertical([
            Constraint::Min(5),
            Constraint::Length(u16::from(self.message.is_some())),
            Constraint::Length(if needs_input { 3 } else { 0 }),
            Constraint::Length(u16::from(needs_tick)),
            Constraint::Length(u16::from(has_drift)),
            Constraint::Length(1),
        ])
        .areas(area);

        let title = if state.dry {
            "Dry run: nothing will change"
        } else if state.plan.op == Op::Apply {
            "These exact changes will be made"
        } else {
            "Undo: these exact changes will be made"
        };
        frame.render_widget(
            Paragraph::new(self.plan_lines(state))
                .wrap(Wrap { trim: false })
                .scroll((state.scroll, 0))
                .block(block(title, true)),
            body,
        );
        self.scrolls.push((body, ScrollArea::Body));

        if let Some(text) = &self.message {
            frame.render_widget(
                Paragraph::new(text.clone()).style(Style::default().fg(Color::Yellow)),
                message,
            );
        }
        if let Some(word) = state.current_word() {
            let instance = state.pending.first().map_or("", |(i, _)| i.as_str());
            let label = format!("{instance}: type \"{word}\" and press Enter");
            frame.render_widget(
                Paragraph::new(format!("{}_", state.typing)).block(block(&label, true)),
                input,
            );
        }
        if needs_tick {
            let mark = if state.ticked { "[x]" } else { "[ ]" };
            frame.render_widget(
                Paragraph::new(format!("{mark} I read the risks (Space)")),
                tick,
            );
            self.hits.push((tick, Hit::Tick));
        }
        if has_drift {
            let mark = if state.drift_restore { "[x]" } else { "[ ]" };
            let mut row = ButtonRow::new(drift);
            row.add(
                &format!("{mark} Overwrite values changed by something else (x)"),
                Button::DriftToggle,
                Style::default(),
            );
            row.finish(frame, drift, &mut self.hits);
        }

        let mut row = ButtonRow::new(buttons);
        let focused = Style::default().add_modifier(Modifier::REVERSED);
        if state.dry {
            row.add("Close (Enter)", Button::Cancel, focused);
        } else {
            let cancel = if state.focus_confirm {
                Style::default()
            } else {
                focused
            };
            row.add("Cancel (Esc)", Button::Cancel, cancel);
            let label = if state.plan.op == Op::Apply {
                "Apply"
            } else {
                "Undo"
            };
            if state.ready() {
                let style = if state.focus_confirm {
                    focused
                } else {
                    Style::default()
                };
                row.add(label, Button::Confirm, style.fg(Color::Green));
            } else {
                // Shown but not clickable until the requirement above is met.
                row.spans.push(Span::styled(
                    format!("[{label}: not yet]"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_restore_offer(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let [body, buttons] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(area);
        frame.render_widget(
            Paragraph::new(
                "This is the first change augeo will make here. A System Restore point is an \
                 extra safety net. augeo's own undo doesn't depend on it.\n\nWindows may silently \
                 skip creating one if another was made in the last 24 hours, and System Protection \
                 is off by default. augeo will tell you what really happened.",
            )
            .wrap(Wrap { trim: true })
            .block(block("Create a restore point first?", true)),
            body,
        );
        let mut row = ButtonRow::new(buttons);
        row.add(
            "Create one (Enter)",
            Button::CreateRestorePoint,
            Style::default().fg(Color::Green),
        );
        row.add("Skip (Esc)", Button::SkipRestorePoint, Style::default());
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_elevate(&mut self, frame: &mut Frame<'_>, area: Rect, command: &str) {
        let [body, buttons] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(area);
        let target = if command.is_empty() {
            "It will open in a new window.".to_string()
        } else {
            format!("It will open in a new window at: {command}")
        };
        frame.render_widget(
            Paragraph::new(format!(
                "Changing system settings needs administrator rights. Windows will ask for \
                 permission. {target}\n\nYou'll still see the exact changes and confirm them \
                 there. Nothing changes until you do."
            ))
            .wrap(Wrap { trim: true })
            .block(block("Elevation needed", true)),
            body,
        );
        let mut row = ButtonRow::new(buttons);
        row.add(
            "Press Enter to continue, Esc to cancel",
            Button::Cancel,
            Style::default(),
        );
        row.finish(frame, buttons, &mut self.hits);
        // The whole row is one target for Cancel, so a stray click can't trigger the UAC prompt.
    }

    fn draw_text(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        title: &str,
        lines: &[String],
        scroll: u16,
    ) {
        let [body, buttons] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(area);
        let text: Vec<Line> = lines.iter().map(|l| Line::from(l.clone())).collect();
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .block(block(title, true)),
            body,
        );
        self.scrolls.push((body, ScrollArea::Body));
        let mut row = ButtonRow::new(buttons);
        row.add("Close (Esc)", Button::Close, Style::default());
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_restart(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let [body, buttons] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(area);
        let pending = self.pending_restart_count();
        frame.render_widget(
            Paragraph::new(format!(
                "{pending} applied change(s) are waiting for a restart. Save your work first. \
                 Windows will restart right away."
            ))
            .wrap(Wrap { trim: true })
            .block(block("Restart now?", true)),
            body,
        );
        let mut row = ButtonRow::new(buttons);
        row.add("Not now (Esc)", Button::Close, Style::default());
        row.add(
            "Restart now (y)",
            Button::Restart,
            Style::default().fg(Color::Yellow),
        );
        row.finish(frame, buttons, &mut self.hits);
    }
}
