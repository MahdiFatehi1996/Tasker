<div align="center">

# Tasker

**A single-binary, offline, keyboard-driven daily task manager for the terminal — with an amber-phosphor retro theme and a scriptable CLI.**

[![Verify & Release](https://github.com/MahdiFatehi1996/Tasker/actions/workflows/release.yml/badge.svg)](https://github.com/MahdiFatehi1996/Tasker/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust: stable](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)
[![Edition: 2024](https://img.shields.io/badge/edition-2024-blueviolet.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![Platforms: Windows | Linux | macOS](https://img.shields.io/badge/platforms-Windows%20%7C%20Linux%20%7C%20macOS-blue.svg)](#platform-notes)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/MahdiFatehi1996/Tasker/issues)

[Features](#-features) · [Install](#-install) · [Usage](#-usage) · [CLI](#-command-line) · [Data](#-data-and-files) · [Contributing](#-contributing)

</div>

---

Each calendar day owns its own task list. Unfinished work never disappears: open tasks from
past days automatically resurface in today's view as **Overdue**, still pinned to their original
day. Deadlines get their own **Due today** and **Missed due** sections, with a desktop toast on
launch for anything already late. Tasks carry **tags** you can filter by, and each task has a
live **timer** that banks billable minutes across restarts.

A month-grid **Calendar** jumps between days, and a built-in detail editor manages titles,
priorities, due moments, tags, and notes — no accounts, no network, no server. Everything lives
in one local SQLite file, and the same storage backs a non-interactive CLI for fast capture and
export. Written in Rust, with `unsafe` code forbidden at the crate level.

## 🖼️ Preview

```text
┌─ Calendar ───────────┐ ┌─ Missed Due ───────────────────────────────────────────────────────┐
│                      │ │  [ ] [due 2026-10-09 17:00 #work] Ship the report             0:45 │
│     October 2026     │ └────────────────────────────────────────────────────────────────────┘
│ Mo Tu We Th Fr Sa Su │ ┌─ Due Today ────────────────────────────────────────────────────────┐
│           1  2  3  4 │ │> [ ] [due 2026-10-13 09:00] Stand-up notes                    0:12 │
│  5  6  7  8  9 10 11 │ └────────────────────────────────────────────────────────────────────┘
│ 12 13 14 15 16 17 18 │ ┌─ Overdue ──────────────────────────────────────────────────────────┐
│ 19 20 21 22 23 24 25 │ │  [ ] [2026-10-11 #errands] Renew the domain                   0:00 │
│  26 27 28 29 30 31   │ └────────────────────────────────────────────────────────────────────┘
│                      │ ┌─ Tasker — Tue Oct 13, 2026 ────────────────────────────────────────┐
└──────────────────────┘ │> [ ] [H] Draft the Q4 roadmap                               ▶ 1:20 │
                         │  [x] [M] Reply to Amir #work                                  0:05 │
                         │  [ ] [L] Water the plants #home                               0:00 │
                         └────────────────────────────────────────────────────────────────────┘

                           2/5 done
                           j/k move · x toggle · a add · e edit · w timer · f filter · [ ] day · t today · Tab cal · ? help · q quit
```

## ✨ Features

- **Day lists** — one task list per calendar date; browse history and plan ahead.
- **Automatic carry-over** — overdue tasks surface in today's view flagged with their original day; completing one clears it.
- **Due moments** — set an exact due date-time; **Due today** and **Missed due** sections surface hard commitments, and a launch toast flags anything already past due.
- **Tags & filter** — attach multiple tags per task and cycle a one-tag filter to focus the whole list.
- **Live timer** — start/stop one task at a time with `w`; totals (`H:MM`) show on rows and in the editor, and survive restarts.
- **Calendar pane** — month grid with a keyboard cursor for jumping to any day.
- **Detail editor** — title, priority (low/medium/high), due moment, tags, and free-text multiline notes, all in-app.
- **CLI companion** — `add` / `done` / `list` / `export` share the storage layer for capture without the TUI.
- **Export** — one-way JSON (full fidelity) and readable markdown; no data lock-in.
- **Amber-phosphor theme** — a single palette applied across every widget.
- **Local-first storage** — embedded SQLite, no setup beyond launching.

## 📦 Install

### Prerequisites

Any stable Rust toolchain with a C linker (used to build bundled SQLite):

- Via [rustup](https://rustup.rs), **or**
- Via MSYS2 UCRT64 on Windows: `pacman -S mingw-w64-ucrt-x86_64-rust`

Make sure `cargo` (and the toolchain's `bin` directory) is on your `PATH`.

### Build from source

```sh
git clone https://github.com/MahdiFatehi1996/Tasker.git
cd Tasker
cargo build --release
```

The binary lands at `target/release/tasker` (`tasker.exe` on Windows).

### Run

```sh
cargo run --release
# or
./target/release/tasker
```

First launch creates the database automatically. The default location is the platform data
directory (`%APPDATA%\tasker\tasks.db` on Windows). Override it per-run or per-environment:

```sh
tasker --db "D:\my-tasks.db"
set TASKER_DB=D:\my-tasks.db & tasker
```

### Prebuilt binaries

Tagged releases publish prebuilt binaries for Windows, Linux, and macOS on the
[Releases](https://github.com/MahdiFatehi1996/Tasker/releases) page.

### Platform notes

- **Windows** — MSVC toolchain or the MSYS2 UCRT64 toolchain both work.
- **Linux** — install a C compiler first (`sudo apt install build-essential` on Debian/Ubuntu).
  The database resolves to `~/.local/share/tasker/tasks.db`.
- **macOS** — builds with the system toolchain; the database resolves to
  `~/Library/Application Support/tasker/tasks.db`.
- **Terminal** — needs UTF-8 for the box-drawing and `▌` / `↑` glyphs; any modern terminal
  emulator qualifies.

## 🚀 Usage

### Your first session

1. **Launch** — `cargo run --release`. You see today's (empty) list, the October-style month
   calendar on the left, and the key hints at the bottom.
2. **Add a task** — press `a`, type `Buy milk`, press `Enter`. The task appears in today's list.
3. **Complete it** — with the task selected, press `x`. It stays in the list, struck through.
   Press `x` again to reopen it.
4. **Quit** — press `q`. Your tasks are already saved; relaunch to see them still there.

### Managing the day

- **Move** with `j`/`k` or `↑`/`↓`.
- **Toggle** any task with `x` or `Space`.
- **Start/stop a timer** with `w`. Only one timer runs at a time — starting on another task banks
  the first one's elapsed seconds first, and quitting banks the running segment. Totals show as
  `H:MM` on the row and in the editor.
- **Edit** the selected task with `e` (or `Enter`). A panel opens:
  - `Tab` / `Shift+Tab` switches between Title, Priority, Notes, Due, and Tags.
  - Type to edit; `Backspace` deletes.
  - `←`/`→` or `Space` cycles priority (low → medium → high → low).
  - **Due** takes `YYYY-MM-DD HH:MM` (also `YYYY-MM-DDTHH:MM` or date-only for midnight); a blank
    clears it, and an invalid value keeps the panel open to fix.
  - **Tags** are comma-separated (`work, admin`); blank clears them.
  - `Enter` saves — except in Notes, where it inserts a newline.
  - `Ctrl+S` saves from any field; `Esc` discards and closes.
- Clearing a title to blank discards the edit instead of saving an empty task.

### Tags, deadlines, and time

- **Filter by tag** — press `f` to cycle through the tags in use; every section narrows to the
  active tag. Press `Esc` to clear. A filter pointing at a vanished tag clears itself.
- **Deadlines** — a task with a due moment today appears under **Due today**; one already past
  under **Missed due**, flagged with its due moment. Completing it clears both. Launching the app
  fires a desktop toast for anything already past due.
- **Timers** persist: accumulated seconds are stored, so closing and reopening never loses
  tracked time.

### Navigating days

- **`[` / `]`** — previous / next day. The list reloads for the viewed day.
- **`t`** — snap back to today.
- **Calendar jumping** — press `Tab` to focus the calendar (bright amber border), move the cursor
  with arrows or `hjkl` (`↑`/`↓` step a week), press `Enter` to jump the viewed day to the cursor,
  `Esc` to return focus to the list.
- **Add to any day** — navigate somewhere else and press `a`; the new task is pinned to the viewed
  day.

### Overdue carry-over

Leave a task open overnight. Tomorrow it appears in an **Overdue** section above today's list,
flagged with its original day (e.g. `[2026-10-08]`). It stays pinned there — completing it removes
it from Overdue. Done tasks and future tasks never surface as overdue.

### Help

Press **`?`** anywhere in the main view for the full keybinding table. Press `?` or `Esc` to close
it. While help is open, all other keys are frozen so nothing happens behind it by accident.

## ⌨️ Command line

A bare `tasker` launches the TUI; any subcommand runs non-interactively against the same database
(same `--db` / `TASKER_DB` resolution) without opening the interface:

```sh
tasker add "Buy milk"                                # add a task to today
tasker done 3                                        # mark task 3 done
tasker list                                          # print today's view (sections + rows)
tasker export --format json                          # full-fidelity dump: every field
tasker export --format markdown --day 2026-10-08     # readable log of one day
```

`add` / `done` print a one-line confirmation; `list` mirrors the TUI's Missed-due / Due-today /
Overdue / Today sections; `export` writes to stdout so you can pipe it anywhere. Argument errors
print usage and exit non-zero.

## 🗂️ Data and files

| What        | Where                                                                              |
| ----------- | ---------------------------------------------------------------------------------- |
| Database    | `%APPDATA%\tasker\tasks.db` (override: `--db`, `TASKER_DB`)                        |
| Single task | id, day (`YYYY-MM-DD`), title, done, priority, notes, due, tracked seconds, tags   |
| Schema      | Created on first launch; a version marker drives migrations (v1 → v2 adds due, tags, tracked seconds) |

Back up by copying the `.db` file.

## 🧱 Project layout

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

## 🤝 Contributing

Contributions are welcome! Before opening a pull request, please run the same checks as CI:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Bugs and feature requests are tracked in
[GitHub Issues](https://github.com/MahdiFatehi1996/Tasker/issues).

## 📄 License

Released under the [MIT License](LICENSE).
