//! UI layer: TUI render + event loop.
//!
//! Rendering observes [`App`] read-only. Keys become [`Message`]s; the
//! returned [`Effect`]s are performed against the [`Database`](crate::db::Database)
//! and the day list reloads. Rendering itself is verified manually.

use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::app::{App, Effect, Message, Mode};
use crate::db::Database;
use crate::model::{Priority, Task};

fn priority_letter(priority: Priority) -> char {
    match priority {
        Priority::Low => 'L',
        Priority::Medium => 'M',
        Priority::High => 'H',
    }
}

/// One task row. `selected` drives the cursor marker; `original_day` (the
/// pin of an overdue task) renders after the checkbox when present.
fn task_row(task: &Task, selected: bool, original_day: Option<String>) -> ListItem<'_> {
    let marker = if selected { "> " } else { "  " };
    let checkbox = if task.done { "[x]" } else { "[ ]" };
    let row_style = if task.done {
        Style::default().add_modifier(Modifier::CROSSED_OUT)
    } else {
        Style::default()
    };
    let mut spans = vec![
        Span::styled(marker, row_style),
        Span::styled(format!("{} ", checkbox), row_style),
    ];
    if let Some(flag) = original_day {
        spans.push(Span::styled(format!("[{}] ", flag), row_style));
    }
    spans.push(Span::styled(
        format!("[{}] {}", priority_letter(task.priority), task.title),
        row_style,
    ));
    ListItem::new(Line::from(spans))
}

/// Draw one frame of the day-list view: the Overdue section (when non-empty)
/// above today's list, then the status footer.
fn render(frame: &mut Frame, app: &App) {
    let mut constraints = Vec::new();
    if !app.overdue().is_empty() {
        constraints.push(Constraint::Length(app.overdue().len() as u16 + 2));
    }
    constraints.push(Constraint::Min(1));
    constraints.push(Constraint::Length(2));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    let mut section = 0;
    if !app.overdue().is_empty() {
        let cursor = app.overdue_cursor();
        let items: Vec<ListItem> = app
            .overdue()
            .iter()
            .enumerate()
            .map(|(i, task)| {
                task_row(
                    task,
                    Some(i) == cursor,
                    Some(task.day.format("%Y-%m-%d").to_string()),
                )
            })
            .collect();
        let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Overdue"));
        let mut state = ListState::default();
        state.select(cursor);
        frame.render_stateful_widget(list, chunks[section], &mut state);
        section += 1;
    }

    let cursor = app.today_cursor();
    let items: Vec<ListItem> = app
        .tasks()
        .iter()
        .enumerate()
        .map(|(i, task)| task_row(task, Some(i) == cursor, None))
        .collect();

    let title = format!("Tasker — {}", app.viewed().format("%a %b %d, %Y"));
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    let mut state = ListState::default();
    state.select(cursor);
    frame.render_stateful_widget(list, chunks[section], &mut state);
    section += 1;

    let (status, help) = match app.mode() {
        Mode::Adding => (
            Line::from(vec![
                Span::raw("Add: "),
                Span::styled(
                    app.draft(),
                    Style::default().add_modifier(Modifier::UNDERLINED),
                ),
                Span::raw("▌"),
            ]),
            Line::raw("Enter commit · Esc cancel"),
        ),
        Mode::Browse => {
            let visible: Vec<&Task> = app.overdue().iter().chain(app.tasks()).collect();
            let done = visible.iter().filter(|task| task.done).count();
            (
                Line::raw(format!("{}/{} done", done, visible.len())),
                Line::raw("j/k move · x toggle · a add · q quit"),
            )
        }
    };
    frame.render_widget(Paragraph::new(vec![status, help]), chunks[section]);
}

/// Block until a key maps to a message for the current mode.
fn read_message(mode: Mode) -> std::io::Result<Message> {
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let message = match mode {
            Mode::Browse => match key.code {
                KeyCode::Char('q') => Message::Quit,
                KeyCode::Char('j') | KeyCode::Down => Message::SelectNext,
                KeyCode::Char('k') | KeyCode::Up => Message::SelectPrev,
                KeyCode::Char('x') | KeyCode::Char(' ') => Message::ToggleSelected,
                KeyCode::Char('a') => Message::BeginAdd,
                _ => continue,
            },
            Mode::Adding => match key.code {
                KeyCode::Esc => Message::CancelAdd,
                KeyCode::Enter => Message::CommitAdd,
                KeyCode::Backspace => Message::AddBackspace,
                KeyCode::Char(c) => Message::AddChar(c),
                _ => continue,
            },
        };
        return Ok(message);
    }
}

/// Run the day-list TUI until quit. The terminal is restored on every
/// exit path, including persistence errors mid-loop.
pub fn run(db: &Database, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, db, app);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    db: &Database,
    app: &mut App,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        terminal.draw(|frame| render(frame, app))?;
        let effects = app.update(read_message(app.mode())?);
        if !effects.is_empty() {
            for effect in effects {
                match effect {
                    Effect::SaveNew(task) => {
                        db.save_task(&task)?;
                    }
                    Effect::Update(task) => {
                        db.update_task(&task)?;
                    }
                }
            }
            app.set_tasks(db.tasks_for_day(app.viewed())?);
            // Overdue is anchored to real today, never to the viewed day:
            // browsing a past day must not hide it, and a future day must
            // not promote today's tasks into it.
            let today = chrono::Local::now().date_naive();
            app.set_overdue(db.overdue_tasks(today)?);
        }
        if app.should_quit() {
            break;
        }
    }
    Ok(())
}
