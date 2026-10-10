//! Non-interactive command-line surface: `add` / `done` / `list` / `export`.
//!
//! Shares the storage seam with the TUI — every command reads and writes
//! through [`Database`], never SQLite directly, and resolves the database
//! file by the same order (explicit flag, `TASKER_DB`, platform data dir).
//! Argument parsing is a pure function ([`parse_args`]) tested without a
//! process; command execution returns the text to print, keeping stdout in
//! `main`. A bare `tasker` stays on [`Command::Tui`].

use std::path::PathBuf;

use chrono::{Local, NaiveDate};

use crate::db::Database;
use crate::model::{NewTask, Task, format_due_input, format_tracked};

/// Output format for `tasker export`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Full-fidelity dump: every task with every field.
    Json,
    /// Readable log of one day.
    Markdown,
}

/// A parsed command line (after `--db` extraction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Launch the interactive TUI (no arguments beyond `--db`).
    Tui,
    /// Add a task titled `title` to today.
    Add {
        /// The task title (trimmed; must be non-blank).
        title: String,
    },
    /// Mark task `id` done.
    Done {
        /// The task id to complete.
        id: i64,
    },
    /// Print today's view (sections + rows).
    List,
    /// Export tasks; `day` selects the markdown log day (markdown only).
    Export {
        /// Output format.
        format: ExportFormat,
        /// Day to log (markdown only); defaults to today.
        day: Option<NaiveDate>,
    },
}

/// Usage banner printed on argument errors.
pub const USAGE: &str = "\
tasker [--db <path>]                       launch the TUI
tasker add \"title\"                        add a task to today
tasker done <id>                           mark a task done
tasker list                                print today's view
tasker export --format json|markdown       dump all tasks or log a day
                [--day YYYY-MM-DD]         (markdown: day to log)";

/// Parse the process argument list (including `argv[0]`) into an optional
/// explicit database path and a command. Extracts `--db <path>` /
/// `--db=<path>` wherever it appears so the two surfaces share one resolution
/// order.
pub fn parse_args(args: &[String]) -> Result<(Option<PathBuf>, Command), String> {
    let mut explicit_db = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if let Some(path) = arg.strip_prefix("--db=") {
            explicit_db = Some(PathBuf::from(path));
            i += 1;
        } else if arg == "--db" {
            let value = args.get(i + 1).ok_or("--db needs a path")?;
            explicit_db = Some(PathBuf::from(value));
            i += 2;
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }
    Ok((explicit_db, parse_command(&positional)?))
}

fn parse_command(positional: &[String]) -> Result<Command, String> {
    match positional.first().map(String::as_str) {
        None => Ok(Command::Tui),
        Some("add") => {
            let title = positional[1..].join(" ").trim().to_string();
            if title.is_empty() {
                Err("add needs a title".to_string())
            } else {
                Ok(Command::Add { title })
            }
        }
        Some("done") => match positional.get(1) {
            None => Err("done needs a task id".to_string()),
            Some(raw) => raw
                .parse::<i64>()
                .map(|id| Command::Done { id })
                .map_err(|_| format!("done needs a numeric task id, got: {raw}")),
        },
        Some("list") => Ok(Command::List),
        Some("export") => parse_export(&positional[1..]),
        Some(other) => Err(format!("unknown command: {other}")),
    }
}

fn parse_export(rest: &[String]) -> Result<Command, String> {
    let mut format = None;
    let mut day = None;
    let mut i = 0;
    while i < rest.len() {
        let arg = &rest[i];
        if let Some(value) = arg.strip_prefix("--format=") {
            format = Some(parse_format(value)?);
            i += 1;
        } else if arg == "--format" {
            let value = rest.get(i + 1).ok_or("--format needs json or markdown")?;
            format = Some(parse_format(value)?);
            i += 2;
        } else if let Some(value) = arg.strip_prefix("--day=") {
            day = Some(parse_day(value)?);
            i += 1;
        } else if arg == "--day" {
            let value = rest.get(i + 1).ok_or("--day needs YYYY-MM-DD")?;
            day = Some(parse_day(value)?);
            i += 2;
        } else {
            return Err(format!("unknown export option: {arg}"));
        }
    }
    let format = format.ok_or("export needs --format json|markdown")?;
    if day.is_some() && format != ExportFormat::Markdown {
        return Err("--day only applies to --format markdown".to_string());
    }
    Ok(Command::Export { format, day })
}

fn parse_format(value: &str) -> Result<ExportFormat, String> {
    match value {
        "json" => Ok(ExportFormat::Json),
        "markdown" => Ok(ExportFormat::Markdown),
        other => Err(format!("unknown export format: {other}")),
    }
}

fn parse_day(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| format!("invalid day: {value}"))
}

