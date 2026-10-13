//! Non-interactive command-line surface: `add` / `done` / `list` / `export`
//! plus the Agent interface's `capabilities`.
//!
//! Shares the storage seam with the TUI — every command reads and writes
//! through [`Database`], never SQLite directly, and resolves the database
//! file by the same order (explicit flag, `TASKER_DB`, platform data dir).
//! Argument parsing is a pure function ([`parse_args`]) tested without a
//! process; command execution returns the text to print, keeping stdout in
//! `main`. A bare `tasker` stays on [`Command::Tui`].
//!
//! Failures are typed ([`CommandError`]) and carry exactly one class, so the
//! process entry point decides the exit code through the pure [`exit_code`]
//! mapping instead of hard-coding it.

use std::path::PathBuf;

use chrono::{Local, NaiveDate};

use crate::db::Database;
use crate::json::JsonWriter;
use crate::model::{NewTask, Task, format_due_input, format_tracked};

/// A command failure, classified so a caller can map it to an exit code.
/// Exactly one class: usage, not-found, or storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// The invocation is unusable: bad arguments or unknown options.
    Usage(String),
    /// The addressed task does not exist.
    NotFound(String),
    /// The storage seam failed.
    Storage(String),
}

impl CommandError {
    /// A usage (argument) failure.
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    /// A missing-task failure.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    /// A storage-seam failure.
    pub fn storage(message: impl Into<String>) -> Self {
        Self::Storage(message.into())
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::Usage(message)
            | CommandError::NotFound(message)
            | CommandError::Storage(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for CommandError {}

/// Wrap a storage-seam failure as a [`CommandError::Storage`].
fn storage(error: impl std::fmt::Display) -> CommandError {
    CommandError::storage(error.to_string())
}

/// The Agent interface contract version. Bumped when the shape of the
/// machine-readable contract changes incompatibly; reported by `capabilities`.
pub const CONTRACT_VERSION: i64 = 1;

/// Map a command outcome to its process exit code. This is the documented
/// exit-code table of the Agent interface, and it deliberately differs from
/// the first CLI release (where not-found and storage both exited `1`):
///
/// | outcome         | code |
/// | --------------- | ---- |
/// | success         | 0    |
/// | usage error     | 2    |
/// | not found       | 3    |
/// | storage failure | 4    |
pub fn exit_code(outcome: &Result<String, CommandError>) -> i32 {
    match outcome {
        Ok(_) => 0,
        Err(CommandError::Usage(_)) => 2,
        Err(CommandError::NotFound(_)) => 3,
        Err(CommandError::Storage(_)) => 4,
    }
}

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
    /// Report the Agent interface contract version and the storage schema
    /// version, so a caller can detect an incompatible Tasker up front.
    Capabilities {
        /// Emit the machine-readable envelope instead of human text.
        json: bool,
    },
}

/// Usage banner printed on argument errors.
pub const USAGE: &str = "\
tasker [--db <path>]                       launch the TUI
tasker add \"title\"                        add a task to today
tasker done <id>                           mark a task done
tasker list                                print today's view
tasker export --format json|markdown       dump all tasks or log a day
                [--day YYYY-MM-DD]         (markdown: day to log)
tasker capabilities [--json]               report contract and schema versions
exit codes: 0 success, 2 usage, 3 not found, 4 storage";

/// Parse the process argument list (including `argv[0]`) into an optional
/// explicit database path and a command. Extracts `--db <path>` /
/// `--db=<path>` wherever it appears so the two surfaces share one resolution
/// order.
pub fn parse_args(args: &[String]) -> Result<(Option<PathBuf>, Command), CommandError> {
    let mut explicit_db = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if let Some(path) = arg.strip_prefix("--db=") {
            explicit_db = Some(PathBuf::from(path));
            i += 1;
        } else if arg == "--db" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| CommandError::usage("--db needs a path"))?;
            explicit_db = Some(PathBuf::from(value));
            i += 2;
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }
    Ok((explicit_db, parse_command(&positional)?))
}

fn parse_command(positional: &[String]) -> Result<Command, CommandError> {
    match positional.first().map(String::as_str) {
        None => Ok(Command::Tui),
        Some("add") => {
            let title = positional[1..].join(" ").trim().to_string();
            if title.is_empty() {
                Err(CommandError::usage("add needs a title"))
            } else {
                Ok(Command::Add { title })
            }
        }
        Some("done") => match positional.get(1) {
            None => Err(CommandError::usage("done needs a task id")),
            Some(raw) => raw
                .parse::<i64>()
                .map(|id| Command::Done { id })
                .map_err(|_| {
                    CommandError::usage(format!("done needs a numeric task id, got: {raw}"))
                }),
        },
        Some("list") => Ok(Command::List),
        Some("export") => parse_export(&positional[1..]),
        Some("capabilities") => parse_capabilities(&positional[1..]),
        Some(other) => Err(CommandError::usage(format!("unknown command: {other}"))),
    }
}

