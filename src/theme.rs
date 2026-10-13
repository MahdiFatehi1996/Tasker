//! The single VS Code Dark Modern palette, consumed by every widget.
//!
//! Neutral dark background, light foreground, grey secondary text, a blue
//! accent for section titles and the focused border, a blue selection fill,
//! yellow warnings, and red errors. No other module may name a [`Color`] —
//! the whole look is defined here, so a restyle is one file.

use ratatui::style::{Color, Modifier, Style};

/// Neutral dark backdrop.
pub const BACKGROUND: Color = Color::Rgb(31, 31, 31);
/// Light foreground: primary text and chrome.
pub const FOREGROUND: Color = Color::Rgb(204, 204, 204);
/// Grey secondary text, borders, hints.
pub const DIM: Color = Color::Rgb(128, 128, 128);
/// Blue accent: section titles and the focused border.
pub const ACCENT: Color = Color::Rgb(0, 120, 212);
/// Blue selection fill behind the selected row or day.
pub const SELECTION: Color = Color::Rgb(38, 79, 120);
/// Yellow warning: attention that is not yet an error.
pub const WARNING: Color = Color::Rgb(204, 167, 0);
/// Red error: the most urgent flags.
pub const ERROR: Color = Color::Rgb(241, 76, 76);

/// Base text: light foreground on the dark backdrop.
pub fn text() -> Style {
    Style::default().fg(FOREGROUND).bg(BACKGROUND)
}

/// Emphasised base text (bold foreground).
pub fn strong() -> Style {
    text().add_modifier(Modifier::BOLD)
}

/// Dimmed secondary text.
pub fn dim() -> Style {
    Style::default().fg(DIM).bg(BACKGROUND)
}

/// Crossed-out style for done rows (dim).
pub fn done() -> Style {
    dim().add_modifier(Modifier::CROSSED_OUT)
}

/// Selection: light text on the blue selection fill.
pub fn selection() -> Style {
    Style::default()
        .fg(FOREGROUND)
        .bg(SELECTION)
        .add_modifier(Modifier::BOLD)
}

/// Error style for the most urgent flags.
pub fn alert() -> Style {
    Style::default().fg(ERROR).bg(BACKGROUND)
}

/// Accent style for section titles and the focused border.
pub fn accent() -> Style {
    Style::default().fg(ACCENT).bg(BACKGROUND)
}

/// Warning style for attention that is not yet an error.
pub fn warning() -> Style {
    Style::default().fg(WARNING).bg(BACKGROUND)
}

/// Block border/backdrop style (dim on the backdrop).
pub fn block() -> Style {
    dim()
}

/// Block border style for the pane that holds keyboard focus (blue accent).
pub fn focused_block() -> Style {
    accent()
}

/// Style for a focused text caret / underlined draft.
pub fn caret() -> Style {
    text().add_modifier(Modifier::UNDERLINED)
}
