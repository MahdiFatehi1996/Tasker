//! Tasker: a TUI daily task manager with a non-interactive CLI companion.
//!
//! Bare `tasker` launches the TUI; `add` / `done` / `rm` / `list` / `export`
//! run against the same storage seam. See issues #8 and #13 for the Pro spec.

mod app;
mod cli;
mod db;
mod json;
mod model;
mod theme;
mod ui;

use app::App;
use db::{Database, resolve_db_path};
use model::format_due_input;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (db_arg, command) = match cli::parse_args(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("{}", cli::USAGE);
            std::process::exit(cli::exit_code(&Err(error)));
        }
    };
    let database = Database::open(&resolve_db_path(db_arg))?;
    match command {
        cli::Command::Tui => run_tui(&database),
        command => {
            let outcome = cli::execute(&database, command);
            if let Ok(output) = &outcome
                && !output.is_empty()
            {
                println!("{output}");
            }
            if let Err(error) = &outcome {
                eprintln!("error: {error}");
            }
            std::process::exit(cli::exit_code(&outcome));
        }
    }
}

/// Launch the interactive day-list TUI for `database`.
fn run_tui(database: &Database) -> Result<(), Box<dyn std::error::Error>> {
    let now = chrono::Local::now().naive_local();
    let today = now.date();
    let mut app = App::new(
        today,
        database.tasks_for_day(today)?,
        database.overdue_tasks(today)?,
    );
    app.set_due_today(database.due_today_tasks(today)?);
    let missed = database.missed_due_tasks(now)?;
    notify_missed_due(&missed);
    app.set_missed_due(missed);
    app.set_all_tags(database.all_tags()?);
    ui::run(database, &mut app)?;
    Ok(())
}

/// One-shot launch toast: one desktop notification per task already past
/// due. Best-effort — a headless session or missing notifier must never
/// block startup, so delivery failures are ignored.
fn notify_missed_due(missed: &[model::Task]) {
    for task in missed {
        let body = match task.due {
            Some(due) => format!("{} (due {})", task.title, format_due_input(&due)),
            None => task.title.clone(),
        };
        let _ = notify_rust::Notification::new()
            .summary("Tasker — past due")
            .body(&body)
            .show();
    }
}
