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
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph,
        calendar::{CalendarEventStore, Monthly},
    },
};

use crate::app::{App, EditField, EditorState, Effect, Message, Mode};
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

/// Note lines visible in the editor panel; older lines collapse behind an
/// "N more above" indicator so the tail follows typing.
const EDITOR_NOTES_VISIBLE: usize = 3;

/// Editor panel height: borders + title + priority + notes label + visible lines.
const EDITOR_PANEL_HEIGHT: u16 = 2 + 1 + 1 + 1 + EDITOR_NOTES_VISIBLE as u16;

/// Detail editor panel: focused field marked, text caret at the draft end.
/// Notes show the last few lines so the tail follows typing.
fn editor_panel(editor: &EditorState) -> Paragraph<'_> {
    let marker = |focused: bool| if focused { "> " } else { "  " };
    let field = editor.field();
    let mut lines = vec![
        Line::from(vec![
            Span::raw(marker(field == EditField::Title)),
            Span::raw(format!(
                "Title: {}",
                with_caret(editor.title(), field == EditField::Title)
            )),
        ]),
        Line::from(vec![
            Span::raw(marker(field == EditField::Priority)),
            Span::raw(format!("Priority: {}", editor.priority().as_str())),
        ]),
        Line::from(vec![
            Span::raw(marker(field == EditField::Notes)),
            Span::raw("Notes:"),
        ]),
    ];
    let note_lines: Vec<&str> = editor.notes().split('\n').collect();
    // Tail follows typing; an indicator replaces the oldest visible line
    // when notes run longer than the panel.
    let (start, show_indicator) = if note_lines.len() > EDITOR_NOTES_VISIBLE {
        (note_lines.len() - (EDITOR_NOTES_VISIBLE - 1), true)
    } else {
        (0, false)
    };
    let visible = &note_lines[start..];
    if show_indicator {
        lines.push(Line::raw(format!("    ↑ {} more line(s) above", start)));
    }
    for (i, line) in visible.iter().enumerate() {
        let focused = i + 1 == visible.len() && field == EditField::Notes;
        lines.push(Line::raw(format!("    {}", with_caret(line, focused))));
    }
    Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Edit"))
}

/// Append the text caret when its field holds keyboard focus.
fn with_caret(text: &str, focused: bool) -> String {
    if focused {
        format!("{}▌", text)
    } else {
        text.to_string()
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
    if app.mode() == Mode::Detail {
        constraints.push(Constraint::Length(EDITOR_PANEL_HEIGHT));
    }
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

    if app.mode() == Mode::Detail
        && let Some(editor) = app.editor()
    {
        frame.render_widget(editor_panel(editor), chunks[section]);
        section += 1;
    }

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
                Line::raw(
                    "j/k move · x toggle · a add · e edit · [ ] day · t today · Tab cal · q quit",
                ),
            )
        }
        Mode::Detail => (
            Line::raw("Enter commit/newline · Ctrl+S save"),
            Line::raw("Tab field · ←/→ priority · Esc cancel"),
        ),
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
        Mode::Detail => match key.code {
            KeyCode::Esc => Some(Message::CancelEdit),
            KeyCode::Enter => Some(Message::PressEnter),
            KeyCode::Tab => Some(Message::NextField),
            KeyCode::BackTab => Some(Message::PrevField),
            KeyCode::Backspace => Some(Message::EditBackspace),
            KeyCode::Left => Some(Message::CyclePriority(-1)),
            KeyCode::Right => Some(Message::CyclePriority(1)),
            KeyCode::Char('s') | KeyCode::Char('S')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                Some(Message::CommitEdit)
            }
            KeyCode::Char(c) => Some(Message::EditChar(c)),
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
                KeyCode::Char('e') | KeyCode::Enter => Some(Message::BeginEdit),
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
            Some(Message::BeginEdit)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('e')), Mode::Browse, false, today),
            Some(Message::BeginEdit)
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

    #[test]
    fn detail_keys_edit() {
        let today = day();
        let detail = |code| map_key(press(code), Mode::Detail, false, today);
        assert_eq!(detail(KeyCode::Enter), Some(Message::PressEnter));
        assert_eq!(detail(KeyCode::Esc), Some(Message::CancelEdit));
        assert_eq!(detail(KeyCode::Tab), Some(Message::NextField));
        assert_eq!(detail(KeyCode::BackTab), Some(Message::PrevField));
        assert_eq!(detail(KeyCode::Left), Some(Message::CyclePriority(-1)));
        assert_eq!(detail(KeyCode::Right), Some(Message::CyclePriority(1)));
        assert_eq!(detail(KeyCode::Backspace), Some(Message::EditBackspace));
        assert_eq!(detail(KeyCode::Char('z')), Some(Message::EditChar('z')));
        assert_eq!(
            detail(KeyCode::Char('q')),
            Some(Message::EditChar('q')),
            "no quit while editing"
        );
        assert_eq!(
            map_key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                Mode::Detail,
                false,
                today
            ),
            Some(Message::CommitEdit)
        );
    }
}
