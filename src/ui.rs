//! UI layer: TUI render + event loop + keymap.
//!
//! Rendering observes [`App`] read-only. Keys become [`Message`]s via the pure
//! [`map_key`]; returned [`Effect`]s are performed against the
//! [`Database`](crate::db::Database) and the viewed day reloads. Widget
//! output is verified manually (headless frame capture); only the keymap
//! has unit tests.

use std::time::Duration;

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime};
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph,
        calendar::{CalendarEventStore, Monthly},
    },
};

use crate::app::{App, EditField, EditorState, Effect, Message, Mode};
use crate::db::Database;
use crate::model::{Priority, Task, format_due_input, format_tracked};
use crate::theme;

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
    store.add(to_time_date(today), theme::strong());

    let mut viewed_style = theme::selection();
    if viewed == today {
        viewed_style = viewed_style.add_modifier(Modifier::BOLD);
    }
    store.add(to_time_date(viewed), viewed_style);

    if focused {
        let mut cursor_style = theme::caret();
        if cursor == viewed {
            cursor_style = theme::selection();
        }
        if cursor == today {
            cursor_style = cursor_style.add_modifier(Modifier::BOLD);
        }
        store.add(to_time_date(cursor), cursor_style);
    }

    let display = if focused { cursor } else { viewed };
    Monthly::new(to_time_date(display), store)
        .default_style(theme::text())
        .show_weekdays_header(theme::dim())
        .show_month_header(theme::strong())
}

/// Note lines visible in the editor panel; older lines collapse behind an
/// "N more above" indicator so the tail follows typing.
const EDITOR_NOTES_VISIBLE: usize = 3;

/// Editor panel height: borders + title + priority + notes label +
/// visible lines + due row + tags row + tracked row.
const EDITOR_PANEL_HEIGHT: u16 = 2 + 1 + 1 + 1 + EDITOR_NOTES_VISIBLE as u16 + 3;

