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
    /// First used by flows in ticket #3; tests are the first caller.
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        Self::VARIANTS
            .into_iter()
            .find(|(priority, _)| *priority == self)
            .map(|(_, name)| name)
            .expect("every priority has a canonical name")
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
/// Constructed by add-flows from ticket #3; tests are the first caller.
#[allow(dead_code)]
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
    #[allow(dead_code)]
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

/// Titles that are empty or whitespace-only can never be committed —
/// shared by add commits and edit commits so the rule lives once.
pub fn is_blank_title(title: &str) -> bool {
    title.trim().is_empty()
}
