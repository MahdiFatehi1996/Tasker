//! The single amber-phosphor palette, consumed by every widget.
//!
//! Near-black brown background, amber primary, dim amber secondary,
//! reversed-amber selection, red alerts. No other module may name a
//! [`Color`] — the whole look is defined here, so a restyle is one file.

use ratatui::style::{Color, Modifier, Style};

/// Near-black brown backdrop.
pub const BACKGROUND: Color = Color::Rgb(24, 16, 8);
/// Amber phosphor: primary text and chrome.
pub const PRIMARY: Color = Color::Rgb(255, 176, 0);
/// Dim amber: secondary text, borders, hints.
pub const DIM: Color = Color::Rgb(120, 88, 40);
/// Alert red: errors and the most urgent flags.
pub const ALERT: Color = Color::Rgb(255, 72, 56);

/// Base text: amber on the near-black brown backdrop.
pub fn text() -> Style {
    Style::default().fg(PRIMARY).bg(BACKGROUND)
}

/// Emphasised base text (bold amber).
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

/// Selection: reversed amber (dark text on amber).
pub fn selection() -> Style {
    Style::default()
        .fg(BACKGROUND)
        .bg(PRIMARY)
        .add_modifier(Modifier::BOLD)
}

/// Alert style for urgent flags.
pub fn alert() -> Style {
    Style::default().fg(ALERT).bg(BACKGROUND)
}

/// Block border/backdrop style (dim on the backdrop).
pub fn block() -> Style {
    dim()
}

/// Block border style for the pane that holds keyboard focus (bright amber).
pub fn focused_block() -> Style {
    text()
}

/// Style for a focused text caret / underlined draft.
pub fn caret() -> Style {
    text().add_modifier(Modifier::UNDERLINED)
}
