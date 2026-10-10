//! Storage seam: the only module that touches SQLite.
//!
//! All task persistence goes through [`Database`]; the UI never opens a
//! connection directly. [`resolve_db_path`] decides where the database file
//! lives: explicit flag, then `TASKER_DB`, then the platform data dir.
//!
//! Schema v2 (Pro P1) adds: a nullable ISO-datetime `due` column on tasks,
//! a `tags` table plus `task_tags` join, and integer `tracked_secs` on tasks.
//! [`Database::open`] migrates v1 databases idempotently.

use std::path::{Path, PathBuf};

use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;

use crate::model::{NewTask, Priority, Task, normalize_tag, normalize_tags};

/// Name of the environment variable overriding the database location.
pub const ENV_DB_PATH: &str = "TASKER_DB";

/// Current schema version, recorded in the database for future migrations.
pub const SCHEMA_VERSION: i64 = 2;

/// Date format used for the `day` column (`YYYY-MM-DD`, lexicographically sortable).
const DAY_FORMAT: &str = "%Y-%m-%d";

/// Date-time format used for the nullable `due` column
/// (`YYYY-MM-DDTHH:MM:SS`, lexicographically sortable, naive local time).
const DUE_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    day TEXT NOT NULL,
    title TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0,
    priority TEXT NOT NULL DEFAULT 'medium',
    notes TEXT NOT NULL DEFAULT ''
);
";

const SCHEMA_TAGS: &str = "
CREATE TABLE IF NOT EXISTS tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS task_tags (
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (task_id, tag_id)
);
";

/// Errors from the storage layer: filesystem or SQLite.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Sql(rusqlite::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "storage I/O error: {e}"),
            Error::Sql(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Sql(e) => Some(e),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Sql(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

/// Task storage backed by a SQLite database file.
pub struct Database {
    conn: Connection,
}

fn column_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(tasks)")?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    names.collect()
}

fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_V1)?;
    // Idempotent v1 -> v2 migration: add columns only when missing so
    // existing v1 databases keep their rows.
    let columns = column_names(conn).unwrap_or_default();
    if !columns.iter().any(|c| c == "due") {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN due TEXT;")?;
    }
    if !columns.iter().any(|c| c == "tracked_secs") {
        conn.execute_batch(
            "ALTER TABLE tasks ADD COLUMN tracked_secs INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    conn.execute_batch(SCHEMA_TAGS)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn enable_foreign_keys(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn parse_day(s: &str) -> rusqlite::Result<NaiveDate> {
    NaiveDate::parse_from_str(s, DAY_FORMAT).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_due(s: &str) -> rusqlite::Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, DUE_FORMAT).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn format_due(due: &NaiveDateTime) -> String {
    due.format(DUE_FORMAT).to_string()
}

fn parse_priority(s: &str) -> rusqlite::Result<Priority> {
    Priority::parse(s).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(UnknownPriority(format!("unknown priority: {s}"))),
        )
    })
}

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let day_text: String = row.get(1)?;
    let priority_text: String = row.get(4)?;
    let due_text: Option<String> = row.get(6)?;
    let due = due_text.as_deref().map(parse_due).transpose()?;
    Ok(Task {
        id: row.get(0)?,
        day: parse_day(&day_text)?,
        title: row.get(2)?,
        done: row.get::<_, i64>(3)? != 0,
        priority: parse_priority(&priority_text)?,
        notes: row.get(5)?,
        due,
        tracked_secs: row.get(7)?,
        tags: Vec::new(),
    })
}

#[derive(Debug)]
struct UnknownPriority(String);

impl std::fmt::Display for UnknownPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for UnknownPriority {}

fn load_tags(conn: &Connection, task_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT tags.name FROM tags JOIN task_tags ON tags.id = task_tags.tag_id WHERE task_tags.task_id = ?1 ORDER BY tags.name",
    )?;
    let names = stmt.query_map([task_id], |row| row.get::<_, String>(0))?;
    names.collect()
}

