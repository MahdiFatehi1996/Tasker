//! Domain types: the Tasker glossary as code.
//!
//! A [`Task`] is a title plus state (open/done), a [`Priority`], one
//! free-text note body, an optional due date-time, accumulated tracked
//! seconds, and attached tags — pinned to a calendar day. Overdue is derived
//! state (open + day before today), never stored. Due-today and missed-due
//! are likewise derived from the due moment, never stored.

use chrono::{NaiveDate, NaiveDateTime};

/// Priority of a task: low, medium, or high. Defaults to medium.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    Low,
    #[default]
    Medium,
    High,
}

impl Priority {
    const VARIANTS: [(Priority, &'static str); 3] = [
        (Priority::Low, "low"),
        (Priority::Medium, "medium"),
        (Priority::High, "high"),
    ];

    /// Canonical lowercase name, as stored in the database.
    pub fn as_str(self) -> &'static str {
        Self::VARIANTS
            .into_iter()
            .find(|(priority, _)| *priority == self)
            .map(|(_, name)| name)
            .expect("every priority has a canonical name")
    }

    /// Single-letter badge shown on compact task rows (`L`/`M`/`H`).
    pub fn letter(self) -> char {
        match self {
            Priority::Low => 'L',
            Priority::Medium => 'M',
            Priority::High => 'H',
        }
    }

    /// Parse a canonical priority name (case-insensitive).
    pub fn parse(s: &str) -> Option<Priority> {
        let needle = s.trim().to_ascii_lowercase();
        Self::VARIANTS
            .into_iter()
            .find(|(_, name)| *name == needle)
            .map(|(priority, _)| priority)
    }

    /// Step through low → medium → high, wrapping around.
    pub fn cycle(self, delta: i32) -> Priority {
        let order = [Priority::Low, Priority::Medium, Priority::High];
        let index = order
            .into_iter()
            .position(|p| p == self)
            .expect("every priority is in the cycle");
        let len = order.len() as i32;
        order[(index as i32 + delta).rem_euclid(len) as usize]
    }
}

/// A task pinned to a calendar day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: i64,
    pub day: NaiveDate,
    pub title: String,
    pub done: bool,
    pub priority: Priority,
    pub notes: String,
    /// Optional exact due date-time (Pro). `None` means no deadline.
    pub due: Option<NaiveDateTime>,
    /// Accumulated tracked seconds (Pro timer). Never negative.
    pub tracked_secs: i64,
    /// Attached tags, sorted ascending (Pro). Empty means untagged.
    pub tags: Vec<String>,
}

impl Task {
    /// Flip the done state. Done is a toggle: completing and reopening
    /// are the same operation.
    pub fn toggle(&mut self) {
        self.done = !self.done;
    }
}

/// A task not yet stored: everything except id, done state, and timer total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    pub day: NaiveDate,
    pub title: String,
    pub priority: Priority,
    pub notes: String,
    /// Optional exact due date-time (Pro). `None` means no deadline.
    pub due: Option<NaiveDateTime>,
    /// Attached tags (Pro). Stored normalized (trimmed, lowercase).
    pub tags: Vec<String>,
}

impl NewTask {
    /// Minimal new task pinned to `day` with a title: medium priority,
    /// no notes, no due moment, no tags. Timer starts at zero on save.
    pub fn dated(day: NaiveDate, title: String) -> NewTask {
        NewTask {
            day,
            title,
            priority: Priority::Medium,
            notes: String::new(),
            due: None,
            tags: Vec::new(),
        }
    }
}

