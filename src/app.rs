//! App state seam: pure state transitions, no terminal or database I/O.
//!
//! The event loop translates keys to [`Message`]. [`App::update`] evolves the
//! in-memory state and returns [`Effect`]s; the runtime performs each effect
//! against the [`Database`](crate::db::Database) and reloads the day list.
//! Tests assert on states and effects only.

use chrono::{Duration, NaiveDate};

use crate::model::{NewTask, Priority, Task};

/// Input mode: browsing the day list or composing a new task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Browse,
    Adding,
}

/// Every event the app reacts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    SelectNext,
    SelectPrev,
    ToggleSelected,
    BeginAdd,
    AddChar(char),
    AddBackspace,
    CommitAdd,
    CancelAdd,
    ViewPrevDay,
    ViewNextDay,
    /// Return to today. The date rides along so pure state never reads a clock.
    ViewToday(NaiveDate),
    /// Move keyboard focus into the calendar pane; cursor starts on viewed.
    FocusCalendar,
    /// Move keyboard focus back to the day list.
    FocusList,
    /// Move the calendar cursor by a day delta (-1/+1 day, -7/+7 week).
    MoveCursor(i32),
    /// Make the cursor day the viewed day.
    JumpToCursor,
    Quit,
}

/// Persistence work the runtime must perform after an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    SaveNew(NewTask),
    Update(Task),
    /// Reload the viewed day's list from storage (after navigation).
    Reload,
}

/// In-memory state of the day-list view: today's tasks plus the
/// Overdue section (open tasks from past days, still pinned to their
/// original day). Selection spans the combined list, overdue first.
#[derive(Debug)]
pub struct App {
    viewed: NaiveDate,
    tasks: Vec<Task>,
    overdue: Vec<Task>,
    selected: usize,
    mode: Mode,
    draft: String,
    calendar_focused: bool,
    cursor: NaiveDate,
    should_quit: bool,
}

impl App {
    /// Day list for `viewed`, freshly loaded with its overdue section.
    pub fn new(viewed: NaiveDate, tasks: Vec<Task>, overdue: Vec<Task>) -> App {
        let mut app = App {
            viewed,
            cursor: viewed,
            tasks: Vec::new(),
            overdue: Vec::new(),
            selected: 0,
            mode: Mode::Browse,
            draft: String::new(),
            calendar_focused: false,
            should_quit: false,
        };
        app.set_tasks(tasks);
        app.set_overdue(overdue);
        app
    }

