//! Tasker: a TUI daily task manager. See issue #1 for the spec.

mod db;
mod model;
mod ui;

use std::path::PathBuf;

use db::{Database, resolve_db_path};

/// Parse an optional `--db <path>` / `--db=<path>` argument.
fn parse_db_arg(args: &[String]) -> Option<PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if let Some(path) = arg.strip_prefix("--db=") {
            return Some(PathBuf::from(path));
        }
        if arg == "--db"
            && let Some(path) = iter.next()
        {
            return Some(PathBuf::from(path));
        }
    }
    None
}

fn main() -> Result<(), db::Error> {
    let args: Vec<String> = std::env::args().collect();
    let path = resolve_db_path(parse_db_arg(&args));
    let database = Database::open(&path)?;
    ui::run(&database, &path)
}
