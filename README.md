# Tasker

A single-binary, offline, keyboard-driven daily task manager for the terminal, written in Rust — with an amber-phosphor retro theme and a scriptable CLI.

Each calendar day owns its own task list. Unfinished work never disappears: open tasks from past days automatically resurface in today's view as **Overdue**, still pinned to their original day. Deadlines get their own **Due today** and **Missed due** sections, with a desktop toast on launch for anything already late. Tasks carry **tags** you can filter by, and each task has a live **timer** that banks billable minutes across restarts. A month-grid **Calendar** jumps between days, and a built-in detail editor manages titles, priorities, due moments, tags, and notes — no accounts, no network, no server. Everything lives in one local SQLite file, and the same storage backs a non-interactive CLI for fast capture and export.

## Features

- **Day lists** — one task list per calendar date; browse history and plan ahead
- **Automatic carry-over** — overdue tasks surface in today's view flagged with their original day; completing one clears it
- **Due moments** — set an exact due date-time; **Due today** and **Missed due** sections surface hard commitments, and a launch toast flags anything already past due
- **Tags & filter** — attach multiple tags per task and cycle a one-tag filter to focus the whole list
- **Live timer** — start/stop one task at a time with `w`; totals (`H:MM`) show on rows and in the editor and survive restarts
- **Calendar pane** — month grid with keyboard cursor for jumping to any day
- **Detail editor** — title, priority (low/medium/high), due moment, tags, and free-text multiline notes, all in-app
- **CLI companion** — `add` / `done` / `list` / `export` share the storage layer for capture without the TUI
- **Export** — one-way JSON (full fidelity) and readable markdown, no data lock-in
- **Amber-phosphor theme** — a single palette applied across every widget
- **Local-first storage** — embedded SQLite, no setup beyond launching

## Install

### 1. Install a Rust toolchain

Any stable Rust with a C linker works:

- Via rustup (https://rustup.rs), **or**
- Via MSYS2 UCRT64 on Windows: `pacman -S mingw-w64-ucrt-x86_64-rust`

Make sure `cargo` (and the toolchain's `bin` dir) is on your `PATH`.

### 2. Build

```sh
git clone https://github.com/MahdiFatehi1996/Tasker.git
cd Tasker
cargo build --release
```

The binary lands at `target/release/tasker`.

### 3. Run

```sh
cargo run --release
# or
./target/release/tasker
```

First launch creates the database automatically. Default location is the platform data dir (`%APPDATA%\tasker\tasks.db` on Windows). Override it per-run or per-environment:

```sh
tasker --db "D:\my-tasks.db"
set TASKER_DB=D:\my-tasks.db & tasker
```

### Running on Linux

The codebase is fully portable — no Windows-specific code or dependencies. On a Linux machine:

```sh
# 1. Toolchain + C compiler (needed to build bundled SQLite)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sudo apt install build-essential   # Debian/Ubuntu; or equivalent

# 2. Build and run (crates.io is used directly; no mirror config needed)
git clone https://github.com/MahdiFatehi1996/Tasker.git
cd Tasker
cargo build --release
./target/release/tasker
```

Differences from Windows:

- **DB location** resolves to `~/.local/share/tasker/tasks.db` (same `--db` / `TASKER_DB` overrides).
- **Terminal** needs UTF-8 for the box-drawing and `▌`/`↑` glyphs — any modern terminal emulator qualifies.

## Step-by-step usage

### Your first session

1. **Launch** — `cargo run --release`. You see today's (empty) list, the October-style month calendar on the left, and the key hints at the bottom.
2. **Add a task** — press `a`, type `Buy milk`, press `Enter`. The task appears in today's list.
3. **Complete it** — with the task selected, press `x`. It stays in the list, struck through. Press `x` again to reopen it.
4. **Quit** — press `q`. Your tasks are already saved; relaunch to see them still there.

### Managing the day

- **Move** with `j`/`k` or `↑`/`↓`.
- **Toggle** any task with `x` or `Space`.
- **Start/stop a timer** with `w`. Only one timer runs at a time — starting on another task banks the first one's elapsed seconds first, and quitting banks the running segment. Totals show as `H:MM` on the row and in the editor.
- **Edit** the selected task with `e` (or `Enter`). A panel opens:
  - `Tab` / `Shift+Tab` switches between Title, Priority, Notes, Due, and Tags.
  - Type to edit; `Backspace` deletes.
  - `←`/`→` or `Space` cycles priority (low → medium → high → low).
  - **Due** takes `YYYY-MM-DD HH:MM` (also `YYYY-MM-DDTHH:MM` or date-only for midnight); a blank clears it, and an invalid value keeps the panel open to fix.
  - **Tags** are comma-separated (`work, admin`); blank clears them.
  - `Enter` saves — except in Notes, where it inserts a newline.
  - `Ctrl+S` saves from any field; `Esc` discards and closes.
- Clearing a title to blank discards the edit instead of saving an empty task.

### Tags, deadlines, and time

- **Filter by tag** — press `f` to cycle through the tags in use; every section narrows to the active tag. Press `Esc` to clear. A filter pointing at a vanished tag clears itself.
- **Deadlines** — a task with a due moment today appears under **Due today**; one already past under **Missed due**, flagged with its due moment. Completing it clears both. Launching the app fires a desktop toast for anything already past due.
- **Timers** persist: accumulated seconds are stored, so closing and reopening never loses tracked time.

### Navigating days

- **`[` / `]`** — previous / next day. The list reloads for the viewed day.
- **`t`** — snap back to today.
- **Calendar jumping** — press `Tab` to focus the calendar (bright amber border), move the cursor with arrows or `hjkl` (`↑`/`↓` step a week), press `Enter` to jump the viewed day to the cursor, `Esc` to return focus to the list.
- **Add to any day** — navigate somewhere else and press `a`; the new task is pinned to the viewed day.

### Overdue carry-over

Leave a task open overnight. Tomorrow it appears in an **Overdue** section above today's list, flagged with its original day (e.g. `[2026-10-08]`). It stays pinned there — completing it removes it from Overdue. Done tasks and future tasks never surface as overdue.

### Help

Press **`?`** anywhere in the main view for the full keybinding table. Press `?` or `Esc` to close it. While help is open, all other keys are frozen so nothing happens behind it by accident.

## Command line

A bare `tasker` launches the TUI; any subcommand runs non-interactively against the same database (same `--db` / `TASKER_DB` resolution) without opening the interface:

```sh
tasker add "Buy milk"                       # add a task to today
tasker done 3                               # mark task 3 done
tasker list                                 # print today's view (sections + rows)
tasker export --format json                 # full-fidelity dump: every field, incl. tags, due, tracked seconds
tasker export --format markdown --day 2026-10-08   # readable log of one day
```

`add`/`done` print a one-line confirmation; `list` mirrors the TUI's Missed-due / Due-today / Overdue / Today sections; `export` writes to stdout so you can pipe it anywhere. Argument errors print usage and exit non-zero.

## Data & files

| What         | Where                                                      |
| ------------ | ---------------------------------------------------------- |
| Database     | `%APPDATA%\tasker\tasks.db` (override: `--db`, `TASKER_DB`) |
| Single task  | id, day (`YYYY-MM-DD`), title, done, priority, notes, due, tracked seconds, tags |
| Schema       | Created on first launch; version marker drives migrations (v1 → v2 adds due, tags, tracked seconds) |

Back up by copying the `.db` file.

## Project layout

```text
src/
  main.rs    CLI args, startup, dispatch (TUI vs subcommands)
  cli.rs     Non-interactive surface: parse args → command; execute via storage
  app.rs     Pure state machine (messages → state + effects), unit-tested
  db.rs      SQLite storage seam (day queries, overdue, due, tags, timer)
  model.rs   Domain types + formatting (tags, due input, H:MM totals)
  theme.rs   Single amber-phosphor palette
  ui.rs      Ratatui render + event loop + keymap
```

Work is tracked in [GitHub Issues](https://github.com/MahdiFatehi1996/Tasker/issues).