fn parse_export(rest: &[String]) -> Result<Command, CommandError> {
    let mut format = None;
    let mut day = None;
    let mut i = 0;
    while i < rest.len() {
        let arg = &rest[i];
        if let Some(value) = arg.strip_prefix("--format=") {
            format = Some(parse_format(value)?);
            i += 1;
        } else if arg == "--format" {
            let value = rest
                .get(i + 1)
                .ok_or_else(|| CommandError::usage("--format needs json or markdown"))?;
            format = Some(parse_format(value)?);
            i += 2;
        } else if let Some(value) = arg.strip_prefix("--day=") {
            day = Some(parse_day(value)?);
            i += 1;
        } else if arg == "--day" {
            let value = rest
                .get(i + 1)
                .ok_or_else(|| CommandError::usage("--day needs YYYY-MM-DD"))?;
            day = Some(parse_day(value)?);
            i += 2;
        } else {
            return Err(CommandError::usage(format!("unknown export option: {arg}")));
        }
    }
    let format =
        format.ok_or_else(|| CommandError::usage("export needs --format json|markdown"))?;
    if day.is_some() && format != ExportFormat::Markdown {
        return Err(CommandError::usage(
            "--day only applies to --format markdown",
        ));
    }
    Ok(Command::Export { format, day })
}

/// Parse `capabilities` options: no flags for human text, or `--json` for the
/// machine-readable envelope.
fn parse_capabilities(rest: &[String]) -> Result<Command, CommandError> {
    let mut json = false;
    for arg in rest {
        if arg == "--json" {
            json = true;
        } else {
            return Err(CommandError::usage(format!(
                "unknown capabilities option: {arg}"
            )));
        }
    }
    Ok(Command::Capabilities { json })
}

fn parse_format(value: &str) -> Result<ExportFormat, CommandError> {
    match value {
        "json" => Ok(ExportFormat::Json),
        "markdown" => Ok(ExportFormat::Markdown),
        other => Err(CommandError::usage(format!(
            "unknown export format: {other}"
        ))),
    }
}

fn parse_day(value: &str) -> Result<NaiveDate, CommandError> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| CommandError::usage(format!("invalid day: {value}")))
}

/// Run a command against `db`, returning the text to print. The TUI has no
/// textual output; the runtime launches it directly instead of calling here.
pub fn execute(db: &Database, command: Command) -> Result<String, CommandError> {
    match command {
        Command::Tui => Ok(String::new()),
        Command::Add { title } => add(db, &title),
        Command::Done { id } => done(db, id),
        Command::List => list(db),
        Command::Export { format, day } => export(db, format, day),
        Command::Capabilities { json } => capabilities(json),
    }
}

fn add(db: &Database, title: &str) -> Result<String, CommandError> {
    let today = Local::now().date_naive();
    let id = db
        .save_task(&NewTask::dated(today, title.to_string()))
        .map_err(storage)?;
    Ok(format!("Added task {id}: {title}"))
}

fn done(db: &Database, id: i64) -> Result<String, CommandError> {
    let mut task = db
        .task_by_id(id)
        .map_err(storage)?
        .ok_or_else(|| CommandError::not_found(format!("no task with id {id}")))?;
    task.done = true;
    db.update_task(&task).map_err(storage)?;
    Ok(format!("Done task {id}: {}", task.title))
}