/// Detail editor panel: focused field marked, text caret at the draft end.
/// Notes show the last few lines so the tail follows typing. `tracked` is the
/// task's displayed timer total (`H:MM`), including any live running segment.
fn editor_panel<'a>(editor: &'a EditorState, tracked: &'a str) -> Paragraph<'a> {
    let field = editor.field();
    let marker = |focused: bool| {
        Span::styled(
            if focused { "> " } else { "  " },
            if focused {
                theme::strong()
            } else {
                theme::dim()
            },
        )
    };
    let content = |text: String, focused: bool| {
        Span::styled(
            with_caret(&text, focused),
            if focused {
                theme::caret()
            } else {
                theme::text()
            },
        )
    };
    let mut lines = vec![
        Line::from(vec![
            marker(field == EditField::Title),
            content(
                format!("Title: {}", editor.title()),
                field == EditField::Title,
            ),
        ]),
        Line::from(vec![
            marker(field == EditField::Priority),
            Span::styled(
                format!("Priority: {}", editor.priority().as_str()),
                theme::text(),
            ),
        ]),
        Line::from(vec![
            marker(field == EditField::Notes),
            Span::styled("Notes:", theme::text()),
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
        lines.push(Line::styled(
            format!("    ↑ {} more line(s) above", start),
            theme::dim(),
        ));
    }
    for (i, line) in visible.iter().enumerate() {
        let focused = i + 1 == visible.len() && field == EditField::Notes;
        lines.push(Line::from(vec![
            Span::styled("    ", theme::text()),
            content(line.to_string(), focused),
        ]));
    }
    lines.push(Line::from(vec![
        marker(field == EditField::Due),
        content(
            format!("Due: {}", editor.due_text()),
            field == EditField::Due,
        ),
    ]));
    lines.push(Line::from(vec![
        marker(field == EditField::Tags),
        content(
            format!("Tags: {}", editor.tags_text()),
            field == EditField::Tags,
        ),
    ]));
    lines.push(Line::from(vec![
        marker(false),
        Span::styled(format!("Tracked: {tracked}"), theme::dim()),
    ]));
    Paragraph::new(lines)
        .style(theme::text())
        .block(section_block("Edit", theme::strong()))
}

/// Append the text caret when its field holds keyboard focus.
fn with_caret(text: &str, focused: bool) -> String {
    if focused {
        format!("{}▌", text)
    } else {
        text.to_string()
    }
}

/// One task row. `selected` drives the cursor marker; `flag` (the day pin
/// of an overdue task, or the due moment of a due-section task) renders
/// after the checkbox when present. `timer` (`H:MM`) renders as a dim suffix
/// on every row, so each task's accumulated time is visible at a glance.
fn task_row(task: &Task, selected: bool, flag: Option<String>, timer: String) -> ListItem<'_> {
    let marker = if selected { "> " } else { "  " };
    let checkbox = if task.done { "[x]" } else { "[ ]" };
    let row_style = if selected {
        theme::selection()
    } else if task.done {
        theme::done()
    } else {
        theme::text()
    };
    let badge_style = if selected {
        theme::selection()
    } else if task.done {
        theme::done()
    } else {
        match task.priority {
            Priority::High => theme::alert(),
            Priority::Medium => theme::warning(),
            Priority::Low => theme::dim(),
        }
    };
    let mut spans = vec![
        Span::styled(marker, row_style),
        Span::styled(format!("{} ", checkbox), row_style),
    ];
    if let Some(flag) = flag {
        spans.push(Span::styled(format!("[{}] ", flag), row_style));
    }
    spans.push(Span::styled(
        format!("[{}] ", task.priority.letter()),
        badge_style,
    ));
    spans.push(Span::styled(task.title.clone(), row_style));
    spans.push(Span::styled(format!(" {timer}"), theme::dim()));
    ListItem::new(Line::from(spans))
}

/// Timer suffix for a row: the stored total plus any live running segment,
/// as `H:MM`, prefixed with a play marker while the task is being timed.
fn timer_display(app: &App, task: &Task, now: NaiveDateTime) -> String {
    let total = format_tracked(app.tracked_display(task, now));
    if app.running_task_id() == Some(task.id) {
        format!("▶ {total}")
    } else {
        total
    }
}

/// Append `#tags` to a section flag so filtered rows show why they
/// matched; untagged tasks keep the bare flag.
fn with_tags(base: String, tags: &[String]) -> String {
    if tags.is_empty() {
        base
    } else {
        format!("{base} #{}", tags.join(" #"))
    }
}

/// Flag rendering a task's day pin in the overdue section.
fn day_flag(task: &Task) -> String {
    with_tags(task.day.format("%Y-%m-%d").to_string(), &task.tags)
}

/// Flag rendering a task's due moment in the due sections,
/// mirroring the overdue `[YYYY-MM-DD]` pin flag.
fn due_flag(task: &Task) -> Option<String> {
    task.due
        .map(|due| with_tags(format!("due {}", format_due_input(&due)), &task.tags))
}

/// Flag rendering a task's tags on the day list (no day/due pin there).
fn tags_flag(task: &Task) -> Option<String> {
    if task.tags.is_empty() {
        None
    } else {
        Some(format!("#{}", task.tags.join(" #")))
    }
}

/// Flag rendering an overdue task's day pin; every overdue row carries one.
fn overdue_flag(task: &Task) -> Option<String> {
    Some(day_flag(task))
}

/// Bordered section frame in the palette: dim border, themed title, and the
/// dark backdrop so blank rows inside the box stay on-theme.
fn section_block(title: &str, title_style: Style) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .title_style(title_style)
        .border_style(theme::block())
        .style(theme::text())
}

/// One section of the day-list column. The variants, in [`SECTIONS`], are the
/// single source of the render: order, titles, styles, and the per-row flag
/// rule all live here, and every section is drawn by the same loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    MissedDue,
    DueToday,
    Overdue,
    Today,
}

