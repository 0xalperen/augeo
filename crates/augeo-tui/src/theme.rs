//! The design system. Every colour on screen comes from here, so the look stays consistent.
//!
//! Every cell gets an explicit foreground and background rather than relying on the terminal's own
//! colours or on reverse video, so a highlight looks the same in every terminal and colour scheme.
//! Colour is never the only signal: states and levels always have words too.

use std::sync::OnceLock;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

pub(crate) const BASE: Color = rgb(25, 23, 36);
pub(crate) const SURFACE: Color = rgb(38, 35, 58);
pub(crate) const SURFACE_HIGH: Color = rgb(57, 53, 82);
pub(crate) const FAINT: Color = rgb(110, 106, 134);
pub(crate) const MUTED: Color = rgb(144, 140, 170);
pub(crate) const TEXT: Color = rgb(224, 222, 244);
pub(crate) const ON_ACCENT: Color = rgb(25, 23, 36);
pub(crate) const IRIS: Color = rgb(196, 167, 231);
pub(crate) const PINK: Color = rgb(235, 111, 146);
pub(crate) const FOAM: Color = rgb(156, 207, 216);
pub(crate) const GREEN: Color = rgb(158, 214, 137);
pub(crate) const GOLD: Color = rgb(246, 193, 119);
pub(crate) const ORANGE: Color = rgb(240, 138, 93);

/// By the `NO_COLOR` convention, any non-empty value asks for no colour.
fn asks_for_no_colour(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

/// `NO_COLOR` is honoured. Words and layout carry the meaning, so nothing is lost. Tests always see
/// colour, so one set in the developer's shell can't quietly change what they check.
pub(crate) fn plain() -> bool {
    static PLAIN: OnceLock<bool> = OnceLock::new();
    !cfg!(test)
        && *PLAIN.get_or_init(|| asks_for_no_colour(std::env::var_os("NO_COLOR").as_deref()))
}

/// Richer glyphs are only safe where the font is known to have them. Classic conhost often uses
/// fonts without braille, so it gets a spinner made of characters every console font has.
pub(crate) fn fancy_glyphs() -> bool {
    static FANCY: OnceLock<bool> = OnceLock::new();
    *FANCY.get_or_init(|| std::env::var_os("WT_SESSION").is_some())
}

pub(crate) fn fg(color: Color) -> Style {
    if plain() {
        Style::default()
    } else {
        Style::default().fg(color)
    }
}

/// Text on a solid colour, for pills and selection bars.
pub(crate) fn filled(background: Color, foreground: Color) -> Style {
    if plain() {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().bg(background).fg(foreground)
    }
}

pub(crate) fn screen() -> Style {
    if plain() {
        Style::default()
    } else {
        Style::default().bg(BASE).fg(TEXT)
    }
}

pub(crate) fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// Mixes two colours. `t` runs from 0 (all `a`) to 1 (all `b`).
pub(crate) fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let channel = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
            Color::Rgb(channel(r1, r2), channel(g1, g2), channel(b1, b2))
        }
        _ => b,
    }
}

/// A colour along the brand gradient, from iris through pink to gold.
pub(crate) fn gradient(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        mix(IRIS, PINK, t * 2.0)
    } else {
        mix(PINK, GOLD, (t - 0.5) * 2.0)
    }
}

pub(crate) fn danger_color(danger: u8) -> Color {
    match danger {
        0 => GREEN,
        1 => FOAM,
        2 => GOLD,
        3 => ORANGE,
        _ => PINK,
    }
}

pub(crate) fn danger_word(danger: u8) -> &'static str {
    match danger {
        0 => "SAFE",
        1 => "LOW",
        2 => "MEDIUM",
        3 => "HIGH",
        _ => "EXPERT",
    }
}

/// A one-cell spinner, driven by the frame count.
pub(crate) fn spinner(frame: u64) -> &'static str {
    const FANCY: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const SAFE: [&str; 6] = ["░", "▒", "▓", "█", "▓", "▒"];
    if fancy_glyphs() {
        FANCY[(frame / 2) as usize % FANCY.len()]
    } else {
        SAFE[(frame / 3) as usize % SAFE.len()]
    }
}

