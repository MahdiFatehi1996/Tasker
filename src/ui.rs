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
use crate::model::Priority;

fn priority_letter(priority: Priority) -> char {
    match priority {
        Priority::Low => 'L',
        Priority::Medium => 'M',
        Priority::High => 'H',
    }
}

/// Draw one frame of the day-list view.
fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(frame.area());

    let items: Vec<ListItem> = app
        .tasks()
        .iter()
        .enumerate()
        .map(|(i, task)| {
            let marker = if i == app.selected() { "> " } else { "  " };
            let checkbox = if task.done { "[x]" } else { "[ ]" };
            let row_style = if task.done {
                Style::default().add_modifier(Modifier::CROSSED_OUT)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, row_style),
                Span::styled(format!("{} ", checkbox), row_style),
                Span::styled(
                    format!("[{}] {}", priority_letter(task.priority), task.title),
                    row_style,
                ),
            ]))
        })
        .collect();

    let title = format!("Tasker — {}", app.viewed().format("%a %b %d, %Y"));
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    let mut state = ListState::default();
    state.select(Some(app.selected()));
    frame.render_stateful_widget(list, chunks[0], &mut state);

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
            let done = app.tasks().iter().filter(|task| task.done).count();
            (
                Line::raw(format!("{}/{} done", done, app.tasks().len())),
                Line::raw("j/k move · x toggle · a add · q quit"),
            )
        }
    };
    frame.render_widget(Paragraph::new(vec![status, help]), chunks[1]);
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
        }
        if app.should_quit() {
            break;
        }
    }
    Ok(())
}