/// Sections in render order, most urgent first.
const SECTIONS: [Section; 4] = [
    Section::MissedDue,
    Section::DueToday,
    Section::Overdue,
    Section::Today,
];

impl Section {
    /// Bordered frame title; the viewed Day list names its date.
    fn title(self, app: &App) -> String {
        match self {
            Section::MissedDue => "Missed Due".to_string(),
            Section::DueToday => "Due Today".to_string(),
            Section::Overdue => "Overdue".to_string(),
            Section::Today => format!("Tasker — {}", app.viewed().format("%a %b %d, %Y")),
        }
    }

    /// Title style: Missed due keeps the error colour, the rest take the
    /// accent so the day-list headings read as one family.
    fn title_style(self) -> Style {
        match self {
            Section::MissedDue => theme::alert(),
            _ => theme::accent(),
        }
    }

    /// The flag drawn after a row's checkbox: the due moment in the due
    /// sections, the day pin when overdue, the tags on the viewed day.
    fn flag(self) -> fn(&Task) -> Option<String> {
        match self {
            Section::MissedDue | Section::DueToday => due_flag,
            Section::Overdue => overdue_flag,
            Section::Today => tags_flag,
        }
    }

    /// Visible rows under the active tag filter.
    fn rows(self, app: &App) -> Vec<Task> {
        match self {
            Section::MissedDue => app.visible_missed_due(),
            Section::DueToday => app.visible_due_today(),
            Section::Overdue => app.visible_overdue(),
            Section::Today => app.visible_tasks(),
        }
    }

    /// Row holding the selection within this section, if any.
    fn cursor(self, app: &App) -> Option<usize> {
        match self {
            Section::MissedDue => app.missed_due_cursor(),
            Section::DueToday => app.due_today_cursor(),
            Section::Overdue => app.overdue_cursor(),
            Section::Today => app.today_cursor(),
        }
    }

    /// Whether the section is dropped when it has no rows. The derived
    /// sections are omitted when empty; the viewed Day list always shows.
    fn omitted_when_empty(self) -> bool {
        self != Section::Today
    }

    /// Height in the vertical split: a derived section sizes to its rows;
    /// the viewed Day list takes the remainder.
    fn constraint(self, rows: usize) -> Constraint {
        match self {
            Section::Today => Constraint::Min(1),
            _ => Constraint::Length(rows as u16 + 2),
        }
    }
}

/// The sections to render this frame, in order, each paired with its visible
/// rows: the one source the day-list column draws from. An empty derived
/// section is dropped; the viewed Day list always remains.
fn day_sections(app: &App) -> Vec<(Section, Vec<Task>)> {
    SECTIONS
        .into_iter()
        .map(|section| (section, section.rows(app)))
        .filter(|(section, rows)| !(rows.is_empty() && section.omitted_when_empty()))
        .collect()
}

