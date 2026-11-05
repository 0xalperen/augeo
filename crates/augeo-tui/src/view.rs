//! Drawing. Every clickable thing is recorded here, so clicks always match what's on screen.

use augeo_catalog::schema::{AntiCheatStance, AntiCheatSystem, Effect, Evidence, Tweak};
use augeo_command::{Component, parse};
use augeo_engine::{InDoubt, TweakState, TweakStatusView, disclaimer};
use augeo_journal::Op;
use augeo_ports::{Clock, Store, System};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    style::{Color, Modifier},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};

use crate::app::{App, Button, ConfirmState, Hit, Host, Pane, Screen, ScrollArea};
use crate::bench::{BenchPhase, BenchSession, RUN_SECONDS};
use crate::theme::{
    self, FAINT, FOAM, GOLD, GREEN, IRIS, LOGO, MUTED, ON_ACCENT, ORANGE, PINK, SURFACE,
    SURFACE_HIGH, TEXT, bold, danger_color, danger_word, fg, filled, gradient, gradient_text,
    meter, mix, pill, progress_bar, pulse, spinner,
};

const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 24;
const WIDE: u16 = 110;
/// Frames a newly shown list takes to fade in.
const FADE_FRAMES: u64 = 6;

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

fn evidence_color(evidence: Evidence) -> Color {
    match evidence {
        Evidence::Measured => GREEN,
        Evidence::Documented => FOAM,
        Evidence::Community => GOLD,
        Evidence::Experimental => PINK,
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
        Ok(view) if view.not_in_effect.is_some() => "not in effect".to_string(),
        Ok(view) if view.restart_pending => format!("{}, restart", state_text(&view.state)),
        Ok(view) => state_text(&view.state).to_string(),
        Err(_) => "n/a".to_string(),
    }
}

/// A state's symbol and colour, for the list.
fn state_look(label: &str) -> (&'static str, Color) {
    match label {
        l if l.starts_with("applied") => ("●", GREEN),
        l if l.starts_with("already") => ("●", FOAM),
        l if l.starts_with("not applied") => ("○", MUTED),
        l if l.starts_with("not in effect") => ("▲", GOLD),
        l if l.starts_with("partial") => ("►", GOLD),
        l if l.starts_with("changed") => ("▲", ORANGE),
        l if l.starts_with("interrupted") => ("▲", PINK),
        l if l.starts_with("unknown") => ("?", GOLD),
        l if l.starts_with("needs restart") => ("►", ORANGE),
        _ => ("·", FAINT),
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
    let mut lines = vec![
        format!("Danger: {}", danger_word(tweak.danger)),
        format!("Evidence: {}", evidence_long(tweak.evidence)),
        format!("When: {}", effect_text(tweak.effect)),
    ];
    match status {
        Some(Ok(view)) => {
            if let Some(note) = &view.not_in_effect {
                lines.push("State: not in effect".to_string());
                lines.push(format!("  {note}"));
            } else {
                lines.push(format!("State: {}", state_text(&view.state)));
            }
            if let Some(note) = &view.live_note {
                lines.push(format!("  {note}"));
            }
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
    lines.push("Undo:".to_string());
    lines.push(tweak.undo_note.clone());
    lines.push(String::new());
    lines.push("Anti-cheat:".to_string());
    lines.push("Untested unless listed here. Nothing here is promised safe.".to_string());
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

fn heading(text: &str, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled("▌ ", fg(color)),
        Span::styled(text.to_string(), fg(color).add_modifier(Modifier::BOLD)),
    ])
}

/// Styles one line of report or detail text. Text stays exactly as written, only colour is added.
pub(crate) fn styled_line(line: &str) -> Line<'static> {
    let owned = line.to_string();
    if let Some(word) = owned.strip_prefix("Danger: ") {
        let level = (0..5).find(|d| danger_word(*d) == word).unwrap_or(0);
        let color = danger_color(level);
        let mut spans = vec![Span::styled("Danger    ", fg(MUTED))];
        spans.extend(meter(usize::from(level) + 1, 5, color));
        spans.push(Span::raw(" "));
        spans.push(pill(word, color, ON_ACCENT));
        // Kept for search and for screen readers reading the plain text.
        return Line::from(spans);
    }
    if let Some(rest) = owned.strip_prefix("Evidence: ") {
        let (count, color) = match rest.split(':').next().unwrap_or("") {
            "Measured" => (4, GREEN),
            "Documented" => (3, FOAM),
            "Community-reported" => (2, GOLD),
            _ => (1, PINK),
        };
        let mut spans = vec![Span::styled("Evidence  ", fg(MUTED))];
        spans.extend(meter(count, 4, color));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(rest.to_string(), fg(color)));
        return Line::from(spans);
    }
    if let Some(rest) = owned.strip_prefix("When: ") {
        return Line::from(vec![
            Span::styled("When      ", fg(MUTED)),
            Span::styled(rest.to_string(), fg(TEXT)),
        ]);
    }
    if let Some(rest) = owned.strip_prefix("State: ") {
        let (symbol, color) = state_look(rest);
        return Line::from(vec![
            Span::styled("State     ", fg(MUTED)),
            Span::styled(
                format!("{symbol} {rest}"),
                fg(color).add_modifier(Modifier::BOLD),
            ),
        ]);
    }
    match owned.as_str() {
        "Risks:" => return heading("Risks", ORANGE),
        "Undo:" => return heading("Undo", GREEN),
        "Anti-cheat:" => return heading("Anti-cheat", GOLD),
        "Sources:" => return heading("Sources", FOAM),
        _ => {}
    }
    for (tag, color) in [
        ("[ OK ]", GREEN),
        ("[LOOK]", GOLD),
        ("[INFO]", FOAM),
        ("[????]", MUTED),
    ] {
        if let Some(rest) = owned.strip_prefix(tag) {
            let word = tag.trim_matches(|c| c == '[' || c == ']').trim();
            let (title, detail) = rest.split_once(": ").unwrap_or((rest, ""));
            return Line::from(vec![
                pill(&format!("{word:^4}"), color, ON_ACCENT),
                Span::styled(title.to_string(), fg(TEXT).add_modifier(Modifier::BOLD)),
                Span::styled(
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(": {detail}")
                    },
                    fg(MUTED),
                ),
            ]);
        }
    }
    let trimmed = owned.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        let indent = &owned[..owned.len() - trimmed.len()];
        return Line::from(vec![
            Span::raw(indent.to_string()),
            Span::styled("● ", fg(IRIS)),
            Span::styled(rest.to_string(), fg(TEXT)),
        ]);
    }
    if trimmed.starts_with("https://") {
        return Line::styled(owned, fg(FOAM));
    }
    for (needle, color) in [
        ("IMPROVED", GREEN),
        ("WORSE", PINK),
        ("no detectable effect", MUTED),
        ("not enough runs", MUTED),
    ] {
        if let Some(at) = owned.find(needle) {
            let (before, after) = owned.split_at(at);
            return Line::from(vec![
                Span::styled(before.to_string(), fg(TEXT)),
                Span::styled(after.to_string(), fg(color).add_modifier(Modifier::BOLD)),
            ]);
        }
    }
    Line::styled(owned, fg(TEXT))
}