/// Run a command against `db`, returning the text to print. The TUI has no
/// textual output; the runtime launches it directly instead of calling here.
pub fn execute(db: &Database, command: Command) -> Result<String, String> {
    match command {
        Command::Tui => Ok(String::new()),
        Command::Add { title } => add(db, &title),
        Command::Done { id } => done(db, id),
        Command::List => list(db),
        Command::Export { format, day } => export(db, format, day),
    }
}

fn add(db: &Database, title: &str) -> Result<String, String> {
    let today = Local::now().date_naive();
    let id = db
        .save_task(&NewTask::dated(today, title.to_string()))
        .map_err(|e| e.to_string())?;
    Ok(format!("Added task {id}: {title}"))
}

fn done(db: &Database, id: i64) -> Result<String, String> {
    let mut task = db
        .task_by_id(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no task with id {id}"))?;
    task.done = true;
    db.update_task(&task).map_err(|e| e.to_string())?;
    Ok(format!("Done task {id}: {}", task.title))
}

/// Today's view as plain text: missed-due, due-today, overdue, then the day,
/// matching the TUI sections. Empty sections are omitted.
fn list(db: &Database) -> Result<String, String> {
    let now = Local::now().naive_local();
    let today = now.date();
    let sections = [
        (
            "Missed due",
            db.missed_due_tasks(now).map_err(|e| e.to_string())?,
        ),
        (
            "Due today",
            db.due_today_tasks(today).map_err(|e| e.to_string())?,
        ),
        (
            "Overdue",
            db.overdue_tasks(today).map_err(|e| e.to_string())?,
        ),
        ("Today", db.tasks_for_day(today).map_err(|e| e.to_string())?),
    ];

    let mut out = String::new();
    for (heading, rows) in &sections {
        if rows.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(heading);
        out.push('\n');
        for task in rows {
            out.push_str(&list_line(task));
            out.push('\n');
        }
    }
    if out.is_empty() {
        Ok("No tasks today.".to_string())
    } else {
        Ok(out)
    }
}

/// One plain-text list row: `  [ ] (M) title  due ...  #tag  H:MM`.
fn list_line(task: &Task) -> String {
    let mut line = format!(
        "  {} ({}) {}",
        if task.done { "[x]" } else { "[ ]" },
        task.priority.letter(),
        task.title
    );
    if let Some(due) = task.due {
        line.push_str(&format!("  due {}", format_due_input(&due)));
    }
    if !task.tags.is_empty() {
        line.push_str(&format!("  #{}", task.tags.join(" #")));
    }
    if task.tracked_secs > 0 {
        line.push_str(&format!("  {}", format_tracked(task.tracked_secs)));
    }
    line
}

fn export(db: &Database, format: ExportFormat, day: Option<NaiveDate>) -> Result<String, String> {
    match format {
        ExportFormat::Json => Ok(tasks_json(&db.all_tasks().map_err(|e| e.to_string())?)),
        ExportFormat::Markdown => {
            let day = day.unwrap_or_else(|| Local::now().date_naive());
            let tasks = db.tasks_for_day(day).map_err(|e| e.to_string())?;
            Ok(day_markdown(day, &tasks))
        }
    }
}

/// Full-fidelity JSON array: every field per task, `null` for a missing due
/// moment, empty array for no tags.
fn tasks_json(tasks: &[Task]) -> String {
    let mut out = String::from("[\n");
    for (i, task) in tasks.iter().enumerate() {
        let due = match task.due {
            Some(due) => json_string(&due.format("%Y-%m-%dT%H:%M:%S").to_string()),
            None => "null".to_string(),
        };
        let tags = task
            .tags
            .iter()
            .map(|tag| json_string(tag))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str("  {\n");
        out.push_str(&format!("    \"id\": {},\n", task.id));
        out.push_str(&format!(
            "    \"day\": {},\n",
            json_string(&task.day.format("%Y-%m-%d").to_string())
        ));
        out.push_str(&format!("    \"title\": {},\n", json_string(&task.title)));
        out.push_str(&format!("    \"done\": {},\n", task.done));
        out.push_str(&format!(
            "    \"priority\": {},\n",
            json_string(task.priority.as_str())
        ));
        out.push_str(&format!("    \"notes\": {},\n", json_string(&task.notes)));
        out.push_str(&format!("    \"due\": {due},\n"));
        out.push_str(&format!("    \"tracked_secs\": {},\n", task.tracked_secs));
        out.push_str(&format!("    \"tags\": [{tags}]\n"));
        let comma = if i + 1 == tasks.len() { "" } else { "," };
        out.push_str(&format!("  }}{comma}\n"));
    }
    out.push(']');
    out
}

/// Readable markdown log for one day: heading, then a bullet per task with
/// due, tags, tracked time, and indented note lines.
fn day_markdown(day: NaiveDate, tasks: &[Task]) -> String {
    let mut out = format!("# Tasker — {}\n", day.format("%Y-%m-%d"));
    if tasks.is_empty() {
        out.push_str("\n_No tasks._\n");
        return out;
    }
    out.push('\n');
    for task in tasks {
        let mut line = format!(
            "- [{}] ({}) {}",
            if task.done { "x" } else { " " },
            task.priority.as_str(),
            task.title
        );
        if let Some(due) = task.due {
            line.push_str(&format!(" — due {}", format_due_input(&due)));
        }
        if !task.tags.is_empty() {
            line.push_str(&format!(" — #{}", task.tags.join(" #")));
        }
        if task.tracked_secs > 0 {
            line.push_str(&format!(" — {}", format_tracked(task.tracked_secs)));
        }
        out.push_str(&line);
        out.push('\n');
        for note_line in task.notes.lines() {
            out.push_str(&format!("  {note_line}\n"));
        }
    }
    out
}

/// Escape a string as a JSON string literal, including the surrounding quotes.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Priority;

    fn args(parts: &[&str]) -> Vec<String> {
        std::iter::once("tasker")
            .chain(parts.iter().copied())
            .map(str::to_string)
            .collect()
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date")
    }

    #[test]
    fn bare_invocation_is_the_tui() {
        assert_eq!(parse_args(&args(&[])), Ok((None, Command::Tui)));
    }

    #[test]
    fn add_collects_and_trims_the_title() {
        assert_eq!(
            parse_args(&args(&["add", "buy", "milk"])),
            Ok((
                None,
                Command::Add {
                    title: "buy milk".to_string()
                }
            ))
        );
        assert!(parse_args(&args(&["add"])).is_err());
        assert!(parse_args(&args(&["add", "   "])).is_err());
    }

    #[test]
    fn done_requires_a_numeric_id() {
        assert_eq!(
            parse_args(&args(&["done", "42"])),
            Ok((None, Command::Done { id: 42 }))
        );
        assert!(parse_args(&args(&["done"])).is_err());
        assert!(parse_args(&args(&["done", "abc"])).is_err());
    }

    #[test]
    fn list_parses() {
        assert_eq!(parse_args(&args(&["list"])), Ok((None, Command::List)));
    }

    #[test]
    fn export_requires_a_format() {
        assert!(parse_args(&args(&["export"])).is_err());
        assert_eq!(
            parse_args(&args(&["export", "--format", "json"])),
            Ok((
                None,
                Command::Export {
                    format: ExportFormat::Json,
                    day: None,
                }
            ))
        );
    }

    #[test]
    fn export_reads_format_and_day_in_both_shapes() {
        assert_eq!(
            parse_args(&args(&[
                "export",
                "--format",
                "markdown",
                "--day",
                "2026-10-01"
            ])),
            Ok((
                None,
                Command::Export {
                    format: ExportFormat::Markdown,
                    day: Some(NaiveDate::from_ymd_opt(2026, 10, 1).expect("valid date")),
                }
            ))
        );
        assert_eq!(
            parse_args(&args(&["export", "--format=markdown", "--day=2026-10-02"])),
            Ok((
                None,
                Command::Export {
                    format: ExportFormat::Markdown,
                    day: Some(NaiveDate::from_ymd_opt(2026, 10, 2).expect("valid date")),
                }
            ))
        );
    }

    #[test]
    fn export_rejects_bad_options() {
        assert!(parse_args(&args(&["export", "--format", "yaml"])).is_err());
        assert!(parse_args(&args(&["export", "--format", "md"])).is_err());
        assert!(parse_args(&args(&["export", "--day", "soon"])).is_err());
        assert!(parse_args(&args(&["export", "--nope"])).is_err());
        assert!(parse_args(&args(&["export", "--format"])).is_err());
        assert!(
            parse_args(&args(&[
                "export",
                "--format",
                "json",
                "--day",
                "2026-10-01"
            ]))
            .is_err(),
            "--day only applies to markdown"
        );
    }

    #[test]
    fn unknown_command_is_rejected() {
        assert!(parse_args(&args(&["frobnicate"])).is_err());
    }

    #[test]
    fn db_flag_extracted_in_both_shapes() {
        let (db, command) = parse_args(&args(&["--db", "E:\\x.db", "list"])).expect("parse");
        assert_eq!(db, Some(PathBuf::from("E:\\x.db")));
        assert_eq!(command, Command::List);

        let (db, command) =
            parse_args(&args(&["export", "--format", "json", "--db=E:\\y.db"])).expect("parse");
        assert_eq!(db, Some(PathBuf::from("E:\\y.db")));
        assert_eq!(
            command,
            Command::Export {
                format: ExportFormat::Json,
                day: None
            }
        );

        assert!(parse_args(&args(&["--db"])).is_err());
    }

    fn seeded_db() -> Database {
        let db = Database::open_in_memory().expect("open in-memory database");
        let id = db
            .save_task(&NewTask {
                day: day(),
                title: "Quarterly report".to_string(),
                priority: Priority::High,
                notes: "line one\nline two".to_string(),
                due: Some(day().and_hms_opt(9, 30, 0).expect("valid datetime")),
                tags: vec!["work".to_string(), "admin".to_string()],
            })
            .expect("save task");
        db.add_tracked_secs(id, 90).expect("bank seconds");
        db
    }

    #[test]
    fn add_then_json_export_carries_every_field() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let output = execute(
            &db,
            Command::Add {
                title: "Buy milk".to_string(),
            },
        )
        .expect("add");
        assert!(output.contains("Buy milk"));

        let json = execute(
            &db,
            Command::Export {
                format: ExportFormat::Json,
                day: None,
            },
        )
        .expect("export json");
        assert!(json.contains("\"title\": \"Buy milk\""), "{json}");
        assert!(json.contains("\"due\": null"), "{json}");
        assert!(json.contains("\"tags\": []"), "{json}");
        assert!(json.contains("\"tracked_secs\": 0"), "{json}");
    }

    #[test]
    fn json_export_includes_due_tags_and_timer() {
        let db = seeded_db();
        let json = execute(
            &db,
            Command::Export {
                format: ExportFormat::Json,
                day: None,
            },
        )
        .expect("export json");
        assert!(json.contains("\"due\": \"2026-10-10T09:30:00\""), "{json}");
        assert!(json.contains("\"tags\": [\"admin\", \"work\"]"), "{json}");
        assert!(json.contains("\"tracked_secs\": 90"), "{json}");
    }

    #[test]
    fn json_export_escapes_special_characters() {
        let db = Database::open_in_memory().expect("open in-memory database");
        db.save_task(&NewTask {
            day: day(),
            title: "Quote \" and \\ slash".to_string(),
            priority: Priority::Medium,
            notes: String::new(),
            due: None,
            tags: Vec::new(),
        })
        .expect("save task");
        let json = execute(
            &db,
            Command::Export {
                format: ExportFormat::Json,
                day: None,
            },
        )
        .expect("export json");
        assert!(
            json.contains(r#""title": "Quote \" and \\ slash""#),
            "{json}"
        );
    }

    #[test]
    fn markdown_export_renders_the_day_readably() {
        let db = seeded_db();
        let md = execute(
            &db,
            Command::Export {
                format: ExportFormat::Markdown,
                day: Some(day()),
            },
        )
        .expect("export markdown");
        assert!(md.starts_with("# Tasker — 2026-10-10"), "{md}");
        assert!(
            md.contains(
                "- [ ] (high) Quarterly report — due 2026-10-10 09:30 — #admin #work — 0:01"
            ),
            "{md}"
        );
        assert!(md.contains("  line one\n  line two"), "{md}");
    }

    #[test]
    fn markdown_export_of_empty_day_notes_nothing() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let md = execute(
            &db,
            Command::Export {
                format: ExportFormat::Markdown,
                day: Some(day()),
            },
        )
        .expect("export markdown");
        assert!(md.contains("_No tasks._"), "{md}");
    }

    #[test]
    fn done_marks_the_task_and_unknown_id_errors() {
        let db = seeded_db();
        let id = db.all_tasks().expect("list")[0].id;
        let output = execute(&db, Command::Done { id }).expect("done");
        assert!(output.contains(&id.to_string()));
        assert!(db.task_by_id(id).expect("load").expect("exists").done);
        assert!(execute(&db, Command::Done { id: 9999 }).is_err());
    }

    #[test]
    fn list_prints_today_sections() {
        let db = Database::open_in_memory().expect("open in-memory database");
        // `list` reads the real clock, so derive the days the same way.
        let today = Local::now().date_naive();
        db.save_task(&NewTask::dated(today, "Today work".to_string()))
            .expect("save today");
        db.save_task(&NewTask {
            day: today - chrono::Duration::days(3),
            title: "Overdue work".to_string(),
            priority: Priority::Medium,
            notes: String::new(),
            due: None,
            tags: Vec::new(),
        })
        .expect("save overdue");
        let output = execute(&db, Command::List).expect("list");
        assert!(output.contains("Overdue"), "{output}");
        assert!(output.contains("Overdue work"), "{output}");
        assert!(output.contains("Today work"), "{output}");
    }

    #[test]
    fn list_reports_an_empty_day() {
        let db = Database::open_in_memory().expect("open in-memory database");
        assert_eq!(
            execute(&db, Command::List).expect("list"),
            "No tasks today."
        );
    }
}