/// Draw one frame: calendar pane left, day-list column right
/// (Missed-due and Due-today sections when non-empty, Overdue section
/// when non-empty, viewed day, status footer).
fn render(frame: &mut Frame, app: &App, today: NaiveDate, now: NaiveDateTime) {
    // Paint the whole frame in the palette first, so any cell no widget
    // covers still sits on the dark backdrop.
    frame.render_widget(Block::default().style(theme::text()), frame.area());

    let calendar = month_calendar(app, today);
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(calendar.width() + 2), Constraint::Min(1)])
        .split(frame.area());

    let border_style = if app.calendar_focused() {
        theme::focused_block()
    } else {
        theme::block()
    };
    frame.render_widget(
        calendar.block(
            Block::default()
                .borders(Borders::ALL)
                .title("Calendar")
                .title_style(theme::strong())
                .border_style(border_style)
                .style(theme::text()),
        ),
        outer[0],
    );

    // One shared path draws every section: order, titles, and flags come from
    // [`day_sections`], so an active tag filter narrows the whole column at
    // once. A derived section with no rows is omitted.
    let sections = day_sections(app);

    let mut constraints: Vec<Constraint> = sections
        .iter()
        .map(|(section, rows)| section.constraint(rows.len()))
        .collect();
    if app.mode() == Mode::Detail {
        constraints.push(Constraint::Length(EDITOR_PANEL_HEIGHT));
    }
    constraints.push(Constraint::Length(2));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(outer[1]);

    for (index, (section, rows)) in sections.iter().enumerate() {
        let cursor = section.cursor(app);
        let items: Vec<ListItem> = rows
            .iter()
            .enumerate()
            .map(|(i, task)| {
                task_row(
                    task,
                    Some(i) == cursor,
                    (section.flag())(task),
                    timer_display(app, task, now),
                )
            })
            .collect();
        let list = List::new(items)
            .style(theme::text())
            .block(section_block(&section.title(app), section.title_style()));
        let mut state = ListState::default();
        state.select(cursor);
        frame.render_stateful_widget(list, chunks[index], &mut state);
    }

    let mut next = sections.len();
    if app.mode() == Mode::Detail
        && let Some(editor) = app.editor()
    {
        let tracked = format_tracked(app.tracked_display(editor.original(), now));
        frame.render_widget(editor_panel(editor, &tracked), chunks[next]);
        next += 1;
    }

    let (status, help) = match app.mode() {
        Mode::Adding => (
            Line::from(vec![
                Span::styled("Add: ", theme::strong()),
                Span::styled(app.draft().to_string(), theme::caret()),
                Span::styled("▌", theme::caret()),
            ]),
            Line::styled("Enter commit · Esc cancel", theme::dim()),
        ),
        Mode::Browse => {
            let total: usize = sections.iter().map(|(_, rows)| rows.len()).sum();
            let done = sections
                .iter()
                .flat_map(|(_, rows)| rows)
                .filter(|task| task.done)
                .count();
            let status = match app.tag_filter() {
                Some(tag) => format!("[#{tag}] {done}/{total} done"),
                None => format!("{done}/{total} done"),
            };
            let status_style = if sections
                .iter()
                .any(|(section, _)| *section == Section::MissedDue)
            {
                theme::alert()
            } else {
                theme::strong()
            };
            (
                Line::styled(status, status_style),
                Line::styled(
                    "j/k move · x toggle · d del · u undo · a add · e edit · w timer · f filter · [ ] day · t today · Tab cal · ? help · q quit",
                    theme::dim(),
                ),
            )
        }
        Mode::Detail => (
            Line::styled("Enter commit/newline · Ctrl+S save", theme::strong()),
            Line::styled("Tab field · ←/→ priority · Esc cancel", theme::dim()),
        ),
    };
    frame.render_widget(
        Paragraph::new(vec![status, help]).style(theme::text()),
        chunks[next],
    );

    if app.help_open() {
        let popup = centered_rect(70, 80, frame.area());
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(help_text())
                .style(theme::text())
                .block(section_block("Help — ?/Esc to close", theme::strong())),
            popup,
        );
    }
}