/// Normalize a tag name: trim + lowercase. Blank names are rejected.
pub fn normalize_tag(name: &str) -> Option<String> {
    let cleaned = name.trim().to_ascii_lowercase();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Normalize a tag list: normalize each, drop blanks, sort, dedupe.
pub fn normalize_tags(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = names.iter().filter_map(|n| normalize_tag(n)).collect();
    out.sort();
    out.dedup();
    out
}

/// Render attached tags for the editor draft (`work, admin`).
pub fn format_tags_input(tags: &[String]) -> String {
    tags.join(", ")
}

/// Parse an editor tags draft: comma-separated, normalized like stored
/// tags. Blank clears all tags; never fails, so any text commits.
pub fn parse_tags_input(s: &str) -> Vec<String> {
    normalize_tags(
        &s.split(',')
            .map(|part| part.to_string())
            .collect::<Vec<_>>(),
    )
}

/// Titles that are empty or whitespace-only can never be committed —
/// shared by add commits and edit commits so the rule lives once.
pub fn is_blank_title(title: &str) -> bool {
    title.trim().is_empty()
}

/// Display format for editing a due moment in the detail editor
/// (`YYYY-MM-DD HH:MM`); a blank draft means no deadline.
pub const DUE_INPUT_FORMAT: &str = "%Y-%m-%d %H:%M";

/// Render a due moment for the editor draft.
pub fn format_due_input(due: &NaiveDateTime) -> String {
    due.format(DUE_INPUT_FORMAT).to_string()
}

/// Parse an editor due draft: blank clears (`Some(None)`), a valid
/// `YYYY-MM-DD HH:MM` (also `YYYY-MM-DDTHH:MM` or date-only for midnight)
/// sets (`Some(Some(..))`); anything else is `None` and the caller keeps
/// the editor open so the typo can be fixed instead of saved.
pub fn parse_due_input(s: &str) -> Option<Option<NaiveDateTime>> {
    let text = s.trim();
    if text.is_empty() {
        return Some(None);
    }
    if let Ok(due) = NaiveDateTime::parse_from_str(text, DUE_INPUT_FORMAT) {
        return Some(Some(due));
    }
    if let Ok(due) = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M") {
        return Some(Some(due));
    }
    if let Ok(day) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Some(Some(day.and_hms_opt(0, 0, 0).expect("midnight exists")));
    }
    None
}

/// Render accumulated tracked seconds as `H:MM` (minutes truncated, never
/// negative). Sub-hour totals show `0:MM`; the hour count is unbounded.
pub fn format_tracked(secs: i64) -> String {
    let secs = secs.max(0);
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    format!("{hours}:{minutes:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noon() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 10)
            .expect("valid date")
            .and_hms_opt(12, 30, 0)
            .expect("valid datetime")
    }

    #[test]
    fn due_input_round_trips_display_format() {
        assert_eq!(format_due_input(&noon()), "2026-10-10 12:30");
        assert_eq!(parse_due_input("2026-10-10 12:30"), Some(Some(noon())));
    }

    #[test]
    fn due_input_accepts_variants_and_blank_clears() {
        assert_eq!(parse_due_input("  "), Some(None));
        assert_eq!(parse_due_input("2026-10-10T12:30"), Some(Some(noon())));
        let midnight = NaiveDate::from_ymd_opt(2026, 10, 10)
            .expect("valid date")
            .and_hms_opt(0, 0, 0)
            .expect("valid datetime");
        assert_eq!(parse_due_input("2026-10-10"), Some(Some(midnight)));
    }

    #[test]
    fn due_input_rejects_garbage() {
        assert_eq!(parse_due_input("soon"), None);
        assert_eq!(parse_due_input("2026-13-10 12:30"), None);
        assert_eq!(parse_due_input("12:30"), None);
    }

    #[test]
    fn tracked_seconds_render_as_h_mm() {
        assert_eq!(format_tracked(0), "0:00");
        assert_eq!(format_tracked(59), "0:00");
        assert_eq!(format_tracked(60), "0:01");
        assert_eq!(format_tracked(90), "0:01");
        assert_eq!(format_tracked(3599), "0:59");
        assert_eq!(format_tracked(3600), "1:00");
        assert_eq!(format_tracked(5400), "1:30");
        assert_eq!(format_tracked(36_000), "10:00");
    }

    #[test]
    fn tracked_seconds_clamp_negative_totals() {
        assert_eq!(format_tracked(-5), "0:00");
    }

    #[test]
    fn tags_input_round_trip() {
        let tags = vec!["admin".to_string(), "work".to_string()];
        assert_eq!(format_tags_input(&tags), "admin, work");
        assert_eq!(parse_tags_input("admin, work"), tags);
    }

    #[test]
    fn tags_input_normalizes_and_blank_clears() {
        assert_eq!(
            parse_tags_input(" Work ,, ADMIN,work "),
            vec!["admin".to_string(), "work".to_string()]
        );
        assert_eq!(parse_tags_input("  "), Vec::<String>::new());
    }
}