/// Today's view as plain text: missed-due, due-today, overdue, then the day,
/// matching the TUI sections. Empty sections are omitted.
fn list(db: &Database) -> Result<String, CommandError> {
    let now = Local::now().naive_local();
    let today = now.date();
    let sections = [
        ("Missed due", db.missed_due_tasks(now).map_err(storage)?),
        ("Due today", db.due_today_tasks(today).map_err(storage)?),
        ("Overdue", db.overdue_tasks(today).map_err(storage)?),
        ("Today", db.tasks_for_day(today).map_err(storage)?),
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

/// Commands the current contract version exposes, reported by `capabilities`.
const COMMANDS: &[&str] = &["add", "capabilities", "done", "export", "list"];

/// Report the Agent interface contract version and the storage schema version.
/// Human text is the default; `--json` wraps the command inventory in the
/// agent envelope carrying both versions.
fn capabilities(json: bool) -> Result<String, CommandError> {
    let schema = crate::db::SCHEMA_VERSION;
    if json {
        let payload = crate::json::string_array(COMMANDS);
        Ok(crate::json::envelope(CONTRACT_VERSION, schema, &payload))
    } else {
        Ok(format!(
            "Tasker agent interface\ncontract: {CONTRACT_VERSION}\nschema: {schema}"
        ))
    }
}

fn export(
    db: &Database,
    format: ExportFormat,
    day: Option<NaiveDate>,
) -> Result<String, CommandError> {
    match format {
        ExportFormat::Json => {
            let tasks = db.all_tasks().map_err(storage)?;
            let mut writer = JsonWriter::new();
            writer.collection(&tasks);
            Ok(writer.finish())
        }
        ExportFormat::Markdown => {
            let day = day.unwrap_or_else(|| Local::now().date_naive());
            let tasks = db.tasks_for_day(day).map_err(storage)?;
            Ok(day_markdown(day, &tasks))
        }
    }
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

    /// Assert `text` is well-formed JSON. A deliberately small recursive
    /// validator, independent of the production writer, so tests prove the
    /// emitted bytes parse without adding a serialisation dependency.
    fn assert_json_valid(text: &str) {
        json_check(text).unwrap_or_else(|error| panic!("invalid JSON: {error}\n{text}"));
    }

    fn json_check(text: &str) -> Result<(), String> {
        let mut parser = JsonCheck {
            bytes: text.as_bytes(),
            at: 0,
        };
        parser.value()?;
        parser.space();
        if parser.at == parser.bytes.len() {
            Ok(())
        } else {
            Err(format!("trailing input at byte {}", parser.at))
        }
    }

    /// Recursive-descent JSON grammar check (an RFC 8259 subset).
    struct JsonCheck<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl JsonCheck<'_> {
        fn space(&mut self) {
            while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.at += 1;
            }
        }

        fn value(&mut self) -> Result<(), String> {
            self.space();
            match self.bytes.get(self.at) {
                Some(b'{') => self.object(),
                Some(b'[') => self.array(),
                Some(b'"') => self.string(),
                Some(b't') => self.keyword("true"),
                Some(b'f') => self.keyword("false"),
                Some(b'n') => self.keyword("null"),
                Some(b'-' | b'0'..=b'9') => self.number(),
                other => Err(format!("unexpected {other:?} at byte {}", self.at)),
            }
        }

        fn keyword(&mut self, word: &str) -> Result<(), String> {
            if self.bytes[self.at..].starts_with(word.as_bytes()) {
                self.at += word.len();
                Ok(())
            } else {
                Err(format!("expected {word} at byte {}", self.at))
            }
        }

        fn object(&mut self) -> Result<(), String> {
            self.at += 1;
            self.space();
            if self.bytes.get(self.at) == Some(&b'}') {
                self.at += 1;
                return Ok(());
            }
            loop {
                self.space();
                self.string()?;
                self.space();
                if self.bytes.get(self.at) != Some(&b':') {
                    return Err(format!("expected ':' at byte {}", self.at));
                }
                self.at += 1;
                self.value()?;
                self.space();
                match self.bytes.get(self.at) {
                    Some(b',') => self.at += 1,
                    Some(b'}') => {
                        self.at += 1;
                        return Ok(());
                    }
                    other => {
                        return Err(format!(
                            "expected ',' or '}}' at byte {}: {other:?}",
                            self.at
                        ));
                    }
                }
            }
        }

        fn array(&mut self) -> Result<(), String> {
            self.at += 1;
            self.space();
            if self.bytes.get(self.at) == Some(&b']') {
                self.at += 1;
                return Ok(());
            }
            loop {
                self.value()?;
                self.space();
                match self.bytes.get(self.at) {
                    Some(b',') => self.at += 1,
                    Some(b']') => {
                        self.at += 1;
                        return Ok(());
                    }
                    other => {
                        return Err(format!(
                            "expected ',' or ']' at byte {}: {other:?}",
                            self.at
                        ));
                    }
                }
            }
        }

        fn string(&mut self) -> Result<(), String> {
            if self.bytes.get(self.at) != Some(&b'"') {
                return Err(format!("expected string at byte {}", self.at));
            }
            self.at += 1;
            loop {
                match self.bytes.get(self.at) {
                    None => return Err("unterminated string".to_string()),
                    Some(b'"') => {
                        self.at += 1;
                        return Ok(());
                    }
                    Some(b'\\') => {
                        self.at += 1;
                        match self.bytes.get(self.at) {
                            Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                                self.at += 1;
                            }
                            Some(b'u') => {
                                self.at += 1;
                                for _ in 0..4 {
                                    match self.bytes.get(self.at) {
                                        Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F') => {
                                            self.at += 1;
                                        }
                                        _ => {
                                            return Err(format!(
                                                "bad \\u escape at byte {}",
                                                self.at
                                            ));
                                        }
                                    }
                                }
                            }
                            other => {
                                return Err(format!("bad escape {other:?} at byte {}", self.at));
                            }
                        }
                    }
                    Some(_) => self.at += 1,
                }
            }
        }

        fn number(&mut self) -> Result<(), String> {
            let start = self.at;
            if self.bytes.get(self.at) == Some(&b'-') {
                self.at += 1;
            }
            while matches!(
                self.bytes.get(self.at),
                Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
            ) {
                self.at += 1;
            }
            let text = std::str::from_utf8(&self.bytes[start..self.at])
                .map_err(|error| error.to_string())?;
            text.parse::<f64>()
                .map_err(|error| format!("bad number {text:?}: {error}"))?;
            Ok(())
        }
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
    fn capabilities_parses_with_and_without_json() {
        assert_eq!(
            parse_args(&args(&["capabilities"])),
            Ok((None, Command::Capabilities { json: false }))
        );
        assert_eq!(
            parse_args(&args(&["capabilities", "--json"])),
            Ok((None, Command::Capabilities { json: true }))
        );
        assert!(parse_args(&args(&["capabilities", "--nope"])).is_err());
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

    #[test]
    fn capabilities_human_output_names_the_versions() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let text = execute(&db, Command::Capabilities { json: false }).expect("capabilities");
        assert!(
            text.contains(&format!("contract: {CONTRACT_VERSION}")),
            "{text}"
        );
        assert!(
            text.contains(&format!("schema: {}", crate::db::SCHEMA_VERSION)),
            "{text}"
        );
        assert!(!text.starts_with('{'), "human output is not JSON: {text}");
    }

    #[test]
    fn capabilities_json_parses_and_reports_contract_and_schema() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let json = execute(&db, Command::Capabilities { json: true }).expect("capabilities");
        assert_json_valid(&json);
        assert!(
            json.contains(&format!("\"contract\": {CONTRACT_VERSION}")),
            "{json}"
        );
        assert!(
            json.contains(&format!("\"schema\": {}", crate::db::SCHEMA_VERSION)),
            "{json}"
        );
        assert!(json.contains("\"payload\""), "{json}");
    }

    #[test]
    fn exit_codes_are_the_documented_contract() {
        assert_eq!(exit_code(&Ok(String::new())), 0, "success is 0");
        assert_eq!(exit_code(&Err(CommandError::usage("bad args"))), 2);
        assert_eq!(exit_code(&Err(CommandError::not_found("no task"))), 3);
        assert_eq!(exit_code(&Err(CommandError::storage("disk full"))), 4);
    }

    #[test]
    fn argument_errors_carry_the_usage_class() {
        let error = parse_args(&args(&["done"])).expect_err("missing id");
        assert!(matches!(error, CommandError::Usage(_)), "{error:?}");
        let error = parse_args(&args(&["frobnicate"])).expect_err("unknown command");
        assert!(matches!(error, CommandError::Usage(_)), "{error:?}");
    }

    #[test]
    fn missing_task_carries_the_not_found_class() {
        let db = seeded_db();
        let error = execute(&db, Command::Done { id: 9999 }).expect_err("no such task");
        assert!(matches!(error, CommandError::NotFound(_)), "{error:?}");
    }

    #[test]
    fn json_export_is_a_bare_array_not_an_envelope() {
        let db = seeded_db();
        let json = execute(
            &db,
            Command::Export {
                format: ExportFormat::Json,
                day: None,
            },
        )
        .expect("export json");
        assert_json_valid(&json);
        assert!(json.starts_with('['), "export stays a bare array: {json}");
        assert!(
            !json.contains("\"contract\""),
            "export must not be wrapped in the agent envelope: {json}"
        );
    }

    #[test]
    fn json_export_is_byte_identical() {
        let db = seeded_db();
        let json = execute(
            &db,
            Command::Export {
                format: ExportFormat::Json,
                day: None,
            },
        )
        .expect("export json");
        let expected = r#"[
  {
    "id": 1,
    "day": "2026-10-10",
    "title": "Quarterly report",
    "done": false,
    "priority": "high",
    "notes": "line one\nline two",
    "due": "2026-10-10T09:30:00",
    "tracked_secs": 90,
    "tags": ["admin", "work"]
  }
]"#;
        assert_eq!(json, expected);
    }

    #[test]
    fn json_export_of_an_empty_database_is_an_empty_array() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let json = execute(
            &db,
            Command::Export {
                format: ExportFormat::Json,
                day: None,
            },
        )
        .expect("export json");
        assert_eq!(json, "[\n]");
    }
}
