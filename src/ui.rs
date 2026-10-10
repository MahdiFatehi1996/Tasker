//! UI layer: TUI render + event loop + keymap.
//!
//! Rendering observes [`App`] read-only. Keys become [`Message`]s via the pure
//! [`map_key`]; returned [`Effect`]s are performed against the
//! [`Database`](crate::db::Database) and the viewed day reloads. Widget
//! output is verified manually (headless frame capture); only the keymap
//! has unit tests.

use chrono::{Datelike, Local, NaiveDate};
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph,
        calendar::{CalendarEventStore, Monthly},
    },
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

fn to_time_date(day: NaiveDate) -> time::Date {
    time::Date::from_calendar_date(
        day.year(),
        time::Month::try_from(day.month() as u8).expect("month 1-12"),
        day.day() as u8,
    )
    .expect("chrono dates are valid calendar dates")
}

/// Month-grid calendar for the focused-or-viewed month. The viewed day is
/// reversed, the cursor (when focused) underlined, real today bold.
fn month_calendar(app: &App, today: NaiveDate) -> Monthly<'_, CalendarEventStore> {
    let viewed = app.viewed();
    let cursor = app.cursor();
    let focused = app.calendar_focused();

    let mut store = CalendarEventStore::default();
    store.add(
        to_time_date(today),
        Style::default().add_modifier(Modifier::BOLD),
    );

    let mut viewed_style = Style::default().add_modifier(Modifier::REVERSED);
    if viewed == today {
        viewed_style = viewed_style.add_modifier(Modifier::BOLD);
    }
    store.add(to_time_date(viewed), viewed_style);

    if focused {
        let mut cursor_style = Style::default().add_modifier(Modifier::UNDERLINED);
        if cursor == viewed {
            cursor_style = cursor_style.add_modifier(Modifier::REVERSED);
        }
        if cursor == today {
            cursor_style = cursor_style.add_modifier(Modifier::BOLD);
        }
        store.add(to_time_date(cursor), cursor_style);
    }

    let display = if focused { cursor } else { viewed };
    Monthly::new(to_time_date(display), store)
        .show_weekdays_header(Style::default().add_modifier(Modifier::DIM))
        .show_month_header(Style::default().add_modifier(Modifier::BOLD))
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

/// Draw one frame: calendar pane left, day-list column right
/// (Overdue section when non-empty, viewed day, status footer).
fn render(frame: &mut Frame, app: &App, today: NaiveDate) {
    let calendar = month_calendar(app, today);
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(calendar.width() + 2), Constraint::Min(1)])
        .split(frame.area());

    let border_style = if app.calendar_focused() {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    frame.render_widget(
        calendar.block(
            Block::default()
                .borders(Borders::ALL)
                .title("Calendar")
                .border_style(border_style),
        ),
        outer[0],
    );

    let mut constraints = Vec::new();
    if !app.overdue().is_empty() {
        constraints.push(Constraint::Length(app.overdue().len() as u16 + 2));
    }
    constraints.push(Constraint::Min(1));
    constraints.push(Constraint::Length(2));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(outer[1]);

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
                Line::raw("j/k move · x toggle · a add · [ ] day · t today · Tab cal · q quit"),
            )
        }
    };
    frame.render_widget(Paragraph::new(vec![status, help]), chunks[section]);
}

/// Pure key-to-message mapping; unmapped keys yield None (ignored).
fn map_key(key: KeyEvent, mode: Mode, calendar_focused: bool, today: NaiveDate) -> Option<Message> {
    match mode {
        Mode::Adding => match key.code {
            KeyCode::Esc => Some(Message::CancelAdd),
            KeyCode::Enter => Some(Message::CommitAdd),
            KeyCode::Backspace => Some(Message::AddBackspace),
            KeyCode::Char(c) => Some(Message::AddChar(c)),
            _ => None,
        },
        Mode::Browse => match key.code {
            // Global day keys work regardless of calendar focus.
            KeyCode::Char('[') => Some(Message::ViewPrevDay),
            KeyCode::Char(']') => Some(Message::ViewNextDay),
            KeyCode::Char('t') => Some(Message::ViewToday(today)),
            KeyCode::Char('q') => Some(Message::Quit),
            _ if calendar_focused => match key.code {
                KeyCode::Left | KeyCode::Char('h') => Some(Message::MoveCursor(-1)),
                KeyCode::Right | KeyCode::Char('l') => Some(Message::MoveCursor(1)),
                KeyCode::Up | KeyCode::Char('k') => Some(Message::MoveCursor(-7)),
                KeyCode::Down | KeyCode::Char('j') => Some(Message::MoveCursor(7)),
                KeyCode::Enter | KeyCode::Char(' ') => Some(Message::JumpToCursor),
                KeyCode::Esc | KeyCode::Tab => Some(Message::FocusList),
                _ => None,
            },
            _ => match key.code {
                KeyCode::Char('j') | KeyCode::Down => Some(Message::SelectNext),
                KeyCode::Char('k') | KeyCode::Up => Some(Message::SelectPrev),
                KeyCode::Char('x') | KeyCode::Char(' ') => Some(Message::ToggleSelected),
                KeyCode::Char('a') => Some(Message::BeginAdd),
                KeyCode::Tab => Some(Message::FocusCalendar),
                _ => None,
            },
        },
    }
}

