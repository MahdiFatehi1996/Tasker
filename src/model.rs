//! Domain types: the Tasker glossary as code.
//!
//! A [`Task`] is a title plus state (open/done), a [`Priority`], and one
//! free-text note body, pinned to a calendar day. Overdue is derived state
//! (open + day before today), never stored.

use chrono::NaiveDate;

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
}

impl Task {
    /// Flip the done state. Done is a toggle: completing and reopening
    /// are the same operation.
    pub fn toggle(&mut self) {
        self.done = !self.done;
    }
}

/// A task not yet stored: everything except id and done state.
/// Constructed by add-flows from ticket #3; tests are the first caller.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    pub day: NaiveDate,
    pub title: String,
    pub priority: Priority,
    pub notes: String,
}