/// A slow pulse between 0 and 1, driven by the frame count.
pub(crate) fn pulse(frame: u64) -> f32 {
    let phase = (frame % 40) as f32 / 40.0 * std::f32::consts::TAU;
    (phase.sin() + 1.0) / 2.0
}

/// A filled pill, such as a badge or button.
pub(crate) fn pill(text: &str, background: Color, foreground: Color) -> Span<'static> {
    Span::styled(format!(" {text} "), filled(background, foreground))
}

/// A level meter such as `■■■··`, in the level's colour.
pub(crate) fn meter(filled_count: usize, total: usize, color: Color) -> Vec<Span<'static>> {
    let filled_count = filled_count.min(total);
    vec![
        Span::styled("■".repeat(filled_count), fg(color)),
        Span::styled("·".repeat(total - filled_count), fg(FAINT)),
    ]
}

/// A horizontal progress bar with a moving sheen, `width` cells wide.
pub(crate) fn progress_bar(fraction: f32, width: usize, frame: u64) -> Line<'static> {
    let fraction = fraction.clamp(0.0, 1.0);
    let done = (fraction * width as f32).round() as usize;
    let sheen = (frame / 2) as usize % (width.max(1) + 8);
    let spans = (0..width)
        .map(|i| {
            if i < done {
                let t = i as f32 / width.max(1) as f32;
                let near = sheen.abs_diff(i) as f32;
                let color = mix(gradient(t), TEXT, (0.5 - near * 0.15).max(0.0));
                Span::styled("█", fg(color))
            } else {
                Span::styled("░", fg(SURFACE_HIGH))
            }
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// Text with the brand gradient across it. `shift` moves a soft highlight along it.
pub(crate) fn gradient_text(text: &str, shift: Option<u64>) -> Vec<Span<'static>> {
    let count = text.chars().count().max(1);
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let mut color = gradient(i as f32 / count as f32);
            if let Some(shift) = shift {
                let position = (shift as usize) % (count + 30);
                let near = position.abs_diff(i) as f32;
                color = mix(color, TEXT, (0.8 - near * 0.3).max(0.0));
            }
            Span::styled(c.to_string(), fg(color).add_modifier(Modifier::BOLD))
        })
        .collect()
}

/// The big logo, for the first screen.
pub(crate) const LOGO: [&str; 5] = [
    " █████  ██   ██  ██████  ███████  ██████ ",
    "██   ██ ██   ██ ██       ██      ██    ██",
    "███████ ██   ██ ██   ███ █████   ██    ██",
    "██   ██ ██   ██ ██    ██ ██      ██    ██",
    "██   ██  █████   ██████  ███████  ██████ ",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_needs_a_value() {
        assert!(!asks_for_no_colour(None));
        assert!(!asks_for_no_colour(Some(std::ffi::OsStr::new(""))));
        assert!(asks_for_no_colour(Some(std::ffi::OsStr::new("1"))));
    }

    #[test]
    fn mixing_stays_between_the_two_colours() {
        assert_eq!(mix(rgb(0, 0, 0), rgb(100, 200, 50), 0.0), rgb(0, 0, 0));
        assert_eq!(mix(rgb(0, 0, 0), rgb(100, 200, 50), 1.0), rgb(100, 200, 50));
        assert_eq!(mix(rgb(0, 0, 0), rgb(100, 200, 50), 0.5), rgb(50, 100, 25));
        assert_eq!(mix(rgb(1, 1, 1), rgb(9, 9, 9), 7.0), rgb(9, 9, 9));
    }

    #[test]
    fn every_danger_level_has_a_distinct_word_and_colour() {
        let words: std::collections::BTreeSet<_> = (0..5).map(danger_word).collect();
        assert_eq!(words.len(), 5);
        for a in 0..5 {
            for b in (a + 1)..5 {
                assert_ne!(danger_color(a), danger_color(b));
            }
        }
    }

    #[test]
    fn progress_bars_are_exactly_as_wide_as_asked() {
        for fraction in [0.0, 0.3, 1.0, 2.0] {
            let bar = progress_bar(fraction, 20, 3);
            assert_eq!(bar.spans.len(), 20);
        }
    }

    #[test]
    fn the_logo_lines_line_up() {
        let width = LOGO[0].chars().count();
        assert!(LOGO.iter().all(|l| l.chars().count() == width));
    }
}