fn sync_tags(conn: &Connection, task_id: i64, tags: &[String]) -> rusqlite::Result<()> {
    let cleaned = normalize_tags(tags);
    conn.execute("DELETE FROM task_tags WHERE task_id = ?1", [task_id])?;
    for name in &cleaned {
        conn.execute("INSERT OR IGNORE INTO tags (name) VALUES (?1)", [name])?;
        let tag_id: i64 = conn.query_row("SELECT id FROM tags WHERE name = ?1", [name], |row| {
            row.get(0)
        })?;
        conn.execute(
            "INSERT OR IGNORE INTO task_tags (task_id, tag_id) VALUES (?1, ?2)",
            rusqlite::params![task_id, tag_id],
        )?;
    }
    // Prune orphan tags so the table never accumulates dead names.
    conn.execute(
        "DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM task_tags)",
        [],
    )?;
    Ok(())
}

const TASK_COLUMNS: &str = "id, day, title, done, priority, notes, due, tracked_secs";

impl Database {
    /// Open (creating if needed) the database at `path`, creating parent
    /// directories and initializing/migrating the schema idempotently.
    pub fn open(path: &Path) -> Result<Database, Error> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        enable_foreign_keys(&conn)?;
        init_schema(&conn)?;
        Ok(Database { conn })
    }

    /// Open a private throwaway in-memory database (used by tests).
    /// Consumed by later tickets' flows; tests are the first caller.
    #[allow(dead_code)]
    pub fn open_in_memory() -> Result<Database, Error> {
        let conn = Connection::open_in_memory()?;
        enable_foreign_keys(&conn)?;
        init_schema(&conn)?;
        Ok(Database { conn })
    }

    fn populate_tags(&self, tasks: &mut [Task]) -> Result<(), Error> {
        for task in tasks.iter_mut() {
            task.tags = load_tags(&self.conn, task.id)?;
        }
        Ok(())
    }

    fn collect_with_tags(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Task>, Error> {
        let mut tasks: Vec<Task> = {
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map(params, task_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        self.populate_tags(&mut tasks)?;
        Ok(tasks)
    }

    /// Store a new task; returns its assigned id. New tasks are never done
    /// and start with zero tracked seconds.
    /// First wired into user flows by ticket #3; tests are the first caller.
    #[allow(dead_code)]
    pub fn save_task(&self, task: &NewTask) -> Result<i64, Error> {
        let due_text = task.due.as_ref().map(format_due);
        self.conn.execute(
            "INSERT INTO tasks (day, title, done, priority, notes, due, tracked_secs) VALUES (?1, ?2, 0, ?3, ?4, ?5, 0)",
            (
                task.day.format(DAY_FORMAT).to_string(),
                &task.title,
                task.priority.as_str(),
                &task.notes,
                due_text,
            ),
        )?;
        let id = self.conn.last_insert_rowid();
        sync_tags(&self.conn, id, &task.tags)?;
        Ok(id)
    }

    /// Persist changes to an existing task's title, state, priority, notes,
    /// due moment, tracked seconds, and tags. The task keeps its original
    /// day and id.
    /// First wired into user flows by ticket #6; tests are the first caller.
    #[allow(dead_code)]
    pub fn update_task(&self, task: &Task) -> Result<(), Error> {
        let due_text = task.due.as_ref().map(format_due);
        self.conn.execute(
            "UPDATE tasks SET title = ?1, done = ?2, priority = ?3, notes = ?4, due = ?5, tracked_secs = ?6 WHERE id = ?7",
            (
                &task.title,
                task.done as i64,
                task.priority.as_str(),
                &task.notes,
                due_text,
                task.tracked_secs.max(0),
                task.id,
            ),
        )?;
        sync_tags(&self.conn, task.id, &task.tags)?;
        Ok(())
    }

    /// Single task by id, with tags, or `None` when missing.
    /// Wired into user flows by later Pro tickets; tests are the first caller.
    #[allow(dead_code)]
    pub fn task_by_id(&self, id: i64) -> Result<Option<Task>, Error> {
        let tasks = self.collect_with_tags(
            &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"),
            [id],
        )?;
        Ok(tasks.into_iter().next())
    }

    /// All tasks pinned to `day`, in insertion order, with tags.
    pub fn tasks_for_day(&self, day: NaiveDate) -> Result<Vec<Task>, Error> {
        self.collect_with_tags(
            &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE day = ?1 ORDER BY id"),
            [day.format(DAY_FORMAT).to_string()],
        )
    }

    /// Open tasks from days before `today`, oldest first: the Overdue
    /// section. Done tasks and today/future tasks are never overdue.
    /// Day pins never change — overdue is derived, not re-dated.
    pub fn overdue_tasks(&self, today: NaiveDate) -> Result<Vec<Task>, Error> {
        self.collect_with_tags(
            &format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE day < ?1 AND done = 0 ORDER BY day, id"
            ),
            [today.format(DAY_FORMAT).to_string()],
        )
    }

    /// Open tasks whose due moment falls within `today` (date part equals
    /// `today`), oldest due first: the Due-today section. Done tasks and
    /// tasks without a due moment are excluded.
    /// Wired into user flows by Pro P2; tests are the first caller.
    #[allow(dead_code)]
    pub fn due_today_tasks(&self, today: NaiveDate) -> Result<Vec<Task>, Error> {
        self.collect_with_tags(
            &format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE done = 0 AND due IS NOT NULL AND substr(due, 1, 10) = ?1 ORDER BY due, id"
            ),
            [today.format(DAY_FORMAT).to_string()],
        )
    }

    /// Open tasks whose due moment is strictly before `now`, oldest due
    /// first: the Missed-due section. Done tasks and tasks without a due
    /// moment are excluded.
    /// Wired into user flows by Pro P2; tests are the first caller.
    #[allow(dead_code)]
    pub fn missed_due_tasks(&self, now: NaiveDateTime) -> Result<Vec<Task>, Error> {
        self.collect_with_tags(
            &format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE done = 0 AND due IS NOT NULL AND due < ?1 ORDER BY due, id"
            ),
            [format_due(&now)],
        )
    }

    /// Set (or clear with `None`) a task's due moment.
    /// Wired into user flows by Pro P2; tests are the first caller.
    #[allow(dead_code)]
    pub fn set_due(&self, task_id: i64, due: Option<NaiveDateTime>) -> Result<(), Error> {
        let due_text = due.as_ref().map(format_due);
        self.conn.execute(
            "UPDATE tasks SET due = ?1 WHERE id = ?2",
            rusqlite::params![due_text, task_id],
        )?;
        Ok(())
    }

    /// All tags attached to a task, sorted ascending.
    /// Wired into user flows by Pro P3; tests are the first caller.
    #[allow(dead_code)]
    pub fn tags_for_task(&self, task_id: i64) -> Result<Vec<String>, Error> {
        Ok(load_tags(&self.conn, task_id)?)
    }

    /// Attach a tag to a task (normalized; blanks rejected with no-op).
    /// Idempotent: attaching twice keeps one join row.
    /// Wired into user flows by Pro P3; tests are the first caller.
    #[allow(dead_code)]
    pub fn add_tag(&self, task_id: i64, name: &str) -> Result<(), Error> {
        let Some(cleaned) = normalize_tag(name) else {
            return Ok(());
        };
        self.conn
            .execute("INSERT OR IGNORE INTO tags (name) VALUES (?1)", [&cleaned])?;
        let tag_id: i64 =
            self.conn
                .query_row("SELECT id FROM tags WHERE name = ?1", [&cleaned], |row| {
                    row.get(0)
                })?;
        self.conn.execute(
            "INSERT OR IGNORE INTO task_tags (task_id, tag_id) VALUES (?1, ?2)",
            rusqlite::params![task_id, tag_id],
        )?;
        Ok(())
    }

    /// Detach a tag from a task; prunes the tag name when orphaned.
    /// Wired into user flows by Pro P3; tests are the first caller.
    #[allow(dead_code)]
    pub fn remove_tag(&self, task_id: i64, name: &str) -> Result<(), Error> {
        let Some(cleaned) = normalize_tag(name) else {
            return Ok(());
        };
        let tag_id: Option<i64> = self
            .conn
            .query_row("SELECT id FROM tags WHERE name = ?1", [&cleaned], |row| {
                row.get(0)
            })
            .ok();
        if let Some(tag_id) = tag_id {
            self.conn.execute(
                "DELETE FROM task_tags WHERE task_id = ?1 AND tag_id = ?2",
                rusqlite::params![task_id, tag_id],
            )?;
            self.conn.execute(
                "DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM task_tags)",
                [],
            )?;
        }
        Ok(())
    }

    /// All tasks carrying `tag` (any day), oldest day first, with tags.
    /// Wired into user flows by Pro P3; tests are the first caller.
    #[allow(dead_code)]
    pub fn tasks_with_tag(&self, tag: &str) -> Result<Vec<Task>, Error> {
        let Some(cleaned) = normalize_tag(tag) else {
            return Ok(Vec::new());
        };
        self.collect_with_tags(
            "SELECT tasks.id, tasks.day, tasks.title, tasks.done, tasks.priority, tasks.notes, tasks.due, tasks.tracked_secs FROM tasks JOIN task_tags ON tasks.id = task_tags.task_id JOIN tags ON tags.id = task_tags.tag_id WHERE tags.name = ?1 ORDER BY tasks.day, tasks.id",
            [cleaned],
        )
    }

    /// Add (or subtract, clamped at zero) tracked seconds for a task.
    /// Survives restarts: the total lives in the tasks row.
    /// Wired into user flows by Pro P4; tests are the first caller.
    #[allow(dead_code)]
    pub fn add_tracked_secs(&self, task_id: i64, delta_secs: i64) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE tasks SET tracked_secs = CASE WHEN tracked_secs + ?1 < 0 THEN 0 ELSE tracked_secs + ?1 END WHERE id = ?2",
            rusqlite::params![delta_secs, task_id],
        )?;
        Ok(())
    }

    /// Schema version recorded in the database (migration gate).
    /// Read by upgrade checks in later tickets; tests are the first caller.
    #[allow(dead_code)]
    pub fn schema_version(&self) -> Result<i64, Error> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }
}