fn panel(title: &str, focused: bool, frame: u64) -> Block<'static> {
    let border = if focused {
        fg(mix(IRIS, PINK, pulse(frame) * 0.45)).add_modifier(Modifier::BOLD)
    } else {
        fg(SURFACE_HIGH)
    };
    let title_span = if focused {
        Span::styled(
            format!(" {title} "),
            filled(IRIS, ON_ACCENT).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(format!(" {title} "), fg(MUTED))
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(border_type())
        .border_style(border)
        .title(title_span)
}

/// A panel whose border colour carries meaning, such as a danger level.
fn accent_panel(title: &str, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(border_type())
        .border_style(fg(color).add_modifier(Modifier::BOLD))
        .title(Span::styled(
            format!(" {title} "),
            filled(color, ON_ACCENT).add_modifier(Modifier::BOLD),
        ))
}

/// Pads to a width, so a row's background reaches its edge. Text that doesn't fit is cut short.
fn pad(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        let kept: String = text.chars().take(width.saturating_sub(1)).collect();
        format!("{kept}…")
    } else {
        format!("{text}{}", " ".repeat(width - count))
    }
}

/// Rounded corners aren't in every console font, so they're only used where the font is known.
fn border_type() -> BorderType {
    if theme::fancy_glyphs() {
        BorderType::Rounded
    } else {
        BorderType::Plain
    }
}

/// How a button looks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Primary,
    Plain,
    Danger,
}

/// Lays buttons out left to right, recording where each one landed.
struct ButtonRow {
    spans: Vec<Span<'static>>,
    x: u16,
    y: u16,
    limit: u16,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
}

impl ButtonRow {
    fn new(area: Rect, hover: Option<Hit>) -> Self {
        Self {
            spans: Vec::new(),
            x: area.x,
            y: area.y,
            limit: area.x + area.width,
            hits: Vec::new(),
            hover,
        }
    }

    fn fits(&self, width: u16) -> bool {
        self.x + width <= self.limit
    }

    fn add(&mut self, label: &str, button: Button, kind: Kind) {
        self.add_focused(label, button, kind, false);
    }

    fn add_focused(&mut self, label: &str, button: Button, kind: Kind, focused: bool) {
        let text = format!(" {label} ");
        let width = text.chars().count() as u16;
        if !self.fits(width) {
            return;
        }
        let hit = Hit::Button(button);
        let hovered = self.hover.as_ref() == Some(&hit);
        let (accent, rest) = match kind {
            Kind::Primary => (GREEN, SURFACE_HIGH),
            Kind::Danger => (PINK, SURFACE_HIGH),
            Kind::Plain => (IRIS, SURFACE),
        };
        let style = if hovered || focused {
            filled(accent, ON_ACCENT).add_modifier(Modifier::BOLD)
        } else if kind == Kind::Plain {
            filled(rest, TEXT)
        } else {
            filled(rest, accent)
        };
        self.hits.push((Rect::new(self.x, self.y, width, 1), hit));
        self.spans.push(Span::styled(text, style));
        self.spans.push(Span::raw(" "));
        self.x += width + 1;
    }

    /// Shown but not clickable, until a requirement is met.
    fn add_disabled(&mut self, label: &str) {
        let text = format!(" {label} ");
        let width = text.chars().count() as u16;
        if self.fits(width) {
            self.spans.push(Span::styled(text, filled(SURFACE, FAINT)));
            self.x += width + 1;
        }
    }

    fn finish(self, frame: &mut Frame<'_>, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
        frame.render_widget(Paragraph::new(Line::from(self.spans)), area);
        hits.extend(self.hits);
    }
}