/// Centered rectangle covering the given percentages of `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// Every keybinding in the app, grouped by context.
fn help_text() -> Vec<Line<'static>> {
    vec![
        Line::raw("Day list"),
        Line::raw("  j/k, arrows   move selection"),
        Line::raw("  x, Space      toggle done"),
        Line::raw("  d             delete task (undo with u)"),
        Line::raw("  u             undo last delete"),
        Line::raw("  a             add task"),
        Line::raw("  e, Enter      edit task"),
        Line::raw("  w             start/stop timer (one at a time)"),
        Line::raw("  f             cycle tag filter"),
        Line::raw("  Esc           clear tag filter"),
        Line::raw("  Tab           focus calendar"),
        Line::raw("  q             quit"),
        Line::raw(""),
        Line::raw("Days"),
        Line::raw("  [ / ]         previous / next day"),
        Line::raw("  t             back to today"),
        Line::raw(""),
        Line::raw("Calendar (Tab to focus)"),
        Line::raw("  arrows, hjkl  move cursor (up/down = week)"),
        Line::raw("  Enter, Space  jump to cursor day"),
        Line::raw("  Esc, Tab      back to list"),
        Line::raw(""),
        Line::raw("Add"),
        Line::raw("  type, Backspace, Enter commit, Esc cancel"),
        Line::raw(""),
        Line::raw("Edit"),
        Line::raw("  Tab / Shift+Tab  switch field"),
        Line::raw("  Left/Right, Space cycle priority"),
        Line::raw("  Due: YYYY-MM-DD HH:MM, blank clears"),
        Line::raw("  Tags: a, b, c (blank clears)"),
        Line::raw("  type, Backspace  edit field"),
        Line::raw("  Enter            commit (newline in notes)"),
        Line::raw("  Ctrl+S           save"),
        Line::raw("  Esc              cancel"),
        Line::raw(""),
        Line::raw("  ?             this help"),
    ]
}

/// Pure key-to-message mapping; unmapped keys yield None (ignored).
/// `now` carries the wall clock into time-stamped messages (`t`, `w`, `q`) so
/// state stays pure.
fn map_key(
    key: KeyEvent,
    mode: Mode,
    calendar_focused: bool,
    now: NaiveDateTime,
) -> Option<Message> {
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
            KeyCode::Char('t') => Some(Message::ViewToday(now.date())),
            KeyCode::Char('q') => Some(Message::Quit(now)),
            KeyCode::Char('?') => Some(Message::ToggleHelp),
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
                KeyCode::Char('d') => Some(Message::DeleteSelected),
                KeyCode::Char('u') => Some(Message::UndoDelete),
                KeyCode::Char('a') => Some(Message::BeginAdd),
                KeyCode::Char('e') | KeyCode::Enter => Some(Message::BeginEdit),
                KeyCode::Char('w') => Some(Message::ToggleTimer(now)),
                KeyCode::Char('f') => Some(Message::CycleTagFilter),
                KeyCode::Esc => Some(Message::ClearTagFilter),
                KeyCode::Tab => Some(Message::FocusCalendar),
                _ => None,
            },
        },
    }
}