/// Block until a key maps to a message for the current mode and focus.
fn read_message(mode: Mode, calendar_focused: bool, today: NaiveDate) -> std::io::Result<Message> {
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if let Some(message) = map_key(key, mode, calendar_focused, today) {
            return Ok(message);
        }
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
        let today = Local::now().date_naive();
        terminal.draw(|frame| render(frame, app, today))?;
        let effects = app.update(read_message(app.mode(), app.calendar_focused(), today)?);
        if !effects.is_empty() {
            for effect in effects {
                match effect {
                    Effect::SaveNew(task) => {
                        db.save_task(&task)?;
                    }
                    Effect::Update(task) => {
                        db.update_task(&task)?;
                    }
                    Effect::Reload => {}
                }
            }
            app.set_tasks(db.tasks_for_day(app.viewed())?);
            // Overdue is anchored to real today, never to the viewed day:
            // browsing a past day must not hide it, and a future day must
            // not promote today's tasks into it.
            app.set_overdue(db.overdue_tasks(today)?);
        }
        if app.should_quit() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date")
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn browse_list_keys_map() {
        let today = day();
        assert_eq!(
            map_key(press(KeyCode::Char('q')), Mode::Browse, false, today),
            Some(Message::Quit)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('j')), Mode::Browse, false, today),
            Some(Message::SelectNext)
        );
        assert_eq!(
            map_key(press(KeyCode::Down), Mode::Browse, false, today),
            Some(Message::SelectNext)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('k')), Mode::Browse, false, today),
            Some(Message::SelectPrev)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('x')), Mode::Browse, false, today),
            Some(Message::ToggleSelected)
        );
        assert_eq!(
            map_key(press(KeyCode::Char(' ')), Mode::Browse, false, today),
            Some(Message::ToggleSelected)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('a')), Mode::Browse, false, today),
            Some(Message::BeginAdd)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('[')), Mode::Browse, false, today),
            Some(Message::ViewPrevDay)
        );
        assert_eq!(
            map_key(press(KeyCode::Char(']')), Mode::Browse, false, today),
            Some(Message::ViewNextDay)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('t')), Mode::Browse, false, today),
            Some(Message::ViewToday(today))
        );
        assert_eq!(
            map_key(press(KeyCode::Tab), Mode::Browse, false, today),
            Some(Message::FocusCalendar)
        );
        assert_eq!(
            map_key(press(KeyCode::Enter), Mode::Browse, false, today),
            None
        );
    }

    #[test]
    fn browse_calendar_keys_move_cursor() {
        let today = day();
        assert_eq!(
            map_key(press(KeyCode::Left), Mode::Browse, true, today),
            Some(Message::MoveCursor(-1))
        );
        assert_eq!(
            map_key(press(KeyCode::Right), Mode::Browse, true, today),
            Some(Message::MoveCursor(1))
        );
        assert_eq!(
            map_key(press(KeyCode::Up), Mode::Browse, true, today),
            Some(Message::MoveCursor(-7))
        );
        assert_eq!(
            map_key(press(KeyCode::Down), Mode::Browse, true, today),
            Some(Message::MoveCursor(7))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('h')), Mode::Browse, true, today),
            Some(Message::MoveCursor(-1))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('l')), Mode::Browse, true, today),
            Some(Message::MoveCursor(1))
        );
        assert_eq!(
            map_key(press(KeyCode::Enter), Mode::Browse, true, today),
            Some(Message::JumpToCursor)
        );
        assert_eq!(
            map_key(press(KeyCode::Char(' ')), Mode::Browse, true, today),
            Some(Message::JumpToCursor)
        );
        assert_eq!(
            map_key(press(KeyCode::Esc), Mode::Browse, true, today),
            Some(Message::FocusList)
        );
        assert_eq!(
            map_key(press(KeyCode::Tab), Mode::Browse, true, today),
            Some(Message::FocusList)
        );
        assert_eq!(
            map_key(press(KeyCode::Char(']')), Mode::Browse, true, today),
            Some(Message::ViewNextDay),
            "day-stepping stays global while focused"
        );
    }

    #[test]
    fn adding_keys_edit_or_commit() {
        let today = day();
        assert_eq!(
            map_key(press(KeyCode::Char('b')), Mode::Adding, false, today),
            Some(Message::AddChar('b'))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('[')), Mode::Adding, false, today),
            Some(Message::AddChar('[')),
            "brackets type while adding"
        );
        assert_eq!(
            map_key(press(KeyCode::Enter), Mode::Adding, false, today),
            Some(Message::CommitAdd)
        );
        assert_eq!(
            map_key(press(KeyCode::Esc), Mode::Adding, false, today),
            Some(Message::CancelAdd)
        );
        assert_eq!(
            map_key(press(KeyCode::Backspace), Mode::Adding, false, today),
            Some(Message::AddBackspace)
        );
        assert_eq!(
            map_key(press(KeyCode::Tab), Mode::Adding, false, today),
            None
        );
    }
}