fn scrollbar(frame: &mut Frame<'_>, area: Rect, content: usize, viewport: usize, position: usize) {
    if content <= viewport {
        return;
    }
    let mut state = ScrollbarState::new(content.saturating_sub(viewport)).position(position);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_symbol("█")
            .track_symbol(Some("│"))
            .thumb_style(fg(IRIS))
            .track_style(fg(SURFACE_HIGH)),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

/// How much wrapped text there is compared with its box, so scrolling stops at the last line.
struct ScrollView {
    total: usize,
    viewport: usize,
}

impl ScrollView {
    /// `side_cells` is how much of the box's width isn't text, such as borders and padding.
    fn new(text: &Paragraph<'_>, area: Rect, side_cells: u16) -> Self {
        Self {
            total: text.line_count(area.width.saturating_sub(side_cells)),
            viewport: area.height.saturating_sub(2) as usize,
        }
    }

    fn clamp(&self, scroll: u16) -> u16 {
        let limit = self.total.saturating_sub(self.viewport);
        scroll.min(u16::try_from(limit).unwrap_or(u16::MAX))
    }

    fn scrollbar(&self, frame: &mut Frame<'_>, area: Rect, scroll: u16) {
        scrollbar(frame, area, self.total, self.viewport, scroll as usize);
    }
}

impl<S: System, T: Store, C: Clock, H: Host> App<S, T, C, H> {
    /// Draws the current screen, and records every clickable and scrollable region.
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        self.hits.clear();
        self.scrolls.clear();
        let area = frame.area();
        frame.render_widget(Clear, area);
        frame.render_widget(Block::default().style(theme::screen()), area);
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            frame.render_widget(
                Paragraph::new(format!(
                    "Please make this window at least {MIN_WIDTH}x{MIN_HEIGHT}. It is {}x{}.",
                    area.width, area.height
                ))
                .wrap(Wrap { trim: true })
                .style(fg(GOLD)),
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
            } => self.draw_text(frame, area, &title, &lines, scroll),
            Screen::RestartPrompt => self.draw_restart(frame, area),
            Screen::KeepDisplay { deadline, .. } => self.draw_keep_display(frame, area, deadline),
            Screen::Bench(session) => self.draw_bench(frame, area, &session),
            Screen::Timer { holding, lines } => self.draw_timer(frame, area, holding, &lines),
            Screen::PollingMeter { since } => self.draw_polling(frame, area, since),
        }
    }

    fn buttons(&self, area: Rect) -> ButtonRow {
        ButtonRow::new(area, self.hover.clone())
    }

    /// Splits a screen into a body and a one-line row of buttons.
    fn body_and_buttons(area: Rect) -> [Rect; 2] {
        Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(area)
    }

    fn draw_logo(&self, frame: &mut Frame<'_>, area: Rect) {
        let width = LOGO[0].chars().count();
        let sweep = self.frame as f32 / 40.0;
        let lines: Vec<Line> = LOGO
            .iter()
            .enumerate()
            .map(|(row, text)| {
                let spans: Vec<Span> = text
                    .chars()
                    .enumerate()
                    .map(|(column, c)| {
                        let t = (column + row) as f32 / width as f32;
                        let wave = ((t * 3.0 - sweep).sin() + 1.0) / 2.0;
                        Span::styled(c.to_string(), fg(mix(gradient(t), TEXT, wave * 0.2)))
                    })
                    .collect();
                Line::from(spans)
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
    }

    fn draw_disclaimer(&mut self, frame: &mut Frame<'_>, area: Rect, typing: &str) {
        let [logo, tagline, body, input, buttons] = Layout::vertical([
            Constraint::Length(5),
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(area);
        self.draw_logo(frame, logo);
        frame.render_widget(
            Paragraph::new("Real, measurable gaming performance. Every change can be undone.")
                .alignment(Alignment::Center)
                .style(fg(MUTED)),
            tagline,
        );
        let text = vec![
            Line::styled("Please read this before using augeo.", bold()),
            Line::from(""),
            Line::from(disclaimer::TEXT),
        ];
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: true })
                .block(accent_panel("Disclaimer", GOLD)),
            body,
        );
        let cursor = if self.cursor_on() { "█" } else { " " };
        frame.render_widget(
            Paragraph::new(format!("{typing}{cursor}")).block(panel(
                "Type accept and press Enter",
                true,
                self.frame,
            )),
            input,
        );
        let mut row = self.buttons(buttons);
        row.add("I accept", Button::Accept, Kind::Primary);
        row.add("Decline and quit", Button::Decline, Kind::Plain);
        row.finish(frame, buttons, &mut self.hits);
    }

    fn cursor_on(&self) -> bool {
        (self.frame / 12).is_multiple_of(2)
    }

    fn draw_recovery(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        items: &[InDoubt],
        message: Option<String>,
    ) {
        let [body, buttons] = Self::body_and_buttons(area);
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
            lines.push(Line::styled(format!("{verb}: {}", item.title), bold()));
            for r in &item.resources {
                let live = r
                    .live
                    .as_ref()
                    .map_or("couldn't read".to_string(), ToString::to_string);
                lines.push(Line::styled(format!("  {}", r.address), fg(FOAM)));
                lines.push(Line::from(format!(
                    "    before: {}   augeo's: {}   now: {live}",
                    r.baseline, r.desired
                )));
            }
        }
        if let Some(message) = message {
            lines.push(Line::from(""));
            lines.extend(
                message
                    .lines()
                    .map(|l| Line::styled(l.to_string(), fg(GOLD))),
            );
        }
        if !self.engine.is_writer() {
            lines.push(Line::from(""));
            lines.push(Line::styled("Resolving this needs elevation.", fg(GOLD)));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(accent_panel("Interrupted change", PINK)),
            body,
        );
        let mut row = self.buttons(buttons);
        if op == Op::Apply {
            row.add("Roll back (Enter)", Button::RollBack, Kind::Primary);
            row.add("Keep what was done (k)", Button::Keep, Kind::Plain);
        } else {
            row.add(
                "Finish the undo (Enter)",
                Button::FinishRevert,
                Kind::Primary,
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
                Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .areas(body);
            (l, Some(d))
        } else {
            (body, None)
        };
        let [components, tweaks] =
            Layout::horizontal([Constraint::Length(20), Constraint::Min(30)]).areas(lists);
        self.draw_components(frame, components);
        self.draw_tweaks(frame, tweaks);
        if let Some(detail) = detail {
            self.draw_detail(frame, detail);
        }

        let mut row = self.buttons(buttons);
        row.add("Apply a", Button::Apply, Kind::Primary);
        row.add("Dry d", Button::Dry, Kind::Plain);
        row.add("Undo u", Button::Undo, Kind::Plain);
        row.add("Status", Button::Status, Kind::Plain);
        row.add("Check", Button::Check, Kind::Plain);
        row.add("Bench", Button::Bench, Kind::Plain);
        row.add("Myths", Button::Myths, Kind::Plain);
        row.add("Help ?", Button::Help, Kind::Plain);
        row.add("Quit q", Button::Quit, Kind::Plain);
        row.finish(frame, buttons, &mut self.hits);

        if let Some(text) = &self.message {
            frame.render_widget(Paragraph::new(text.clone()).style(fg(GOLD)), message);
        }
        self.draw_command_bar(frame, bar);
        self.draw_status_line(frame, status);
    }

    fn draw_status_line(&self, frame: &mut Frame<'_>, area: Rect) {
        let hints: &[(&str, &str)] = if self.pane == Pane::Bar {
            &[("Enter", "run"), ("Tab", "complete"), ("Esc", "cancel")]
        } else {
            &[
                ("↑↓", "move"),
                ("Tab", "pane"),
                ("Space", "tick"),
                ("Enter", "open"),
                ("/", "command"),
            ]
        };
        let mut spans = Vec::new();
        for (key, what) in hints {
            spans.push(Span::styled(format!(" {key} "), filled(SURFACE_HIGH, TEXT)));
            spans.push(Span::styled(format!(" {what}  "), fg(MUTED)));
        }
        let used: usize = spans.iter().map(Span::width).sum();
        let room = (area.width as usize).saturating_sub(used);
        let environment: String = self.environment_line.chars().take(room).collect();
        let gap = room.saturating_sub(environment.chars().count());
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(environment, fg(FAINT)));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_command_bar(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let focused = self.pane == Pane::Bar;
        let mut spans = Vec::new();
        if self.bar.is_empty() && !focused {
            spans.push(Span::styled(
                "Press / to type a command, like auge!cpu#1",
                fg(FAINT),
            ));
        } else {
            let valid = parse(&self.bar).is_ok();
            spans.push(Span::styled(
                self.bar.clone(),
                fg(if valid { GREEN } else { TEXT }).add_modifier(Modifier::BOLD),
            ));
            let rest = self.completion().unwrap_or_default();
            let mut rest = rest.chars();
            if focused {
                // The cursor sits where the next character will go, on top of any suggestion.
                let under = rest.next().unwrap_or(' ').to_string();
                let style = if self.cursor_on() {
                    filled(IRIS, ON_ACCENT)
                } else {
                    fg(FAINT)
                };
                spans.push(Span::styled(under, style));
            }
            spans.push(Span::styled(rest.collect::<String>(), fg(FAINT)));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).block(panel("Command", focused, self.frame)),
            area,
        );
        self.hits.push((area, Hit::Bar));
    }

    fn draw_header(&mut self, frame: &mut Frame<'_>, area: Rect) {
        // A soft highlight passes over the name now and then.
        let shine = self.frame % 240;
        let shift = (shine < 40).then_some(shine / 2);
        let mut spans = vec![Span::raw(" ")];
        spans.extend(gradient_text("augeo", shift));
        spans.push(Span::styled(
            format!(" {} ", env!("CARGO_PKG_VERSION")),
            fg(FAINT),
        ));
        spans.push(Span::raw(" "));
        let x = area.x + spans.iter().map(Span::width).sum::<usize>() as u16;

        if self.engine.is_writer() {
            spans.push(pill("ELEVATED", GOLD, ON_ACCENT));
            spans.push(Span::raw(" "));
        } else {
            let label = "READ-ONLY";
            spans.push(pill(label, SURFACE_HIGH, FOAM));
            let elevate = " Elevate e ";
            let width = (label.len() + 2) as u16;
            let hit = Hit::Button(Button::Elevate);
            let hovered = self.hover.as_ref() == Some(&hit);
            self.hits
                .push((Rect::new(x, area.y, width + elevate.len() as u16, 1), hit));
            let style = if hovered {
                filled(IRIS, ON_ACCENT)
            } else {
                filled(SURFACE, IRIS)
            };
            spans.push(Span::styled(elevate, style));
            spans.push(Span::raw(" "));
        }
        if self.engine.open_report().history_lost {
            spans.push(pill(
                "JOURNAL DAMAGED: some history was lost",
                PINK,
                ON_ACCENT,
            ));
            spans.push(Span::raw(" "));
        }
        let applied = self.engine.ledger().applied.len();
        if applied > 0 {
            spans.push(pill(&format!("{applied} applied"), SURFACE_HIGH, GREEN));
            spans.push(Span::raw(" "));
        }
        let pending = self.pending_restart_count();
        if pending > 0 {
            spans.push(pill(
                &format!("{pending} change(s) need a restart"),
                ORANGE,
                ON_ACCENT,
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_components(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let focused = self.pane == Pane::Components;
        let outer = panel("Components", focused, self.frame);
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        self.scrolls.push((area, ScrollArea::Components));
        let width = inner.width as usize;
        let mut lines = Vec::new();
        for (i, component) in Component::ALL.iter().enumerate() {
            let selected = i == self.component;
            let hit = Hit::Component(i);
            let hovered = self.hover.as_ref() == Some(&hit);
            let hue = gradient(i as f32 / (Component::ALL.len() - 1) as f32);
            let count = self.tweak_count(*component);
            let applied = self.applied_count(*component);
            let tally = if applied > 0 {
                format!("{applied}/{count}")
            } else {
                count.to_string()
            };
            let name_width = width.saturating_sub(3 + tally.chars().count());
            let row = format!("{}{tally}", pad(component.as_str(), name_width));
            let line = if selected && focused {
                Line::from(vec![
                    Span::styled(" ► ", filled(IRIS, ON_ACCENT)),
                    Span::styled(row, filled(IRIS, ON_ACCENT).add_modifier(Modifier::BOLD)),
                ])
            } else if selected {
                Line::from(vec![
                    Span::styled(" ► ", filled(SURFACE_HIGH, IRIS)),
                    Span::styled(row, filled(SURFACE_HIGH, TEXT).add_modifier(Modifier::BOLD)),
                ])
            } else if hovered {
                Line::from(vec![
                    Span::styled(" ● ", filled(SURFACE, hue)),
                    Span::styled(row, filled(SURFACE, TEXT)),
                ])
            } else {
                Line::from(vec![
                    Span::styled(" ● ", fg(hue)),
                    Span::styled(row, fg(if applied > 0 { TEXT } else { MUTED })),
                ])
            };
            lines.push(line);
            if (i as u16) < inner.height {
                self.hits
                    .push((Rect::new(inner.x, inner.y + i as u16, inner.width, 1), hit));
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
        let outer = panel(&title, focused, self.frame);
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        self.scrolls.push((area, ScrollArea::Tweaks));

        let tweaks: Vec<Tweak> = self.tweaks().into_iter().cloned().collect();
        if tweaks.is_empty() {
            frame.render_widget(
                Paragraph::new("No tweaks here yet. Dropped ones are on the Myths screen.")
                    .wrap(Wrap { trim: true })
                    .style(fg(MUTED)),
                inner,
            );
            return;
        }
        let list = inner;

        // Each tweak takes two lines: its name, then its badges. That leaves room for full names
        // even in a narrow pane. A blank line goes between them when they all fit that way.
        const INDENT: usize = 8;
        const DANGER: usize = 8;
        const EVIDENCE: usize = 4;
        let total = list.width as usize;
        let title_width = total.saturating_sub(INDENT);
        let height = list.height as usize;
        let step = if height + 1 >= tweaks.len() * 3 { 3 } else { 2 };
        let visible = ((height + step - 2) / step).max(1);
        if self.tweak < self.tweak_scroll {
            self.tweak_scroll = self.tweak;
        } else if self.tweak >= self.tweak_scroll + visible {
            self.tweak_scroll = self.tweak + 1 - visible;
        }
        // A list fades in briefly when it changes, which draws the eye to it.
        let age = self.frame.saturating_sub(self.list_changed_at);
        let fade = (age as f32 / FADE_FRAMES as f32).min(1.0);

        let mut lines = Vec::new();
        for (row, (index, tweak)) in tweaks
            .iter()
            .enumerate()
            .skip(self.tweak_scroll)
            .take(visible)
            .enumerate()
        {
            let selected = index == self.tweak;
            let y = list.y + (row * step) as u16;
            let row_hit = Hit::Tweak(index);
            let box_hit = Hit::TweakBox(index);
            let hovered =
                self.hover.as_ref() == Some(&row_hit) || self.hover.as_ref() == Some(&box_hit);
            let applicable = self.applicability(tweak);
            let label = match &applicable {
                Err(_) => "n/a".to_string(),
                Ok(()) => self
                    .status_of(tweak)
                    .map_or("pick a game".to_string(), |s| {
                        let full = state_label(s);
                        if full.ends_with(", restart") {
                            "needs restart".to_string()
                        } else {
                            full
                        }
                    }),
            };
            let (symbol, state_color) = state_look(&label);
            let enabled = applicable.is_ok() && !matches!(self.status_of(tweak), Some(Err(_)));

            // The row's background reaches both edges, so where you are is obvious at a glance.
            let (background, text) = match (selected, focused, hovered) {
                (true, true, _) => (Some(IRIS), ON_ACCENT),
                (true, false, _) => (Some(SURFACE_HIGH), TEXT),
                (false, _, true) => (Some(SURFACE), TEXT),
                _ => (
                    None,
                    if enabled {
                        mix(FAINT, TEXT, fade)
                    } else {
                        FAINT
                    },
                ),
            };
            let base = match background {
                Some(b) => filled(b, text),
                None => fg(text),
            };
            let accent_on = |color: Color| match background {
                Some(b) if selected && focused => filled(b, ON_ACCENT),
                Some(b) => filled(b, color),
                None => fg(color),
            };

            let marker = if selected { "▌" } else { " " };
            let marker_style = if selected && focused {
                filled(IRIS, mix(ON_ACCENT, PINK, pulse(self.frame)))
            } else if selected {
                accent_on(IRIS)
            } else {
                base
            };
            let tick = if self.is_ticked(tweak) { "[x]" } else { "[ ]" };
            let tick_style = if self.is_ticked(tweak) {
                accent_on(GREEN).add_modifier(Modifier::BOLD)
            } else {
                base
            };
            let word = danger_word(tweak.danger);
            let danger = danger_color(tweak.danger);
            let danger_style = if enabled {
                filled(danger, ON_ACCENT)
            } else {
                accent_on(FAINT)
            };
            let state_text = format!("{symbol} {label}");
            let repair = if Self::is_repair(tweak) {
                "  repair"
            } else {
                ""
            };
            let used = INDENT
                + DANGER
                + 2
                + EVIDENCE
                + 2
                + state_text.chars().count()
                + repair.chars().count();
            lines.push(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(format!("{tick} "), tick_style),
                Span::styled(format!("{:>2} ", tweak.number), accent_on(MUTED)),
                Span::styled(
                    pad(&tweak.title, title_width),
                    if selected {
                        base.add_modifier(Modifier::BOLD)
                    } else {
                        base
                    },
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(" ".repeat(INDENT - 1), base),
                Span::styled(format!("{word:^8}"), danger_style),
                Span::styled("  ", base),
                Span::styled(
                    pad(evidence_short(tweak.evidence), EVIDENCE),
                    accent_on(if enabled {
                        evidence_color(tweak.evidence)
                    } else {
                        FAINT
                    }),
                ),
                Span::styled("  ", base),
                Span::styled(
                    state_text,
                    accent_on(if enabled { state_color } else { FAINT }),
                ),
                Span::styled(repair, accent_on(FOAM)),
                Span::styled(" ".repeat(total.saturating_sub(used)), base),
            ]));
            if step == 3 {
                lines.push(Line::from(""));
            }
            self.hits
                .push((Rect::new(list.x, y, list.width, 2), row_hit));
            self.hits.push((Rect::new(list.x, y, 4, 1), box_hit));
        }
        frame.render_widget(Paragraph::new(lines), list);
        scrollbar(frame, area, tweaks.len(), visible, self.tweak_scroll);
    }

    fn draw_detail(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let focused = self.pane == Pane::Detail;
        self.scrolls.push((area, ScrollArea::Detail));
        let Some(tweak) = self.highlighted().cloned() else {
            frame.render_widget(panel("Details", focused, self.frame), area);
            return;
        };
        let mut lines: Vec<String> = Vec::new();
        if let Err(reason) = self.applicability(&tweak) {
            lines.push(format!("[LOOK] Doesn't apply here: {reason}"));
            lines.push(String::new());
        }
        lines.extend(detail_lines(
            &tweak,
            self.status_of(&tweak),
            self.options.get(&tweak.id),
        ));
        let text: Vec<Line> = lines.iter().map(|l| styled_line(l)).collect();
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        let view = ScrollView::new(&paragraph, area, 2);
        self.detail_scroll = view.clamp(self.detail_scroll);
        frame.render_widget(
            paragraph.scroll((self.detail_scroll, 0)).block(panel(
                &tweak.title,
                focused,
                self.frame,
            )),
            area,
        );
        view.scrollbar(frame, area, self.detail_scroll);
    }

    fn plan_lines(&self, state: &ConfirmState) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for planned in &state.plan.tweaks {
            let color = danger_color(planned.danger);
            lines.push(Line::from(vec![
                Span::styled(planned.title.clone(), fg(TEXT).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                pill(danger_word(planned.danger), color, ON_ACCENT),
                Span::styled(format!("  {}", effect_text(planned.effect)), fg(MUTED)),
            ]));
            for step in &planned.steps {
                lines.push(Line::styled(format!("  {}", step.address), fg(FOAM)));
                let note = if step.baseline_lost {
                    Line::styled(
                        "    The original value was lost, so this stays as it is.",
                        fg(GOLD),
                    )
                } else if step.drift && !step.write {
                    Line::styled(
                        format!(
                            "    Changed by something else to {}. Kept as it is.",
                            step.current
                        ),
                        fg(GOLD),
                    )
                } else if step.write {
                    Line::from(vec![
                        Span::styled(format!("    {}", step.current), fg(MUTED)),
                        Span::styled("  ->  ", fg(IRIS)),
                        Span::styled(
                            step.target.to_string(),
                            fg(GREEN).add_modifier(Modifier::BOLD),
                        ),
                    ])
                } else {
                    Line::styled(
                        format!("    {}  (no change needed)", step.current),
                        fg(FAINT),
                    )
                };
                lines.push(note);
            }
            if state.plan.op == Op::Apply {
                let tweak = self
                    .engine
                    .catalog()
                    .tweaks
                    .iter()
                    .find(|t| t.id == planned.tweak_id);
                if let Some(tweak) = tweak {
                    lines.push(Line::from(vec![
                        Span::styled("  Evidence: ", fg(MUTED)),
                        Span::styled(
                            evidence_long(tweak.evidence),
                            fg(evidence_color(tweak.evidence)),
                        ),
                    ]));
                    lines.push(Line::styled("  Risks:", fg(ORANGE)));
                    for risk in &tweak.risks {
                        lines.push(styled_line(&format!("    - {risk}")));
                    }
                    for note in &tweak.anticheat {
                        lines.push(Line::styled(
                            format!("  {}: {}", system_name(note.system), note.note),
                            fg(GOLD),
                        ));
                    }
                    lines.push(Line::from(vec![
                        Span::styled("  Undo: ", fg(GREEN)),
                        Span::styled(tweak.undo_note.clone(), fg(TEXT)),
                    ]));
                }
            }
            if !planned.writes() {
                lines.push(Line::styled("  Nothing to change.", fg(FAINT)));
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
        // The frame takes the colour of the riskiest change in it.
        let top = state
            .plan
            .tweaks
            .iter()
            .map(|t| t.danger)
            .max()
            .unwrap_or(0);
        let color = if state.dry { FOAM } else { danger_color(top) };
        let paragraph = Paragraph::new(self.plan_lines(state)).wrap(Wrap { trim: false });
        let view = ScrollView::new(&paragraph, body, 2);
        let scroll = view.clamp(state.scroll);
        if let Screen::Confirm(live) = &mut self.screen {
            live.scroll = scroll;
        }
        frame.render_widget(
            paragraph
                .scroll((scroll, 0))
                .block(accent_panel(title, color)),
            body,
        );
        view.scrollbar(frame, body, scroll);
        self.scrolls.push((body, ScrollArea::Body));

        if let Some(text) = &self.message {
            frame.render_widget(Paragraph::new(text.clone()).style(fg(GOLD)), message);
        }
        if let Some(word) = state.current_word() {
            let instance = state.pending.first().map_or("", |(i, _)| i.as_str());
            let label = format!("{instance}: type \"{word}\" and press Enter");
            let matches = word.starts_with(state.typing.as_str());
            let cursor = if self.cursor_on() { "█" } else { " " };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(state.typing.clone(), fg(if matches { GREEN } else { PINK })),
                    Span::styled(cursor, fg(IRIS)),
                ]))
                .block(accent_panel(&label, color)),
                input,
            );
        }
        if needs_tick {
            let mark = if state.ticked { "[x]" } else { "[ ]" };
            let style = if state.ticked { fg(GREEN) } else { fg(GOLD) };
            frame.render_widget(
                Paragraph::new(format!("{mark} I read the risks (Space)")).style(style),
                tick,
            );
            self.hits.push((tick, Hit::Tick));
        }
        if has_drift {
            let mark = if state.drift_restore { "[x]" } else { "[ ]" };
            let mut row = self.buttons(drift);
            row.add(
                &format!("{mark} Overwrite values changed by something else (x)"),
                Button::DriftToggle,
                Kind::Plain,
            );
            row.finish(frame, drift, &mut self.hits);
        }

        let mut row = self.buttons(buttons);
        if state.dry {
            row.add_focused("Close (Enter)", Button::Cancel, Kind::Plain, true);
        } else {
            row.add_focused(
                "Cancel (Esc)",
                Button::Cancel,
                Kind::Plain,
                !state.focus_confirm,
            );
            let label = if state.plan.op == Op::Apply {
                "Apply"
            } else {
                "Undo"
            };
            if state.ready() {
                row.add_focused(
                    label,
                    Button::Confirm,
                    if top >= 3 {
                        Kind::Danger
                    } else {
                        Kind::Primary
                    },
                    state.focus_confirm,
                );
            } else {
                row.add_disabled(&format!("{label}: not yet"));
            }
        }
        row.finish(frame, buttons, &mut self.hits);
    }

    /// A short message in a card, sized to its content and centred on screen.
    fn card(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        title: &str,
        color: Color,
        lines: Vec<Line<'static>>,
    ) -> Rect {
        let width = area.width.min(80);
        // Wrapped text takes more rows than it has lines, so this leaves room generously.
        let height = (lines.len() as u16 * 2 + 4).min(area.height);
        let [_, middle, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .areas(area);
        let [_, card, _] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .areas(middle);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }).block(
                accent_panel(title, color).padding(ratatui::widgets::Padding::horizontal(1)),
            ),
            card,
        );
        card
    }

    fn draw_restore_offer(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let [body, buttons] = Self::body_and_buttons(area);
        self.card(
            frame,
            body,
            "Create a restore point first?",
            FOAM,
            vec![
                Line::from(
                    "This is the first change augeo will make here. A System Restore point is an \
                     extra safety net. augeo's own undo doesn't depend on it.",
                ),
                Line::from(""),
                Line::styled(
                    "Windows may silently skip creating one if another was made in the last 24 \
                     hours, and System Protection is off by default. augeo will tell you what \
                     really happened.",
                    fg(MUTED),
                ),
            ],
        );
        let mut row = self.buttons(buttons);
        row.add(
            "Create one (Enter)",
            Button::CreateRestorePoint,
            Kind::Primary,
        );
        row.add("Skip (Esc)", Button::SkipRestorePoint, Kind::Plain);
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_elevate(&mut self, frame: &mut Frame<'_>, area: Rect, command: &str) {
        let [body, buttons] = Self::body_and_buttons(area);
        let target = if command.is_empty() {
            "It will open in a new window.".to_string()
        } else {
            format!("It will open in a new window at: {command}")
        };
        self.card(
            frame,
            body,
            "Elevation needed",
            GOLD,
            vec![
                Line::from(format!(
                    "Changing system settings needs administrator rights. Windows will ask for \
                     permission. {target}"
                )),
                Line::from(""),
                Line::styled(
                    "You'll still see the exact changes and confirm them there. Nothing changes \
                     until you do.",
                    fg(MUTED),
                ),
            ],
        );
        // Only Cancel is clickable, so a stray click can't bring up the UAC prompt.
        let mut row = self.buttons(buttons);
        row.add_disabled("Press Enter to continue");
        row.add("Cancel (Esc)", Button::Cancel, Kind::Plain);
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_text(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        title: &str,
        lines: &[String],
        scroll: u16,
    ) {
        let [body, buttons] = Self::body_and_buttons(area);
        let text: Vec<Line> = lines.iter().map(|l| styled_line(l)).collect();
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        // Borders and a column of padding on each side.
        let view = ScrollView::new(&paragraph, body, 4);
        let scroll = view.clamp(scroll);
        if let Screen::Text { scroll: live, .. } = &mut self.screen {
            *live = scroll;
        }
        frame.render_widget(
            paragraph
                .scroll((scroll, 0))
                .block(accent_panel(title, IRIS).padding(ratatui::widgets::Padding::horizontal(1))),
            body,
        );
        view.scrollbar(frame, body, scroll);
        self.scrolls.push((body, ScrollArea::Body));
        let mut row = self.buttons(buttons);
        row.add_focused("Close (Esc)", Button::Close, Kind::Plain, true);
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_restart(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let [body, buttons] = Self::body_and_buttons(area);
        let pending = self.pending_restart_count();
        self.card(
            frame,
            body,
            "Restart now?",
            ORANGE,
            vec![Line::from(format!(
                "{pending} applied change(s) are waiting for a restart. Save your work first. \
                 Windows will restart right away."
            ))],
        );
        let mut row = self.buttons(buttons);
        row.add_focused("Not now (Esc)", Button::Close, Kind::Plain, true);
        row.add("Restart now (y)", Button::Restart, Kind::Danger);
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_keep_display(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        deadline: std::time::Instant,
    ) {
        let [body, buttons] = Self::body_and_buttons(area);
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let fraction = left.as_secs_f32() / crate::app::DISPLAY_REVERT_AFTER.as_secs_f32();
        self.card(
            frame,
            body,
            "Keep this display mode?",
            GOLD,
            vec![
                Line::from(format!(
                    "Can you see this clearly? The display mode goes back by itself in {} seconds \
                     unless you keep it.",
                    left.as_secs() + 1
                )),
                Line::from(""),
                progress_bar(fraction, 60, self.frame),
            ],
        );
        let mut row = self.buttons(buttons);
        row.add("Keep it (Enter)", Button::KeepDisplay, Kind::Primary);
        row.add("Put it back (Esc)", Button::RevertDisplay, Kind::Plain);
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_bench(&mut self, frame: &mut Frame<'_>, area: Rect, session: &BenchSession) {
        let [body, buttons] = Self::body_and_buttons(area);
        let now = std::time::Instant::now();
        let done = session.next as f32 / session.queue.len().max(1) as f32;
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Game  ", fg(MUTED)),
                Span::styled(session.exe.clone(), fg(TEXT).add_modifier(Modifier::BOLD)),
            ]),
            Line::styled(session.run_line(), fg(IRIS).add_modifier(Modifier::BOLD)),
            progress_bar(done, 60, 0),
            Line::from(""),
        ];
        match &session.phase {
            BenchPhase::Prepare => {
                if session.needs_game_restart() {
                    lines.push(Line::styled(
                        "This tweak takes effect when the game starts, so restart the game now.",
                        fg(GOLD),
                    ));
                }
                lines.push(Line::from(
                    "Get the game to your repeatable scene, then come back and press Enter. You \
                     then have 10 seconds to switch to the game. A beep marks the start, and \
                     another the end.",
                ));
            }
            BenchPhase::Countdown { until } => {
                let left = until.saturating_duration_since(now);
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", spinner(self.frame)), fg(GOLD)),
                    Span::styled(
                        format!(
                            "Switch to the game now. Starting in {} seconds.",
                            left.as_secs() + 1
                        ),
                        fg(GOLD).add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.push(progress_bar(
                    1.0 - left.as_secs_f32() / crate::bench::COUNTDOWN.as_secs_f32(),
                    60,
                    self.frame,
                ));
            }
            BenchPhase::Capturing { since } => {
                let elapsed = now.saturating_duration_since(*since).as_secs();
                let total = u64::from(RUN_SECONDS);
                lines.push(Line::from(vec![
                    Span::styled("● ", fg(PINK)),
                    Span::styled(
                        format!(
                            "Capturing: {}s of {total}s. Play the same scene until the second beep.",
                            elapsed.min(total)
                        ),
                        fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.push(progress_bar(elapsed as f32 / total as f32, 60, 0));
                lines.push(Line::styled(
                    "This screen updates once a second during capture, to stay out of the way.",
                    fg(FAINT),
                ));
            }
        }
        if let Some(note) = &session.note {
            lines.push(Line::from(""));
            lines.push(Line::styled(note.clone(), fg(GOLD)));
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                accent_panel("Benchmark", IRIS).padding(ratatui::widgets::Padding::horizontal(1)),
            ),
            body,
        );
        let mut row = self.buttons(buttons);
        if session.phase == BenchPhase::Prepare {
            row.add("Start (Enter)", Button::BenchGo, Kind::Primary);
        }
        row.add("Stop (Esc)", Button::BenchStop, Kind::Plain);
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_timer(&mut self, frame: &mut Frame<'_>, area: Rect, holding: bool, lines: &[String]) {
        let [body, buttons] = Self::body_and_buttons(area);
        let text: Vec<Line> = lines.iter().map(|l| styled_line(l)).collect();
        let color = if holding { GREEN } else { IRIS };
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(
                accent_panel("Timer tool", color).padding(ratatui::widgets::Padding::horizontal(1)),
            ),
            body,
        );
        let mut row = self.buttons(buttons);
        let label = if holding {
            "Stop holding (h)"
        } else {
            "Hold high resolution while open (h)"
        };
        row.add(label, Button::TimerHold, Kind::Primary);
        row.add("Close (Esc)", Button::TimerClose, Kind::Plain);
        row.finish(frame, buttons, &mut self.hits);
    }

    fn draw_polling(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        since: Option<std::time::Instant>,
    ) {
        let [body, buttons] = Self::body_and_buttons(area);
        let lines = match since {
            None => vec![
                Line::from(vec![
                    Span::styled(format!("{} ", spinner(self.frame)), fg(IRIS)),
                    Span::styled(
                        "Waiting for you. Click inside the test window that just opened.",
                        fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::styled(
                    "Windows limits background programs to about 125 mouse reports a second, so \
                     an accurate reading needs a window that has your focus.",
                    fg(MUTED),
                ),
            ],
            Some(started) => {
                let elapsed = std::time::Instant::now().saturating_duration_since(started);
                let total = crate::app::POLLING_SECONDS as f32;
                vec![
                    Line::from(vec![
                        Span::styled("● ", fg(PINK)),
                        Span::styled(
                            format!(
                                "Testing: {:.0}s left. Keep moving the mouse quickly in circles.",
                                (total - elapsed.as_secs_f32()).max(0.0).ceil()
                            ),
                            fg(TEXT).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(""),
                    progress_bar(elapsed.as_secs_f32() / total, 60, self.frame),
                    Line::from(""),
                    Line::styled(
                        "Slow movement sends fewer reports, so fast movement gives the true rate.",
                        fg(MUTED),
                    ),
                ]
            }
        };
        self.card(frame, body, "Mouse polling rate", IRIS, lines);
        let mut row = self.buttons(buttons);
        row.add("Cancel (Esc)", Button::PollingCancel, Kind::Plain);
        row.finish(frame, buttons, &mut self.hits);
    }
}