/// Block until a key maps to a message for the current mode and focus.
/// While the help overlay is open the app behind it is frozen: only `?`
/// and `Esc` get through (closing help); every other key is swallowed so
/// the overlay can neither act nor be left stale behind.
fn read_message(mode: Mode, calendar_focused: bool, help_open: bool) -> std::io::Result<Message> {
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if help_open {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => return Ok(Message::ToggleHelp),
                _ => continue,
            }
        }
        // Read the clock after the key so timer starts are stamped at the
        // moment of the press, not when the previous frame was drawn.
        let now = Local::now().naive_local();
        if let Some(message) = map_key(key, mode, calendar_focused, now) {
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
        let now = Local::now().naive_local();
        let today = now.date();
        terminal.draw(|frame| render(frame, app, today, now))?;
        // Idle refreshes keep a running timer ticking; any key wakes us at once.
        if !event::poll(Duration::from_millis(500))? {
            if app.should_quit() {
                break;
            }
            continue;
        }
        let effects = app.update(read_message(
            app.mode(),
            app.calendar_focused(),
            app.help_open(),
        )?);
        if !effects.is_empty() {
            for effect in effects {
                match effect {
                    Effect::SaveNew(task) => {
                        db.save_task(&task)?;
                    }
                    Effect::Update(task) => {
                        db.update_task(&task)?;
                    }
                    Effect::AddTrackedSecs {
                        task_id,
                        delta_secs,
                    } => {
                        db.add_tracked_secs(task_id, delta_secs)?;
                    }
                    Effect::Delete { task_id } => {
                        db.delete_task(task_id)?;
                    }
                    Effect::Restore(task) => {
                        db.restore_task(&task)?;
                    }
                    Effect::Reload => {}
                }
            }
            app.set_tasks(db.tasks_for_day(app.viewed())?);
            // Overdue and the due sections are anchored to real today, never
            // to the viewed day: browsing a past day must not hide them, and
            // a future day must not promote today's tasks into them.
            app.set_overdue(db.overdue_tasks(today)?);
            app.set_due_today(db.due_today_tasks(today)?);
            app.set_missed_due(db.missed_due_tasks(now)?);
            // Tag names feed the `f` filter cycle; a filter pointing at a
            // vanished tag clears inside the setter.
            app.set_all_tags(db.all_tags()?);
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

    fn noon() -> NaiveDateTime {
        day().and_hms_opt(12, 0, 0).expect("valid datetime")
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn browse_list_keys_map() {
        let now = noon();
        assert_eq!(
            map_key(press(KeyCode::Char('q')), Mode::Browse, false, now),
            Some(Message::Quit(now))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('j')), Mode::Browse, false, now),
            Some(Message::SelectNext)
        );
        assert_eq!(
            map_key(press(KeyCode::Down), Mode::Browse, false, now),
            Some(Message::SelectNext)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('k')), Mode::Browse, false, now),
            Some(Message::SelectPrev)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('x')), Mode::Browse, false, now),
            Some(Message::ToggleSelected)
        );
        assert_eq!(
            map_key(press(KeyCode::Char(' ')), Mode::Browse, false, now),
            Some(Message::ToggleSelected)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('d')), Mode::Browse, false, now),
            Some(Message::DeleteSelected),
            "delete is its own key, distinct from the done toggle"
        );
        assert_eq!(
            map_key(press(KeyCode::Char('u')), Mode::Browse, false, now),
            Some(Message::UndoDelete)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('a')), Mode::Browse, false, now),
            Some(Message::BeginAdd)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('[')), Mode::Browse, false, now),
            Some(Message::ViewPrevDay)
        );
        assert_eq!(
            map_key(press(KeyCode::Char(']')), Mode::Browse, false, now),
            Some(Message::ViewNextDay)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('t')), Mode::Browse, false, now),
            Some(Message::ViewToday(day()))
        );
        assert_eq!(
            map_key(press(KeyCode::Tab), Mode::Browse, false, now),
            Some(Message::FocusCalendar)
        );
        assert_eq!(
            map_key(press(KeyCode::Enter), Mode::Browse, false, now),
            Some(Message::BeginEdit)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('e')), Mode::Browse, false, now),
            Some(Message::BeginEdit)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('w')), Mode::Browse, false, now),
            Some(Message::ToggleTimer(now))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('f')), Mode::Browse, false, now),
            Some(Message::CycleTagFilter)
        );
        assert_eq!(
            map_key(press(KeyCode::Esc), Mode::Browse, false, now),
            Some(Message::ClearTagFilter)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('?')), Mode::Browse, false, now),
            Some(Message::ToggleHelp)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('?')), Mode::Browse, true, now),
            Some(Message::ToggleHelp),
            "help is reachable while focused"
        );
    }

    #[test]
    fn browse_calendar_keys_move_cursor() {
        let now = noon();
        assert_eq!(
            map_key(press(KeyCode::Left), Mode::Browse, true, now),
            Some(Message::MoveCursor(-1))
        );
        assert_eq!(
            map_key(press(KeyCode::Right), Mode::Browse, true, now),
            Some(Message::MoveCursor(1))
        );
        assert_eq!(
            map_key(press(KeyCode::Up), Mode::Browse, true, now),
            Some(Message::MoveCursor(-7))
        );
        assert_eq!(
            map_key(press(KeyCode::Down), Mode::Browse, true, now),
            Some(Message::MoveCursor(7))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('h')), Mode::Browse, true, now),
            Some(Message::MoveCursor(-1))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('l')), Mode::Browse, true, now),
            Some(Message::MoveCursor(1))
        );
        assert_eq!(
            map_key(press(KeyCode::Enter), Mode::Browse, true, now),
            Some(Message::JumpToCursor)
        );
        assert_eq!(
            map_key(press(KeyCode::Char(' ')), Mode::Browse, true, now),
            Some(Message::JumpToCursor)
        );
        assert_eq!(
            map_key(press(KeyCode::Esc), Mode::Browse, true, now),
            Some(Message::FocusList)
        );
        assert_eq!(
            map_key(press(KeyCode::Tab), Mode::Browse, true, now),
            Some(Message::FocusList)
        );
        assert_eq!(
            map_key(press(KeyCode::Char(']')), Mode::Browse, true, now),
            Some(Message::ViewNextDay),
            "day-stepping stays global while focused"
        );
        assert_eq!(
            map_key(press(KeyCode::Char('w')), Mode::Browse, true, now),
            None,
            "timer is list-scoped, like the other row actions"
        );
        assert_eq!(
            map_key(press(KeyCode::Char('d')), Mode::Browse, true, now),
            None,
            "delete is list-scoped too"
        );
        assert_eq!(
            map_key(press(KeyCode::Char('u')), Mode::Browse, true, now),
            None,
            "undo is list-scoped too"
        );
    }

    #[test]
    fn adding_keys_edit_or_commit() {
        let now = noon();
        assert_eq!(
            map_key(press(KeyCode::Char('b')), Mode::Adding, false, now),
            Some(Message::AddChar('b'))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('[')), Mode::Adding, false, now),
            Some(Message::AddChar('[')),
            "brackets type while adding"
        );
        assert_eq!(
            map_key(press(KeyCode::Enter), Mode::Adding, false, now),
            Some(Message::CommitAdd)
        );
        assert_eq!(
            map_key(press(KeyCode::Esc), Mode::Adding, false, now),
            Some(Message::CancelAdd)
        );
        assert_eq!(
            map_key(press(KeyCode::Backspace), Mode::Adding, false, now),
            Some(Message::AddBackspace)
        );
        assert_eq!(map_key(press(KeyCode::Tab), Mode::Adding, false, now), None);
    }

    #[test]
    fn detail_keys_edit() {
        let now = noon();
        let detail = |code| map_key(press(code), Mode::Detail, false, now);
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
                now
            ),
            Some(Message::CommitEdit)
        );
    }

    fn task(id: i64, title: &str) -> Task {
        Task {
            id,
            day: day(),
            title: title.to_string(),
            done: false,
            priority: Priority::Medium,
            notes: String::new(),
            due: None,
            tracked_secs: 0,
            tags: Vec::new(),
        }
    }

    fn due_task(id: i64, title: &str) -> Task {
        Task {
            due: Some(noon()),
            ..task(id, title)
        }
    }

    fn tagged_task(id: i64, title: &str, tags: &[&str]) -> Task {
        Task {
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            ..task(id, title)
        }
    }

    /// Render one frame headlessly and return the backing buffer.
    fn render_frame(app: &App, now: NaiveDateTime) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(140, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render(frame, app, now.date(), now))
            .expect("draw frame");
        terminal.backend().buffer().clone()
    }

    /// Flatten a rendered buffer into plain text, one line per row.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn smoke_app() -> App {
        let mut app = App::new(
            day(),
            vec![task(1, "today work"), task(2, "another task")],
            vec![task(9, "old overdue work")],
        );
        app.set_missed_due(vec![due_task(7, "missed work")]);
        app.set_due_today(vec![due_task(8, "due work")]);
        app
    }

    #[test]
    fn headless_frame_renders_sections_through_the_palette() {
        let app = smoke_app();
        let buffer = render_frame(&app, noon());
        let text = buffer_text(&buffer);

        assert!(text.contains("Missed Due"), "missed section drawn");
        assert!(text.contains("Due Today"), "due section drawn");
        assert!(text.contains("Overdue"), "overdue section drawn");
        assert!(text.contains("Tasker — "), "day heading drawn");
        assert!(text.contains("old overdue work"), "overdue row drawn");

        // The palette reaches the buffer: backdrop set, and every role used.
        assert_eq!(buffer[(0, 0)].bg, theme::BACKGROUND, "backdrop painted");
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.fg == theme::FOREGROUND),
            "foreground used"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == theme::DIM),
            "dim secondary used"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == theme::ACCENT),
            "accent used"
        );
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.bg == theme::SELECTION),
            "selection fill used"
        );
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.fg == theme::WARNING),
            "warning used"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == theme::ERROR),
            "error used"
        );
    }

    #[test]
    fn headless_frame_shows_due_flags_and_timer_totals() {
        let mut app = smoke_app();
        let mut timed = task(3, "timed task");
        timed.tracked_secs = 3660;
        app.set_tasks(vec![timed]);

        let text = buffer_text(&render_frame(&app, noon()));

        assert!(text.contains("2026-10-10"), "due flag rendered");
        assert!(text.contains("1:01"), "timer total rendered");
        assert!(
            text.contains("0:00"),
            "every row carries a total, even at zero"
        );
    }

    #[test]
    fn headless_editor_frame_shows_tracked_total() {
        let mut app = smoke_app();
        app.update(crate::app::Message::BeginEdit);
        let text = buffer_text(&render_frame(&app, noon()));
        assert!(text.contains("Tracked:"), "editor pane shows timer");
    }

    #[test]
    fn headless_help_overlay_documents_delete_and_undo() {
        let mut app = smoke_app();
        app.update(crate::app::Message::ToggleHelp);
        let text = buffer_text(&render_frame(&app, noon()));
        assert!(
            text.contains("delete task"),
            "help documents delete:\n{text}"
        );
        assert!(
            text.contains("undo last delete"),
            "help documents undo:\n{text}"
        );
    }

    #[test]
    fn day_sections_render_in_one_fixed_order_with_fixed_titles() {
        let app = smoke_app();
        let titles: Vec<String> = day_sections(&app)
            .iter()
            .map(|(section, _)| section.title(&app))
            .collect();
        assert_eq!(
            titles,
            vec![
                "Missed Due".to_string(),
                "Due Today".to_string(),
                "Overdue".to_string(),
                format!("Tasker — {}", day().format("%a %b %d, %Y")),
            ]
        );
    }

    #[test]
    fn day_sections_omit_empty_derived_sections_but_keep_the_day_list() {
        let app = App::new(day(), vec![], vec![]);
        let titles: Vec<String> = day_sections(&app)
            .iter()
            .map(|(section, _)| section.title(&app))
            .collect();
        assert_eq!(
            titles,
            vec![format!("Tasker — {}", day().format("%a %b %d, %Y"))],
            "an empty viewed day still draws its section"
        );
    }

    #[test]
    fn day_sections_narrow_together_under_a_tag_filter() {
        let mut app = App::new(
            day(),
            vec![
                tagged_task(1, "work today", &["work"]),
                tagged_task(2, "admin today", &["admin"]),
            ],
            vec![tagged_task(9, "work overdue", &["work"])],
        );
        app.set_missed_due(vec![
            tagged_task(7, "work missed", &["work"]),
            tagged_task(6, "admin missed", &["admin"]),
        ]);
        app.set_due_today(vec![tagged_task(8, "work due", &["work"])]);
        app.set_all_tags(vec!["work".to_string(), "admin".to_string()]);
        app.update(Message::CycleTagFilter); // -> "#work"

        let sections = day_sections(&app);
        assert_eq!(sections.len(), 4, "each section still holds a #work row");
        for (section, rows) in &sections {
            assert!(
                rows.iter()
                    .all(|task| task.tags.iter().any(|tag| tag == "work")),
                "{section:?} narrows with every other section"
            );
        }
    }
}
