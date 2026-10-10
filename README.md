# Tasker

A single-binary, offline, keyboard-driven daily task manager for the terminal, written in Rust.

Each calendar day owns its own task list. Unfinished work never disappears: open tasks from past days automatically resurface in today's view as **Overdue**, still pinned to their original day. A month-grid **Calendar** jumps between days, and a built-in detail editor manages titles, priorities, and notes — no accounts, no network, no server. Everything lives in one local SQLite file.

## Features

- **Day lists** — one task list per calendar date; browse history and plan ahead
- **Automatic carry-over** — overdue tasks surface in today's view flagged with their original day; completing one clears it
- **Calendar pane** — month grid with keyboard cursor for jumping to any day
- **Detail editor** — title, priority (low/medium/high), and free-text multiline notes, all in-app
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

## Step-by-step usage

### Your first session

1. **Launch** — `cargo run --release`. You see today's (empty) list, the October-style month calendar on the left, and the key hints at the bottom.
2. **Add a task** — press `a`, type `Buy milk`, press `Enter`. The task appears in today's list.
3. **Complete it** — with the task selected, press `x`. It stays in the list, struck through. Press `x` again to reopen it.
4. **Quit** — press `q`. Your tasks are already saved; relaunch to see them still there.

### Managing the day

- **Move** with `j`/`k` or `↑`/`↓`.
- **Toggle** any task with `x` or `Space`.
- **Edit** the selected task with `e` (or `Enter`). A panel opens:
  - `Tab` / `Shift+Tab` switches between Title, Priority, and Notes.
  - Type to edit; `Backspace` deletes.
  - `←`/`→` or `Space` cycles priority (low → medium → high → low).
  - `Enter` saves — except in Notes, where it inserts a newline.
  - `Ctrl+S` saves from any field; `Esc` discards and closes.
- Clearing a title to blank discards the edit instead of saving an empty task.

### Navigating days

- **`[` / `]`** — previous / next day. The list reloads for the viewed day.
- **`t`** — snap back to today.
- **Calendar jumping** — press `Tab` to focus the calendar (yellow border), move the cursor with arrows or `hjkl` (`↑`/`↓` step a week), press `Enter` to jump the viewed day to the cursor, `Esc` to return focus to the list.
- **Add to any day** — navigate somewhere else and press `a`; the new task is pinned to the viewed day.

### Overdue carry-over

Leave a task open overnight. Tomorrow it appears in an **Overdue** section above today's list, flagged with its original day (e.g. `[2026-10-08]`). It stays pinned there — completing it removes it from Overdue. Done tasks and future tasks never surface as overdue.

### Help

Press **`?`** anywhere in the main view for the full keybinding table. Press `?` or `Esc` to close it. While help is open, all other keys are frozen so nothing happens behind it by accident.

## Data & files

| What         | Where                                                      |
| ------------ | ---------------------------------------------------------- |
| Database     | `%APPDATA%\tasker\tasks.db` (override: `--db`, `TASKER_DB`) |
| Single task  | id, day (`YYYY-MM-DD`), title, done, priority, notes        |
| Schema       | Created on first launch; version marker reserved for migrations |

Back up by copying the `.db` file.

## Project layout

```text
src/
  main.rs    CLI args, startup, wiring
  app.rs     Pure state machine (messages → state + effects), unit-tested
  db.rs      SQLite storage seam (day queries, overdue, save/update)
  ui.rs      Ratatui render + event loop + keymap
CONTEXT.md       Domain glossary
docs/adr/        Architecture decisions (SQLite, ratatui TUI)
docs/agents/     Agent-skill configuration (issue tracker, labels, domain)
```

## Developing

```sh
cargo fmt        # format
cargo clippy --all-targets -- -D warnings   # lint gate
cargo test       # full suite (storage + state-transition seams)
```

Work is tracked in [GitHub Issues](https://github.com/MahdiFatehi1996/Tasker/issues). See `AGENTS.md` for the agent workflow.
