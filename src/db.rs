//! Storage seam: the only module that touches SQLite.
//!
//! All task persistence goes through [`Database`]; the UI never opens a
//! connection directly. [`resolve_db_path`] decides where the database file
//! lives: explicit flag, then `TASKER_DB`, then the platform data dir.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use rusqlite::Connection;

use crate::model::{NewTask, Priority, Task};

/// Name of the environment variable overriding the database location.
pub const ENV_DB_PATH: &str = "TASKER_DB";

/// Current schema version, recorded in the database for future migrations.
pub const SCHEMA_VERSION: i64 = 1;

/// Date format used for the `day` column (`YYYY-MM-DD`, lexicographically sortable).
const DAY_FORMAT: &str = "%Y-%m-%d";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    day TEXT NOT NULL,
    title TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0,
    priority TEXT NOT NULL DEFAULT 'medium',
    notes TEXT NOT NULL DEFAULT ''
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

fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn parse_day(s: &str) -> rusqlite::Result<NaiveDate> {
    NaiveDate::parse_from_str(s, DAY_FORMAT).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
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
    Ok(Task {
        id: row.get(0)?,
        day: parse_day(&day_text)?,
        title: row.get(2)?,
        done: row.get::<_, i64>(3)? != 0,
        priority: parse_priority(&priority_text)?,
        notes: row.get(5)?,
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

impl Database {
    /// Open (creating if needed) the database at `path`, creating parent
    /// directories and initializing the schema idempotently.
    pub fn open(path: &Path) -> Result<Database, Error> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        init_schema(&conn)?;
        Ok(Database { conn })
    }

    /// Open a private throwaway in-memory database (used by tests).
    /// Consumed by later tickets' flows; tests are the first caller.
    #[allow(dead_code)]
    pub fn open_in_memory() -> Result<Database, Error> {
        let conn = Connection::open_in_memory()?;
        init_schema(&conn)?;
        Ok(Database { conn })
    }

    /// Store a new task; returns its assigned id. New tasks are never done.
    /// First wired into user flows by ticket #3; tests are the first caller.
    #[allow(dead_code)]
    pub fn save_task(&self, task: &NewTask) -> Result<i64, Error> {
        self.conn.execute(
            "INSERT INTO tasks (day, title, done, priority, notes) VALUES (?1, ?2, 0, ?3, ?4)",
            (
                task.day.format(DAY_FORMAT).to_string(),
                &task.title,
                task.priority.as_str(),
                &task.notes,
            ),
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Persist changes to an existing task's title, state, priority, notes.
    /// The task keeps its original day and id.
    /// First wired into user flows by ticket #6; tests are the first caller.
    #[allow(dead_code)]
    pub fn update_task(&self, task: &Task) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE tasks SET title = ?1, done = ?2, priority = ?3, notes = ?4 WHERE id = ?5",
            (
                &task.title,
                task.done as i64,
                task.priority.as_str(),
                &task.notes,
                task.id,
            ),
        )?;
        Ok(())
    }

    /// All tasks pinned to `day`, in insertion order.
    pub fn tasks_for_day(&self, day: NaiveDate) -> Result<Vec<Task>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, day, title, done, priority, notes FROM tasks WHERE day = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([day.format(DAY_FORMAT).to_string()], |row| {
            task_from_row(row)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Error::Sql)
    }

    /// Open tasks from days before `today`, oldest first: the Overdue
    /// section. Done tasks and today/future tasks are never overdue.
    /// Day pins never change — overdue is derived, not re-dated.
    pub fn overdue_tasks(&self, today: NaiveDate) -> Result<Vec<Task>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, day, title, done, priority, notes FROM tasks WHERE day < ?1 AND done = 0 ORDER BY day, id",
        )?;
        let rows = stmt.query_map([today.format(DAY_FORMAT).to_string()], |row| {
            task_from_row(row)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Error::Sql)
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
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn test_day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date")
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
            }
        );
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
            })
            .expect("save past open task");
        let done_id = db
            .save_task(&NewTask {
                day: past,
                title: "Finished past work".to_string(),
                priority: Priority::Medium,
                notes: String::new(),
            })
            .expect("save past task to complete");
        db.update_task(&Task {
            id: done_id,
            day: past,
            title: "Finished past work".to_string(),
            done: true,
            priority: Priority::Medium,
            notes: String::new(),
        })
        .expect("complete past task");
        db.save_task(&NewTask {
            day: today,
            title: "Today work".to_string(),
            priority: Priority::Medium,
            notes: String::new(),
        })
        .expect("save today task");
        db.save_task(&NewTask {
            day: future,
            title: "Future work".to_string(),
            priority: Priority::Medium,
            notes: String::new(),
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
        })
        .expect("update task");

        let tasks = db.tasks_for_day(day).expect("query day");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Write the annual report");
        assert!(tasks[0].done);
        assert_eq!(tasks[0].priority, Priority::Low);
        assert_eq!(tasks[0].notes, "Updated notes.");
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