    pub fn viewed(&self) -> NaiveDate {
        self.viewed
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// Open past-day tasks surfacing in today's view, oldest first.
    pub fn overdue(&self) -> &[Task] {
        &self.overdue
    }

    /// Raw cursor over the combined list. Prefer the section cursors
    /// ([`App::overdue_cursor`], [`App::today_cursor`]) in new code;
    /// kept for tests asserting wrap-around positions.
    #[allow(dead_code)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Rows in the combined list (overdue first, then today).
    fn combined_len(&self) -> usize {
        self.overdue.len() + self.tasks.len()
    }

    /// Split a combined-list index into its section: `true` + row means an
    /// overdue row, `false` + row means a today row.
    fn split_index(&self, index: usize) -> (bool, usize) {
        if index < self.overdue.len() {
            (true, index)
        } else {
            (false, index - self.overdue.len())
        }
    }

    /// Cursor row within the overdue section, if it holds the selection.
    pub fn overdue_cursor(&self) -> Option<usize> {
        match self.split_index(self.selected) {
            (true, row) if row < self.overdue.len() => Some(row),
            _ => None,
        }
    }

    /// Cursor row within today's list, if it holds the selection.
    pub fn today_cursor(&self) -> Option<usize> {
        match self.split_index(self.selected) {
            (false, row) if row < self.tasks.len() => Some(row),
            _ => None,
        }
    }

    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.combined_len().saturating_sub(1));
    }

    /// Task under the selection cursor, if the combined list is non-empty.
    /// Used by detail flows from ticket #6; tests are the first caller.
    #[allow(dead_code)]
    pub fn selected_task(&self) -> Option<&Task> {
        match self.split_index(self.selected) {
            (true, row) => self.overdue.get(row),
            (false, row) => self.tasks.get(row),
        }
    }

    fn selected_task_mut(&mut self) -> Option<&mut Task> {
        match self.split_index(self.selected) {
            (true, row) => self.overdue.get_mut(row),
            (false, row) => self.tasks.get_mut(row),
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// Whether keyboard focus is in the calendar pane.
    pub fn calendar_focused(&self) -> bool {
        self.calendar_focused
    }

    /// The calendar cursor day.
    pub fn cursor(&self) -> NaiveDate {
        self.cursor
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Replace today's task list (e.g. after reloading from storage),
    /// keeping the selection on a valid row of the combined list.
    pub fn set_tasks(&mut self, tasks: Vec<Task>) {
        self.tasks = tasks;
        self.clamp_selection();
    }

    /// Replace the overdue section (e.g. after reloading from storage),
    /// keeping the selection on a valid row of the combined list.
    pub fn set_overdue(&mut self, overdue: Vec<Task>) {
        self.overdue = overdue;
        self.clamp_selection();
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.draft.clear();
    }

    /// Shift a day, saturating at the calendar edge (unreachable in practice).
    fn shift_day(day: NaiveDate, delta: i32) -> NaiveDate {
        day.checked_add_signed(Duration::days(delta as i64))
            .unwrap_or(day)
    }

    /// Make `day` the viewed day: cursor follows, list reloads via effect.
    fn goto_day(&mut self, day: NaiveDate) -> Vec<Effect> {
        self.viewed = day;
        self.cursor = day;
        vec![Effect::Reload]
    }

    /// Step the viewed day by a day delta (month edges handled by chrono).
    fn step_viewed(&mut self, delta: i32) -> Vec<Effect> {
        let day = Self::shift_day(self.viewed, delta);
        self.goto_day(day)
    }

    fn move_selection(&mut self, step: isize) {
        if self.combined_len() == 0 {
            return;
        }
        let len = self.combined_len() as isize;
        self.selected = (self.selected as isize + step).rem_euclid(len) as usize;
    }

    /// Evolve state for one message, returning persistence effects
    /// for the runtime. Never touches storage or the terminal.
    pub fn update(&mut self, message: Message) -> Vec<Effect> {
        match message {
            Message::SelectNext => {
                self.move_selection(1);
                vec![]
            }
            Message::SelectPrev => {
                self.move_selection(-1);
                vec![]
            }
            Message::ToggleSelected => match self.selected_task_mut() {
                Some(task) => {
                    task.toggle();
                    vec![Effect::Update(task.clone())]
                }
                None => vec![],
            },
            Message::BeginAdd => {
                self.set_mode(Mode::Adding);
                vec![]
            }
            Message::AddChar(c) => {
                if self.mode == Mode::Adding {
                    self.draft.push(c);
                }
                vec![]
            }
            Message::AddBackspace => {
                if self.mode == Mode::Adding {
                    self.draft.pop();
                }
                vec![]
            }
            Message::CommitAdd => {
                let title = self.draft.trim().to_string();
                self.set_mode(Mode::Browse);
                if title.is_empty() {
                    vec![]
                } else {
                    vec![Effect::SaveNew(NewTask {
                        day: self.viewed,
                        title,
                        priority: Priority::Medium,
                        notes: String::new(),
                    })]
                }
            }
            Message::CancelAdd => {
                self.set_mode(Mode::Browse);
                vec![]
            }
            Message::ViewPrevDay => self.step_viewed(-1),
            Message::ViewNextDay => self.step_viewed(1),
            Message::ViewToday(today) => self.goto_day(today),
            Message::FocusCalendar => {
                self.calendar_focused = true;
                self.cursor = self.viewed;
                vec![]
            }
            Message::FocusList => {
                self.calendar_focused = false;
                vec![]
            }
            Message::MoveCursor(delta) => {
                if self.calendar_focused {
                    self.cursor = Self::shift_day(self.cursor, delta);
                }
                vec![]
            }
            Message::JumpToCursor => {
                if !self.calendar_focused {
                    return vec![];
                }
                self.calendar_focused = false;
                self.goto_day(self.cursor)
            }
            Message::Quit => {
                self.should_quit = true;
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date")
    }

    fn task(id: i64, title: &str) -> Task {
        Task {
            id,
            day: day(),
            title: title.to_string(),
            done: false,
            priority: Priority::Medium,
            notes: String::new(),
        }
    }

    fn three_tasks() -> App {
        App::new(
            day(),
            vec![task(1, "one"), task(2, "two"), task(3, "three")],
            vec![],
        )
    }

    fn with_overdue() -> App {
        let mut old = task(9, "old unfinished");
        old.day = NaiveDate::from_ymd_opt(2026, 10, 8).expect("valid date");
        App::new(day(), vec![task(1, "one"), task(2, "two")], vec![old])
    }

    #[test]
    fn overdue_section_is_exposed_separately() {
        let app = with_overdue();
        assert_eq!(app.overdue().len(), 1);
        assert_eq!(app.overdue()[0].title, "old unfinished");
        assert_eq!(app.tasks().len(), 2);
    }

    #[test]
    fn selection_spans_the_combined_list() {
        let mut app = with_overdue();
        assert_eq!(app.selected(), 0);
        assert_eq!(
            app.selected_task().map(|t| t.title.as_str()),
            Some("old unfinished")
        );
        app.update(Message::SelectNext);
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("one"));
        app.update(Message::SelectNext);
        app.update(Message::SelectNext);
        assert_eq!(app.selected(), 0, "selection wraps the combined list");
    }

    #[test]
    fn toggle_overdue_task_flips_and_emits_update() {
        let mut app = with_overdue();

        let effects = app.update(Message::ToggleSelected);

        assert!(app.overdue()[0].done);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], Effect::Update(app.overdue()[0].clone()));
    }

    #[test]
    fn completing_overdue_then_reloading_clears_the_section() {
        let mut app = with_overdue();
        app.update(Message::ToggleSelected);

        app.set_overdue(vec![]);
        app.set_tasks(app.tasks().to_vec());

        assert!(app.overdue().is_empty());
        assert_eq!(app.selected(), 0);
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("one"));
    }

    #[test]
    fn set_overdue_clamps_selection() {
        let mut app = with_overdue();
        app.update(Message::SelectNext);
        app.update(Message::SelectNext);
        assert_eq!(app.selected(), 2);

        app.set_tasks(vec![]);
        app.set_overdue(vec![]);
        assert_eq!(app.selected(), 0);
        assert_eq!(app.selected_task(), None);
    }

    #[test]
    fn select_next_wraps_around() {
        let mut app = three_tasks();
        app.update(Message::SelectNext);
        assert_eq!(app.selected(), 1);
        app.update(Message::SelectNext);
        app.update(Message::SelectNext);
        assert_eq!(app.selected(), 0);
    }

    #[test]
    fn select_prev_wraps_around() {
        let mut app = three_tasks();
        app.update(Message::SelectPrev);
        assert_eq!(app.selected(), 2);
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("three"));
    }

    #[test]
    fn select_on_empty_list_is_noop() {
        let mut app = App::new(day(), vec![], vec![]);
        assert_eq!(app.update(Message::SelectNext), vec![]);
        assert_eq!(app.update(Message::SelectPrev), vec![]);
        assert_eq!(app.selected(), 0);
        assert_eq!(app.selected_task(), None);
    }

    #[test]
    fn toggle_flips_state_and_emits_update() {
        let mut app = three_tasks();
        app.update(Message::SelectNext);

        let effects = app.update(Message::ToggleSelected);

        let mut expected = task(2, "two");
        expected.done = true;
        assert_eq!(effects, vec![Effect::Update(expected.clone())]);
        assert_eq!(app.selected_task(), Some(&expected));

        let effects = app.update(Message::ToggleSelected);
        assert_eq!(
            effects,
            vec![Effect::Update(task(2, "two"))],
            "toggling again reopens the task"
        );
    }

    #[test]
    fn toggle_on_empty_list_emits_nothing() {
        let mut app = App::new(day(), vec![], vec![]);
        assert_eq!(app.update(Message::ToggleSelected), vec![]);
    }

    #[test]
    fn begin_add_enters_adding_with_empty_draft() {
        let mut app = three_tasks();
        assert_eq!(app.update(Message::BeginAdd), vec![]);
        assert_eq!(app.mode(), Mode::Adding);
        assert_eq!(app.draft(), "");
    }

    #[test]
    fn typing_and_backspace_edit_the_draft() {
        let mut app = three_tasks();
        app.update(Message::BeginAdd);
        app.update(Message::AddChar('h'));
        app.update(Message::AddChar('i'));
        assert_eq!(app.draft(), "hi");
        app.update(Message::AddBackspace);
        assert_eq!(app.draft(), "h");
    }

    #[test]
    fn typing_outside_add_mode_is_ignored() {
        let mut app = three_tasks();
        app.update(Message::AddChar('x'));
        assert_eq!(app.draft(), "");
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn commit_add_emits_save_new_and_returns_to_browse() {
        let mut app = three_tasks();
        app.update(Message::BeginAdd);
        for c in "  buy milk ".chars() {
            app.update(Message::AddChar(c));
        }

        let effects = app.update(Message::CommitAdd);

        assert_eq!(
            effects,
            vec![Effect::SaveNew(NewTask {
                day: day(),
                title: "buy milk".to_string(),
                priority: Priority::Medium,
                notes: String::new(),
            })]
        );
        assert_eq!(app.mode(), Mode::Browse);
        assert_eq!(app.draft(), "");
    }

    #[test]
    fn commit_blank_add_emits_nothing() {
        let mut app = three_tasks();
        app.update(Message::BeginAdd);
        app.update(Message::AddChar(' '));
        assert_eq!(app.update(Message::CommitAdd), vec![]);
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn cancel_add_discards_the_draft() {
        let mut app = three_tasks();
        app.update(Message::BeginAdd);
        app.update(Message::AddChar('x'));
        assert_eq!(app.update(Message::CancelAdd), vec![]);
        assert_eq!(app.mode(), Mode::Browse);
        assert_eq!(app.draft(), "");
    }

    #[test]
    fn set_tasks_clamps_selection() {
        let mut app = three_tasks();
        app.update(Message::SelectNext);
        app.update(Message::SelectNext);
        assert_eq!(app.selected(), 2);

        app.set_tasks(vec![task(1, "one")]);
        assert_eq!(app.selected(), 0);
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("one"));
    }

    #[test]
    fn quit_sets_should_quit() {
        let mut app = three_tasks();
        assert!(!app.should_quit());
        app.update(Message::Quit);
        assert!(app.should_quit());
    }

    #[test]
    fn view_next_day_advances_and_emits_reload() {
        let mut app = three_tasks();
        let effects = app.update(Message::ViewNextDay);
        assert_eq!(
            app.viewed(),
            NaiveDate::from_ymd_opt(2026, 10, 11).expect("valid date")
        );
        assert_eq!(effects, vec![Effect::Reload]);
    }

    #[test]
    fn view_prev_day_crosses_month_boundary() {
        let mut app = App::new(
            NaiveDate::from_ymd_opt(2026, 10, 1).expect("valid date"),
            vec![],
            vec![],
        );
        assert_eq!(app.update(Message::ViewPrevDay), vec![Effect::Reload]);
        assert_eq!(
            app.viewed(),
            NaiveDate::from_ymd_opt(2026, 9, 30).expect("valid date")
        );
    }

    #[test]
    fn view_next_day_crosses_month_boundary() {
        let mut app = App::new(
            NaiveDate::from_ymd_opt(2026, 10, 31).expect("valid date"),
            vec![],
            vec![],
        );
        assert_eq!(app.update(Message::ViewNextDay), vec![Effect::Reload]);
        assert_eq!(
            app.viewed(),
            NaiveDate::from_ymd_opt(2026, 11, 1).expect("valid date")
        );
    }

    #[test]
    fn view_today_returns_with_payload() {
        let mut app = three_tasks();
        app.update(Message::ViewNextDay);
        let today = NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date");
        assert_eq!(app.update(Message::ViewToday(today)), vec![Effect::Reload]);
        assert_eq!(app.viewed(), today);
    }

    #[test]
    fn navigation_syncs_cursor() {
        let mut app = three_tasks();
        app.update(Message::ViewNextDay);
        assert_eq!(app.cursor(), app.viewed());
    }

    #[test]
    fn focus_calendar_sets_cursor_to_viewed() {
        let mut app = three_tasks();
        app.update(Message::ViewNextDay);
        assert!(!app.calendar_focused());
        assert_eq!(app.update(Message::FocusCalendar), vec![]);
        assert!(app.calendar_focused());
        assert_eq!(app.cursor(), app.viewed());
    }

    #[test]
    fn move_cursor_steps_by_delta() {
        let mut app = three_tasks();
        app.update(Message::FocusCalendar);
        app.update(Message::MoveCursor(1));
        assert_eq!(
            app.cursor(),
            NaiveDate::from_ymd_opt(2026, 10, 11).expect("valid date")
        );
        app.update(Message::MoveCursor(-7));
        assert_eq!(
            app.cursor(),
            NaiveDate::from_ymd_opt(2026, 10, 4).expect("valid date")
        );
        assert_eq!(app.viewed(), day(), "cursor moves do not navigate");
    }

    #[test]
    fn move_cursor_ignored_when_list_focused() {
        let mut app = three_tasks();
        app.update(Message::MoveCursor(5));
        assert_eq!(app.cursor(), day());
    }

    #[test]
    fn jump_to_cursor_sets_viewed_and_emits_reload() {
        let mut app = three_tasks();
        app.update(Message::FocusCalendar);
        app.update(Message::MoveCursor(9));
        let effects = app.update(Message::JumpToCursor);
        assert_eq!(
            app.viewed(),
            NaiveDate::from_ymd_opt(2026, 10, 19).expect("valid date")
        );
        assert_eq!(effects, vec![Effect::Reload]);
        assert!(!app.calendar_focused(), "jump returns focus to the list");
    }

    #[test]
    fn jump_ignored_when_list_focused() {
        let mut app = three_tasks();
        assert_eq!(app.update(Message::JumpToCursor), vec![]);
        assert_eq!(app.viewed(), day());
    }

    #[test]
    fn commit_add_targets_viewed_day() {
        let past = NaiveDate::from_ymd_opt(2026, 10, 5).expect("valid date");
        let mut app = App::new(past, vec![], vec![]);
        app.update(Message::BeginAdd);
        for c in "past work".chars() {
            app.update(Message::AddChar(c));
        }
        let effects = app.update(Message::CommitAdd);
        assert_eq!(
            effects,
            vec![Effect::SaveNew(NewTask {
                day: past,
                title: "past work".to_string(),
                priority: Priority::Medium,
                notes: String::new(),
            })]
        );
    }
}
