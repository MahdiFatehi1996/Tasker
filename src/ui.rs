//! UI layer (foundation): today-at-a-glance summary.
//!
//! Full TUI (day list, calendar, detail pane) arrives with later tickets;
//! this stub keeps every layer wired end to end from the first slice.

use std::path::Path;

use chrono::Local;

use crate::db::Database;

/// Open the database (already done by the caller) and report today's state.
pub fn run(db: &Database, path: &Path) -> Result<(), crate::db::Error> {
    let today = Local::now().date_naive();
    let tasks = db.tasks_for_day(today)?;
    let done = tasks.iter().filter(|task| task.done).count();
    println!(
        "Tasker — {}/{} tasks done today — db: {}",
        done,
        tasks.len(),
        path.display()
    );
    Ok(())
}