/// Resolve the database file location: explicit path wins, then the
/// `TASKER_DB` environment variable, then the platform data dir.
pub fn resolve_db_path(explicit: Option<PathBuf>) -> PathBuf {
    resolve_db_path_with_env(explicit, &|key| std::env::var(key).ok())
}

/// [`resolve_db_path`] with injectable environment lookup (keeps tests
/// deterministic without touching process-global state).
fn resolve_db_path_with_env(
    explicit: Option<PathBuf>,
    getenv: &dyn Fn(&str) -> Option<String>,
) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Some(path) = getenv(ENV_DB_PATH)
        && !path.trim().is_empty()
    {
        return PathBuf::from(path);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tasker")
        .join("tasks.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn test_day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date")
    }

    fn test_noon() -> NaiveDateTime {
        test_day().and_hms_opt(12, 0, 0).expect("valid datetime")
    }

    fn memory_db() -> Database {
        Database::open_in_memory().expect("open in-memory database")
    }

    fn temp_db_path() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("tasker-test-{}-{}.db", std::process::id(), n))
    }

    fn sample_task() -> NewTask {
        NewTask {
            day: test_day(),
            title: "Write the quarterly report".to_string(),
            priority: Priority::High,
            notes: "Include the revenue chart.".to_string(),
            due: None,
            tags: Vec::new(),
        }
    }

    fn stored_task(day: NaiveDate, title: &str) -> Task {
        Task {
            id: 0,
            day,
            title: title.to_string(),
            done: false,
            priority: Priority::Medium,
            notes: String::new(),
            due: None,
            tracked_secs: 0,
            tags: Vec::new(),
        }
    }

    #[test]
    fn fresh_database_returns_empty_day_list() {
        let db = memory_db();
        let tasks = db.tasks_for_day(test_day()).expect("query fresh database");
        assert!(tasks.is_empty());
    }

    #[test]
    fn fresh_database_records_schema_version() {
        let db = memory_db();
        assert_eq!(
            db.schema_version().expect("read schema version"),
            SCHEMA_VERSION
        );
    }

    #[test]
    fn fresh_database_has_pro_columns() {
        let db = memory_db();
        let id = db.save_task(&sample_task()).expect("save task");
        let task = db.task_by_id(id).expect("load task").expect("task exists");
        assert_eq!(task.due, None);
        assert_eq!(task.tracked_secs, 0);
        assert!(task.tags.is_empty());
    }

    #[test]
    fn saved_task_round_trips_all_fields() {
        let db = memory_db();
        let day = test_day();

        let id = db.save_task(&sample_task()).expect("save task");
        let tasks = db.tasks_for_day(day).expect("query day");

        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0],
            Task {
                id,
                day,
                title: "Write the quarterly report".to_string(),
                done: false,
                priority: Priority::High,
                notes: "Include the revenue chart.".to_string(),
                due: None,
                tracked_secs: 0,
                tags: Vec::new(),
            }
        );
    }

    #[test]
    fn saved_task_round_trips_due_tags_and_timer() {
        let db = memory_db();
        let due = test_noon();
        let id = db
            .save_task(&NewTask {
                day: test_day(),
                title: "Quarterly report".to_string(),
                priority: Priority::High,
                notes: "Notes.".to_string(),
                due: Some(due),
                tags: vec!["work".to_string(), " Admin ".to_string()],
            })
            .expect("save task");
        let task = db.task_by_id(id).expect("load task").expect("task exists");
        assert_eq!(task.due, Some(due));
        assert_eq!(task.tags, vec!["admin".to_string(), "work".to_string()]);
        assert_eq!(task.tracked_secs, 0);
    }

    #[test]
    fn tasks_for_day_returns_only_that_day() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let monday = NaiveDate::from_ymd_opt(2026, 10, 12).expect("valid date");
        let tuesday = NaiveDate::from_ymd_opt(2026, 10, 13).expect("valid date");

        for (day, title) in [(monday, "Monday work"), (tuesday, "Tuesday work")] {
            db.save_task(&NewTask {
                day,
                title: title.to_string(),
                priority: Priority::Medium,
                notes: String::new(),
                due: None,
                tags: Vec::new(),
            })
            .expect("save task");
        }

        let tasks = db.tasks_for_day(monday).expect("query monday");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Monday work");
        assert_eq!(tasks[0].day, monday);
    }

    #[test]
    fn toggled_done_survives_reopen() {
        let path = temp_db_path();
        let day = NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date");

        let db = Database::open(&path).expect("open file database");
        db.save_task(&sample_task()).expect("save task");
        let mut tasks = db.tasks_for_day(day).expect("query day");
        assert!(!tasks[0].done);
        tasks[0].done = true;
        db.update_task(&tasks[0]).expect("toggle done");
        drop(db);

        let db = Database::open(&path).expect("reopen file database");
        let tasks = db.tasks_for_day(day).expect("query day after reopen");
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].done, "toggled state survives a restart");
        drop(db);

        std::fs::remove_file(&path).expect("remove temp db");
    }

    #[test]
    fn overdue_returns_only_open_past_tasks() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let past = NaiveDate::from_ymd_opt(2026, 10, 8).expect("valid date");
        let today = NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date");
        let future = NaiveDate::from_ymd_opt(2026, 10, 12).expect("valid date");

        let overdue_id = db
            .save_task(&NewTask {
                day: past,
                title: "Unfinished past work".to_string(),
                priority: Priority::Medium,
                notes: String::new(),
                due: None,
                tags: Vec::new(),
            })
            .expect("save past open task");
        let done_id = db
            .save_task(&NewTask {
                day: past,
                title: "Finished past work".to_string(),
                priority: Priority::Medium,
                notes: String::new(),
                due: None,
                tags: Vec::new(),
            })
            .expect("save past task to complete");
        db.update_task(&Task {
            id: done_id,
            day: past,
            title: "Finished past work".to_string(),
            done: true,
            priority: Priority::Medium,
            notes: String::new(),
            due: None,
            tracked_secs: 0,
            tags: Vec::new(),
        })
        .expect("complete past task");
        db.save_task(&NewTask {
            day: today,
            title: "Today work".to_string(),
            priority: Priority::Medium,
            notes: String::new(),
            due: None,
            tags: Vec::new(),
        })
        .expect("save today task");
        db.save_task(&NewTask {
            day: future,
            title: "Future work".to_string(),
            priority: Priority::Medium,
            notes: String::new(),
            due: None,
            tags: Vec::new(),
        })
        .expect("save future task");

        let overdue = db.overdue_tasks(today).expect("query overdue");
        assert_eq!(overdue.len(), 1);
        assert_eq!(overdue[0].id, overdue_id);
        assert_eq!(overdue[0].title, "Unfinished past work");
        assert_eq!(overdue[0].day, past, "overdue keeps its original day");
    }

    #[test]
    fn completed_task_leaves_overdue() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let past = NaiveDate::from_ymd_opt(2026, 10, 8).expect("valid date");
        let today = NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date");
        let id = db
            .save_task(&NewTask {
                day: past,
                title: "Past work".to_string(),
                priority: Priority::Medium,
                notes: String::new(),
                due: None,
                tags: Vec::new(),
            })
            .expect("save past open task");
        assert_eq!(db.overdue_tasks(today).expect("query overdue").len(), 1);

        let mut tasks = db.tasks_for_day(past).expect("query past day");
        tasks[0].done = true;
        db.update_task(&tasks[0]).expect("complete past task");
        assert_eq!(id, tasks[0].id);

        assert!(
            db.overdue_tasks(today)
                .expect("query overdue after completion")
                .is_empty(),
            "completing a task removes it from Overdue"
        );
    }

    #[test]
    fn update_task_persists_changes() {
        let db = memory_db();
        let day = test_day();
        let id = db.save_task(&sample_task()).expect("save task");

        db.update_task(&Task {
            id,
            day,
            title: "Write the annual report".to_string(),
            done: true,
            priority: Priority::Low,
            notes: "Updated notes.".to_string(),
            due: Some(test_noon()),
            tracked_secs: 90,
            tags: vec!["work".to_string()],
        })
        .expect("update task");

        let tasks = db.tasks_for_day(day).expect("query day");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Write the annual report");
        assert!(tasks[0].done);
        assert_eq!(tasks[0].priority, Priority::Low);
        assert_eq!(tasks[0].notes, "Updated notes.");
        assert_eq!(tasks[0].due, Some(test_noon()));
        assert_eq!(tasks[0].tracked_secs, 90);
        assert_eq!(tasks[0].tags, vec!["work".to_string()]);
    }

    #[test]
    fn reopening_file_database_preserves_tasks() {
        let path = temp_db_path();
        let day = test_day();

        {
            let db = Database::open(&path).expect("open file database");
            db.save_task(&sample_task()).expect("save task");
        }
        assert!(path.exists());

        let db = Database::open(&path).expect("reopen file database");
        let tasks = db.tasks_for_day(day).expect("query day");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Write the quarterly report");
        drop(db);

        std::fs::remove_file(&path).expect("remove temp db");
    }

    #[test]
    fn v1_database_migrates_idempotently_preserving_tasks() {
        let path = temp_db_path();
        // Build a genuine v1 database on disk: old table, version marker 1.
        {
            let conn = rusqlite::Connection::open(&path).expect("open raw connection");
            conn.execute_batch(
                "CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, day TEXT NOT NULL, title TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0, priority TEXT NOT NULL DEFAULT 'medium', notes TEXT NOT NULL DEFAULT '');",
            )
            .expect("create v1 schema");
            conn.execute(
                "INSERT INTO tasks (day, title, done, priority, notes) VALUES ('2026-10-09', 'Legacy work', 0, 'high', 'kept')",
                [],
            )
            .expect("insert legacy task");
            conn.pragma_update(None, "user_version", 1)
                .expect("mark version 1");
        }

        let db = Database::open(&path).expect("open migrates to v2");
        assert_eq!(db.schema_version().expect("read version"), SCHEMA_VERSION);
        let tasks = db
            .tasks_for_day(NaiveDate::from_ymd_opt(2026, 10, 9).expect("valid date"))
            .expect("query migrated day");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Legacy work");
        assert_eq!(tasks[0].due, None);
        assert_eq!(tasks[0].tracked_secs, 0);
        assert!(tasks[0].tags.is_empty());

        // Reopening stays at v2 with rows intact: migration is idempotent.
        drop(db);
        let db = Database::open(&path).expect("reopen migrated database");
        assert_eq!(db.schema_version().expect("read version"), SCHEMA_VERSION);
        assert_eq!(
            db.tasks_for_day(NaiveDate::from_ymd_opt(2026, 10, 9).expect("valid date"))
                .expect("query day")
                .len(),
            1
        );
        // New Pro columns are usable after migration.
        let id = tasks[0].id;
        db.set_due(id, Some(test_noon())).expect("set due");
        db.add_tag(id, "work").expect("add tag");
        db.add_tracked_secs(id, 60).expect("add seconds");
        let reloaded = db.task_by_id(id).expect("load").expect("exists");
        assert_eq!(reloaded.due, Some(test_noon()));
        assert_eq!(reloaded.tags, vec!["work".to_string()]);
        assert_eq!(reloaded.tracked_secs, 60);
        drop(db);

        std::fs::remove_file(&path).expect("remove temp db");
    }

    #[test]
    fn due_today_includes_only_open_tasks_due_today() {
        let db = memory_db();
        let today = test_day();
        let morning = today.and_hms_opt(9, 0, 0).expect("valid datetime");
        let evening = today.and_hms_opt(18, 30, 0).expect("valid datetime");
        let tomorrow = NaiveDate::from_ymd_opt(2026, 10, 11)
            .expect("valid date")
            .and_hms_opt(9, 0, 0)
            .expect("valid datetime");

        let mut first = stored_task(today, "Morning deadline");
        first.due = Some(morning);
        let mut second = stored_task(today, "Evening deadline");
        second.due = Some(evening);
        let mut future = stored_task(today, "Tomorrow deadline");
        future.due = Some(tomorrow);
        let undated = stored_task(today, "No deadline");
        let mut done = stored_task(today, "Done deadline");
        done.due = Some(morning);

        let mut ids = Vec::new();
        for t in [&first, &second, &future, &undated, &done] {
            ids.push(
                db.save_task(&NewTask {
                    day: t.day,
                    title: t.title.clone(),
                    priority: Priority::Medium,
                    notes: String::new(),
                    due: t.due,
                    tags: Vec::new(),
                })
                .expect("save task"),
            );
        }
        db.update_task(&Task {
            id: ids[4],
            done: true,
            ..done.clone()
        })
        .expect("complete one");

        let due_today = db.due_today_tasks(today).expect("query due today");
        assert_eq!(due_today.len(), 2);
        assert_eq!(due_today[0].title, "Morning deadline");
        assert_eq!(due_today[1].title, "Evening deadline");
    }

    #[test]
    fn missed_due_includes_only_open_tasks_before_now() {
        let db = memory_db();
        let now = test_noon();
        let past = test_day().and_hms_opt(9, 0, 0).expect("valid datetime");
        let future = test_day().and_hms_opt(15, 0, 0).expect("valid datetime");

        let late_id = db
            .save_task(&NewTask {
                day: test_day(),
                title: "Late work".to_string(),
                priority: Priority::Medium,
                notes: String::new(),
                due: Some(past),
                tags: Vec::new(),
            })
            .expect("save late task");
        db.save_task(&NewTask {
            day: test_day(),
            title: "Upcoming work".to_string(),
            priority: Priority::Medium,
            notes: String::new(),
            due: Some(future),
            tags: Vec::new(),
        })
        .expect("save future task");
        db.save_task(&NewTask {
            day: test_day(),
            title: "Undated work".to_string(),
            priority: Priority::Medium,
            notes: String::new(),
            due: None,
            tags: Vec::new(),
        })
        .expect("save undated task");
        let done_id = db
            .save_task(&NewTask {
                day: test_day(),
                title: "Done late work".to_string(),
                priority: Priority::Medium,
                notes: String::new(),
                due: Some(past),
                tags: Vec::new(),
            })
            .expect("save done task");
        let done = db.task_by_id(done_id).expect("load").expect("exists");
        db.update_task(&Task { done: true, ..done })
            .expect("complete task");

        let missed = db.missed_due_tasks(now).expect("query missed");
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].id, late_id);
        assert_eq!(missed[0].due, Some(past));
    }

    #[test]
    fn completing_due_task_clears_due_sections() {
        let db = memory_db();
        let now = test_noon();
        let id = db
            .save_task(&NewTask {
                day: test_day(),
                title: "Due work".to_string(),
                priority: Priority::Medium,
                notes: String::new(),
                due: Some(now),
                tags: Vec::new(),
            })
            .expect("save task");
        assert_eq!(db.due_today_tasks(test_day()).expect("due today").len(), 1);

        let mut task = db.task_by_id(id).expect("load").expect("exists");
        task.done = true;
        db.update_task(&task).expect("complete task");

        assert!(
            db.due_today_tasks(test_day())
                .expect("due today")
                .is_empty()
        );
        assert!(db.missed_due_tasks(now).expect("missed").is_empty());
    }

    #[test]
    fn tag_attach_detach_filter_round_trip() {
        let db = memory_db();
        let day = test_day();
        let id = db.save_task(&sample_task()).expect("save task");

        db.add_tag(id, "Work").expect("attach work");
        db.add_tag(id, "admin").expect("attach admin");
        db.add_tag(id, "work").expect("re-attach is idempotent");
        db.add_tag(id, "   ").expect("blank is a no-op");
        assert_eq!(
            db.tags_for_task(id).expect("list tags"),
            vec!["admin".to_string(), "work".to_string()]
        );

        let other = db
            .save_task(&NewTask {
                day,
                title: "Other work".to_string(),
                priority: Priority::Low,
                notes: String::new(),
                due: None,
                tags: vec!["work".to_string()],
            })
            .expect("save second task");
        let filtered = db.tasks_with_tag("WORK").expect("filter by tag");
        assert_eq!(filtered.len(), 2);

        db.remove_tag(id, "admin").expect("detach admin");
        assert_eq!(
            db.tags_for_task(id).expect("list tags"),
            vec!["work".to_string()]
        );
        assert_eq!(db.tasks_with_tag("admin").expect("filter admin").len(), 0);

        // Clearing the last tag restores the unfiltered picture: the task
        // still exists on its day, just untagged.
        db.remove_tag(id, "work").expect("detach work");
        db.remove_tag(other, "work").expect("detach other");
        assert!(db.tags_for_task(id).expect("list tags").is_empty());
        assert_eq!(db.tasks_for_day(day).expect("day list").len(), 2);
    }

    #[test]
    fn tracked_seconds_accumulate_and_survive_reopen() {
        let path = temp_db_path();
        let id = {
            let db = Database::open(&path).expect("open file database");
            let id = db.save_task(&sample_task()).expect("save task");
            db.add_tracked_secs(id, 90).expect("bank ninety seconds");
            db.add_tracked_secs(id, 30).expect("bank thirty more");
            id
        };
        let db = Database::open(&path).expect("reopen file database");
        let task = db.task_by_id(id).expect("load").expect("exists");
        assert_eq!(task.tracked_secs, 120, "totals survive restarts");
        db.add_tracked_secs(id, -200).expect("subtract clamps");
        assert_eq!(
            db.task_by_id(id)
                .expect("load")
                .expect("exists")
                .tracked_secs,
            0,
            "totals never go negative"
        );
        drop(db);

        std::fs::remove_file(&path).expect("remove temp db");
    }

    #[test]
    fn resolve_db_path_prefers_explicit_path() {
        let explicit = PathBuf::from("E:\\custom\\tasks.db");
        assert_eq!(resolve_db_path(Some(explicit.clone())), explicit);
    }

    #[test]
    fn resolve_db_path_reads_env_var() {
        let path = resolve_db_path_with_env(None, &|key| {
            assert_eq!(key, ENV_DB_PATH);
            Some("E:\\from-env\\tasks.db".to_string())
        });
        assert_eq!(path, PathBuf::from("E:\\from-env\\tasks.db"));
    }

    #[test]
    fn resolve_db_path_ignores_blank_env_var() {
        let path = resolve_db_path_with_env(None, &|_| Some("   ".to_string()));
        assert_eq!(
            path.parent()
                .expect("resolved path has a parent")
                .file_name()
                .expect("parent has a name"),
            "tasker"
        );
    }

    #[test]
    fn resolve_db_path_defaults_under_tasker_dir() {
        let path = resolve_db_path(None);
        assert_eq!(
            path.file_name().expect("resolved path has a file name"),
            "tasks.db"
        );
        assert_eq!(
            path.parent()
                .expect("resolved path has a parent")
                .file_name()
                .expect("parent has a name"),
            "tasker"
        );
    }
}
