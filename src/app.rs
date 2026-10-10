//! App state seam: pure state transitions, no terminal or database I/O.
//!
//! The event loop translates keys to [`Message`]. [`App::update`] evolves the
//! in-memory state and returns [`Effect`]s; the runtime performs each effect
//! against the [`Database`](crate::db::Database) and reloads the day list.
//! Tests assert on states and effects only.

use chrono::{Duration, NaiveDate};

use crate::model::{NewTask, Priority, Task, format_due_input, is_blank_title, parse_due_input};

/// Input mode: browsing the day list, composing a new task, or editing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Browse,
    Adding,
    Detail,
}

/// Field focused in the detail editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    Title,
    Priority,
    Notes,
    Due,
}

impl EditField {
    fn next(self) -> EditField {
        match self {
            EditField::Title => EditField::Priority,
            EditField::Priority => EditField::Notes,
            EditField::Notes => EditField::Due,
            EditField::Due => EditField::Title,
        }
    }

    fn prev(self) -> EditField {
        match self {
            EditField::Title => EditField::Due,
            EditField::Priority => EditField::Title,
            EditField::Notes => EditField::Priority,
            EditField::Due => EditField::Notes,
        }
    }
}

/// Section of the combined list holding a row.
/// Order matches the render: most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    MissedDue,
    DueToday,
    Overdue,
    Today,
}

/// In-progress edit of one task: drafts plus the snapshot they came from
/// (id, day, and done state carry over untouched on commit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorState {
    original: Task,
    title: String,
    priority: Priority,
    notes: String,
    due_text: String,
    field: EditField,
}

impl EditorState {
    fn new(task: &Task) -> EditorState {
        EditorState {
            original: task.clone(),
            title: task.title.clone(),
            priority: task.priority,
            notes: task.notes.clone(),
            due_text: task.due.as_ref().map(format_due_input).unwrap_or_default(),
            field: EditField::Title,
        }
    }

    /// The due draft parsed: `None` means the text is not a valid due
    /// moment (the commit path keeps the editor open to fix it).
    fn parsed_due(&self) -> Option<Option<chrono::NaiveDateTime>> {
        parse_due_input(&self.due_text)
    }

    fn has_changes(&self) -> bool {
        self.title.trim() != self.original.title
            || self.priority != self.original.priority
            || self.notes != self.original.notes
            || self.parsed_due() != Some(self.original.due)
    }

    /// Step the focused field; wrapping both directions.
    fn advance_field(&mut self, delta: i32) {
        self.field = if delta >= 0 {
            self.field.next()
        } else {
            self.field.prev()
        };
    }

    /// Step priority; no-op unless the priority field is focused.
    fn cycle_priority(&mut self, delta: i32) {
        if self.field == EditField::Priority {
            self.priority = self.priority.cycle(delta);
        }
    }

    fn updated_task(&self, due: Option<chrono::NaiveDateTime>) -> Task {
        Task {
            id: self.original.id,
            day: self.original.day,
            title: self.title.trim().to_string(),
            done: self.original.done,
            priority: self.priority,
            notes: self.notes.clone(),
            due,
            tracked_secs: self.original.tracked_secs,
            tags: self.original.tags.clone(),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Raw text of the due draft (`YYYY-MM-DD HH:MM`, blank for none).
    pub fn due_text(&self) -> &str {
        &self.due_text
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }

    pub fn notes(&self) -> &str {
        &self.notes
    }

    pub fn field(&self) -> EditField {
        self.field
    }
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
    /// Open the detail editor for the selected task.
    BeginEdit,
    /// Type into the focused editor field (title/notes; space cycles priority).
    EditChar(char),
    EditBackspace,
    /// Enter in the editor: newline in notes, commit elsewhere.
    PressEnter,
    NextField,
    PrevField,
    /// Step priority by delta; only applies while editing the priority field.
    CyclePriority(i32),
    /// Save the edit (only emits when drafts differ).
    CommitEdit,
    CancelEdit,
    /// Toggle the in-app keybinding help overlay.
    ToggleHelp,
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

/// In-memory state of the day-list view: today's tasks plus the derived
/// sections — Missed-due (open + due before now), Due-today (open + due
/// within today), and Overdue (open tasks from past days, still pinned to
/// their original day). Selection spans the combined list, most urgent
/// first: missed-due, due-today, overdue, then today.
#[derive(Debug)]
pub struct App {
    viewed: NaiveDate,
    tasks: Vec<Task>,
    overdue: Vec<Task>,
    due_today: Vec<Task>,
    missed_due: Vec<Task>,
    selected: usize,
    mode: Mode,
    draft: String,
    editor: Option<EditorState>,
    calendar_focused: bool,
    cursor: NaiveDate,
    help_open: bool,
    should_quit: bool,
}

impl App {
    /// Day list for `viewed`, freshly loaded with its overdue section.
    /// Due sections start empty; the runtime fills them via
    /// [`App::set_due_today`] and [`App::set_missed_due`] after loading.
    pub fn new(viewed: NaiveDate, tasks: Vec<Task>, overdue: Vec<Task>) -> App {
        let mut app = App {
            viewed,
            cursor: viewed,
            tasks: Vec::new(),
            overdue: Vec::new(),
            due_today: Vec::new(),
            missed_due: Vec::new(),
            selected: 0,
            mode: Mode::Browse,
            draft: String::new(),
            editor: None,
            calendar_focused: false,
            help_open: false,
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

    /// Open tasks due within today, oldest due first.
    pub fn due_today(&self) -> &[Task] {
        &self.due_today
    }

    /// Open tasks past due, oldest due first.
    pub fn missed_due(&self) -> &[Task] {
        &self.missed_due
    }

    /// Raw cursor over the combined list. Prefer the section cursors
    /// ([`App::overdue_cursor`], [`App::today_cursor`]) in new code;
    /// kept for tests asserting wrap-around positions.
    #[allow(dead_code)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Rows in the combined list (missed-due, due-today, overdue, today).
    fn combined_len(&self) -> usize {
        self.missed_due.len() + self.due_today.len() + self.overdue.len() + self.tasks.len()
    }

    /// Split a combined-list index into its section plus row.
    fn split_index(&self, index: usize) -> (Section, usize) {
        let mut rest = index;
        for (section, len) in [
            (Section::MissedDue, self.missed_due.len()),
            (Section::DueToday, self.due_today.len()),
            (Section::Overdue, self.overdue.len()),
            (Section::Today, self.tasks.len()),
        ] {
            if rest < len {
                return (section, rest);
            }
            rest -= len;
        }
        (Section::Today, rest)
    }

    /// Cursor row within the missed-due section, if it holds the selection.
    pub fn missed_due_cursor(&self) -> Option<usize> {
        match self.split_index(self.selected) {
            (Section::MissedDue, row) if row < self.missed_due.len() => Some(row),
            _ => None,
        }
    }

    /// Cursor row within the due-today section, if it holds the selection.
    pub fn due_today_cursor(&self) -> Option<usize> {
        match self.split_index(self.selected) {
            (Section::DueToday, row) if row < self.due_today.len() => Some(row),
            _ => None,
        }
    }

    /// Cursor row within the overdue section, if it holds the selection.
    pub fn overdue_cursor(&self) -> Option<usize> {
        match self.split_index(self.selected) {
            (Section::Overdue, row) if row < self.overdue.len() => Some(row),
            _ => None,
        }
    }

    /// Cursor row within today's list, if it holds the selection.
    pub fn today_cursor(&self) -> Option<usize> {
        match self.split_index(self.selected) {
            (Section::Today, row) if row < self.tasks.len() => Some(row),
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
            (Section::MissedDue, row) => self.missed_due.get(row),
            (Section::DueToday, row) => self.due_today.get(row),
            (Section::Overdue, row) => self.overdue.get(row),
            (Section::Today, row) => self.tasks.get(row),
        }
    }

    fn selected_task_mut(&mut self) -> Option<&mut Task> {
        match self.split_index(self.selected) {
            (Section::MissedDue, row) => self.missed_due.get_mut(row),
            (Section::DueToday, row) => self.due_today.get_mut(row),
            (Section::Overdue, row) => self.overdue.get_mut(row),
            (Section::Today, row) => self.tasks.get_mut(row),
        }
    }

    /// Mutable text of the focused editor field (title/notes/due); None when
    /// the editor is closed or the priority field is focused.
    fn focused_text_mut(&mut self) -> Option<&mut String> {
        match self.editor.as_mut() {
            Some(editor) if editor.field == EditField::Title => Some(&mut editor.title),
            Some(editor) if editor.field == EditField::Notes => Some(&mut editor.notes),
            Some(editor) if editor.field == EditField::Due => Some(&mut editor.due_text),
            _ => None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// The in-progress edit, if the detail editor is open.
    pub fn editor(&self) -> Option<&EditorState> {
        self.editor.as_ref()
    }

    /// Whether keyboard focus is in the calendar pane.
    pub fn calendar_focused(&self) -> bool {
        self.calendar_focused
    }

    /// The calendar cursor day.
    pub fn cursor(&self) -> NaiveDate {
        self.cursor
    }

    /// Whether the keybinding help overlay is open.
    pub fn help_open(&self) -> bool {
        self.help_open
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

    /// Replace the due-today section, keeping the selection valid.
    pub fn set_due_today(&mut self, due_today: Vec<Task>) {
        self.due_today = due_today;
        self.clamp_selection();
    }

    /// Replace the missed-due section, keeping the selection valid.
    pub fn set_missed_due(&mut self, missed_due: Vec<Task>) {
        self.missed_due = missed_due;
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

    /// Commit the open edit, emitting an update only when drafts differ.
    /// An emptied title discards the edit, mirroring blank add commits.
    /// A due draft that parses to nothing valid keeps the editor open so
    /// the typo can be fixed instead of saved.
    fn commit_edit(&mut self) -> Vec<Effect> {
        let due = match self.editor.as_ref().map(|editor| editor.parsed_due()) {
            Some(Some(due)) => due,
            Some(None) => return vec![],
            None => return vec![],
        };
        let Some(editor) = self.editor.take() else {
            return vec![];
        };
        self.set_mode(Mode::Browse);
        if is_blank_title(&editor.title) || !editor.has_changes() {
            return vec![];
        }
        vec![Effect::Update(editor.updated_task(due))]
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
                if is_blank_title(&title) {
                    vec![]
                } else {
                    vec![Effect::SaveNew(NewTask {
                        day: self.viewed,
                        title,
                        priority: Priority::Medium,
                        notes: String::new(),
                        due: None,
                        tags: Vec::new(),
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
            Message::BeginEdit => match self.selected_task().cloned() {
                Some(task) => {
                    self.editor = Some(EditorState::new(&task));
                    self.set_mode(Mode::Detail);
                    vec![]
                }
                None => vec![],
            },
            Message::EditChar(c) => {
                let in_priority = matches!(
                    self.editor.as_ref(),
                    Some(editor) if editor.field == EditField::Priority
                );
                if in_priority {
                    if c == ' '
                        && let Some(editor) = self.editor.as_mut()
                    {
                        editor.cycle_priority(1);
                    }
                } else if let Some(text) = self.focused_text_mut() {
                    text.push(c);
                }
                vec![]
            }
            Message::EditBackspace => {
                if let Some(text) = self.focused_text_mut() {
                    let _ = text.pop();
                }
                vec![]
            }
            Message::PressEnter => {
                let in_notes = matches!(self.editor.as_ref(), Some(editor) if editor.field == EditField::Notes);
                if in_notes {
                    self.focused_text_mut()
                        .expect("notes field holds text")
                        .push('\n');
                    vec![]
                } else {
                    self.commit_edit()
                }
            }
            Message::NextField => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.advance_field(1);
                }
                vec![]
            }
            Message::PrevField => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.advance_field(-1);
                }
                vec![]
            }
            Message::CyclePriority(delta) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.cycle_priority(delta);
                }
                vec![]
            }
            Message::CommitEdit => self.commit_edit(),
            Message::CancelEdit => {
                if self.editor.is_some() {
                    self.editor = None;
                    self.set_mode(Mode::Browse);
                }
                vec![]
            }
            Message::ToggleHelp => {
                self.help_open = !self.help_open;
                vec![]
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
            due: None,
            tracked_secs: 0,
            tags: Vec::new(),
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

    fn noon() -> chrono::NaiveDateTime {
        day().and_hms_opt(12, 0, 0).expect("valid datetime")
    }

    fn due_task(id: i64, title: &str) -> Task {
        Task {
            due: Some(noon()),
            ..task(id, title)
        }
    }

    fn with_due_sections() -> App {
        let mut app = App::new(day(), vec![task(1, "one")], vec![task(9, "old")]);
        app.set_missed_due(vec![due_task(7, "missed work")]);
        app.set_due_today(vec![due_task(8, "due work")]);
        app
    }

    fn focus_due(app: &mut App) {
        for _ in 0..3 {
            app.update(Message::NextField);
        }
        assert_eq!(app.editor().expect("editor open").field, EditField::Due);
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
                due: None,
                tags: Vec::new(),
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
                due: None,
                tags: Vec::new(),
            })]
        );
    }

    fn begin_detail(app: &mut App) {
        assert_eq!(app.update(Message::BeginEdit), vec![]);
        assert_eq!(app.mode(), Mode::Detail);
    }

    #[test]
    fn begin_edit_opens_detail_with_snapshot() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        let editor = app.editor().expect("editor open");
        assert_eq!(editor.title, "one");
        assert_eq!(editor.priority, Priority::Medium);
        assert_eq!(editor.notes, "");
        assert_eq!(editor.field, EditField::Title);
    }

    #[test]
    fn begin_edit_empty_list_is_noop() {
        let mut app = App::new(day(), vec![], vec![]);
        assert_eq!(app.update(Message::BeginEdit), vec![]);
        assert_eq!(app.mode(), Mode::Browse);
        assert_eq!(app.editor(), None);
    }

    #[test]
    fn typing_and_backspace_edit_title() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        app.update(Message::EditChar('!'));
        assert_eq!(app.editor().expect("editor open").title, "one!");
        app.update(Message::EditBackspace);
        app.update(Message::EditBackspace);
        assert_eq!(app.editor().expect("editor open").title, "on");
    }

    #[test]
    fn edit_messages_outside_detail_are_ignored() {
        let mut app = three_tasks();
        assert_eq!(app.update(Message::EditChar('x')), vec![]);
        assert_eq!(app.update(Message::CommitEdit), vec![]);
        assert_eq!(app.update(Message::CancelEdit), vec![]);
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn fields_cycle_forward_and_back() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        app.update(Message::NextField);
        assert_eq!(
            app.editor().expect("editor open").field,
            EditField::Priority
        );
        app.update(Message::NextField);
        assert_eq!(app.editor().expect("editor open").field, EditField::Notes);
        app.update(Message::NextField);
        assert_eq!(app.editor().expect("editor open").field, EditField::Due);
        app.update(Message::NextField);
        assert_eq!(app.editor().expect("editor open").field, EditField::Title);
        app.update(Message::PrevField);
        assert_eq!(app.editor().expect("editor open").field, EditField::Due);
    }

    #[test]
    fn priority_cycles_and_wraps() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        app.update(Message::NextField);
        app.update(Message::CyclePriority(1));
        assert_eq!(app.editor().expect("editor open").priority, Priority::High);
        app.update(Message::CyclePriority(1));
        assert_eq!(app.editor().expect("editor open").priority, Priority::Low);
        app.update(Message::CyclePriority(-1));
        assert_eq!(app.editor().expect("editor open").priority, Priority::High);
    }

    #[test]
    fn chars_ignored_in_priority_except_space() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        app.update(Message::NextField);
        app.update(Message::EditChar('a'));
        assert_eq!(
            app.editor().expect("editor open").priority,
            Priority::Medium
        );
        app.update(Message::EditChar(' '));
        assert_eq!(app.editor().expect("editor open").priority, Priority::High);
    }

    #[test]
    fn newline_in_notes_inserts_but_commits_in_title() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        app.update(Message::NextField);
        app.update(Message::NextField);
        app.update(Message::EditChar('a'));
        app.update(Message::PressEnter);
        app.update(Message::EditChar('b'));
        assert_eq!(app.editor().expect("editor open").notes, "a\nb");
        assert_eq!(app.mode(), Mode::Detail);

        let mut app = three_tasks();
        begin_detail(&mut app);
        let effects = app.update(Message::PressEnter);
        assert_eq!(effects, vec![], "unchanged title commits silently");
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn commit_unchanged_emits_nothing() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        assert_eq!(app.update(Message::CommitEdit), vec![]);
        assert_eq!(app.mode(), Mode::Browse);
        assert_eq!(app.editor(), None);
    }

    #[test]
    fn commit_with_emptied_title_discards() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        for _ in 0..3 {
            app.update(Message::EditBackspace);
        }
        assert_eq!(app.editor().expect("editor open").title, "");
        assert_eq!(app.update(Message::CommitEdit), vec![]);
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn commit_with_whitespace_only_change_emits_nothing() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        app.update(Message::EditChar(' '));
        assert_eq!(app.update(Message::CommitEdit), vec![]);
    }

    #[test]
    fn commit_changed_emits_update() {
        let mut app = three_tasks();
        app.update(Message::SelectNext);
        begin_detail(&mut app);
        for c in " edited".chars() {
            app.update(Message::EditChar(c));
        }
        app.update(Message::NextField);
        app.update(Message::CyclePriority(1));
        app.update(Message::NextField);
        for c in "some notes".chars() {
            app.update(Message::EditChar(c));
        }

        let effects = app.update(Message::CommitEdit);

        assert_eq!(
            effects,
            vec![Effect::Update(Task {
                id: 2,
                day: day(),
                title: "two edited".to_string(),
                done: false,
                priority: Priority::High,
                notes: "some notes".to_string(),
                due: None,
                tracked_secs: 0,
                tags: Vec::new(),
            })]
        );
        assert_eq!(app.mode(), Mode::Browse);
        assert_eq!(app.editor(), None);
    }

    #[test]
    fn cancel_discards_and_later_commit_is_noop() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        app.update(Message::EditChar('x'));
        assert_eq!(app.update(Message::CancelEdit), vec![]);
        assert_eq!(app.mode(), Mode::Browse);
        assert_eq!(app.editor(), None);
        assert_eq!(app.update(Message::CommitEdit), vec![]);
    }

    #[test]
    fn due_sections_are_exposed_separately() {
        let app = with_due_sections();
        assert_eq!(app.missed_due().len(), 1);
        assert_eq!(app.missed_due()[0].title, "missed work");
        assert_eq!(app.due_today().len(), 1);
        assert_eq!(app.due_today()[0].title, "due work");
        assert_eq!(app.overdue().len(), 1);
        assert_eq!(app.tasks().len(), 1);
    }

    #[test]
    fn selection_spans_missed_due_today_overdue() {
        let mut app = with_due_sections();
        assert_eq!(
            app.selected_task().map(|t| t.title.as_str()),
            Some("missed work"),
            "most urgent first"
        );
        app.update(Message::SelectNext);
        assert_eq!(
            app.selected_task().map(|t| t.title.as_str()),
            Some("due work")
        );
        assert_eq!(app.due_today_cursor(), Some(0));
        app.update(Message::SelectNext);
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("old"));
        assert_eq!(app.overdue_cursor(), Some(0));
        app.update(Message::SelectNext);
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("one"));
        assert_eq!(app.today_cursor(), Some(0));
        app.update(Message::SelectNext);
        assert_eq!(app.missed_due_cursor(), Some(0), "selection wraps");
    }

    #[test]
    fn toggle_due_task_flips_and_emits_update() {
        let mut app = with_due_sections();

        let effects = app.update(Message::ToggleSelected);

        assert!(app.missed_due()[0].done);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], Effect::Update(app.missed_due()[0].clone()));
    }

    #[test]
    fn completing_due_task_then_reloading_clears_the_sections() {
        let mut app = with_due_sections();
        // Complete the missed-due task, then the due-today task.
        app.update(Message::ToggleSelected);
        app.update(Message::SelectNext);
        app.update(Message::ToggleSelected);

        app.set_missed_due(vec![]);
        app.set_due_today(vec![]);

        assert!(app.missed_due().is_empty());
        assert!(app.due_today().is_empty());
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("one"));
    }

    #[test]
    fn begin_edit_shows_due_draft() {
        let mut app = with_due_sections();
        app.update(Message::SelectNext);
        assert_eq!(app.update(Message::BeginEdit), vec![]);
        let editor = app.editor().expect("editor open");
        assert_eq!(editor.due_text(), "2026-10-10 12:00");
    }

    #[test]
    fn typing_in_due_field_edits_the_draft() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        focus_due(&mut app);
        for c in "2026-10-11 09:00".chars() {
            app.update(Message::EditChar(c));
        }
        assert_eq!(
            app.editor().expect("editor open").due_text(),
            "2026-10-11 09:00"
        );
        app.update(Message::EditBackspace);
        assert_eq!(
            app.editor().expect("editor open").due_text(),
            "2026-10-11 09:0"
        );
    }

    #[test]
    fn commit_with_valid_due_sets_the_moment() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        focus_due(&mut app);
        for c in "2026-10-11 09:00".chars() {
            app.update(Message::EditChar(c));
        }

        let effects = app.update(Message::CommitEdit);

        let expected_due = NaiveDate::from_ymd_opt(2026, 10, 11)
            .expect("valid date")
            .and_hms_opt(9, 0, 0)
            .expect("valid datetime");
        assert_eq!(
            effects,
            vec![Effect::Update(Task {
                due: Some(expected_due),
                ..task(1, "one")
            })]
        );
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn commit_with_blank_due_clears_the_moment() {
        let mut app = with_due_sections();
        app.update(Message::SelectNext);
        begin_detail(&mut app);
        focus_due(&mut app);
        for _ in 0.."2026-10-10 12:00".len() {
            app.update(Message::EditBackspace);
        }
        assert_eq!(app.editor().expect("editor open").due_text(), "");

        let effects = app.update(Message::CommitEdit);

        assert_eq!(
            effects,
            vec![Effect::Update(Task {
                due: None,
                ..due_task(8, "due work")
            })]
        );
    }

    #[test]
    fn commit_with_invalid_due_keeps_the_editor_open() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        focus_due(&mut app);
        for c in "soon".chars() {
            app.update(Message::EditChar(c));
        }

        assert_eq!(app.update(Message::CommitEdit), vec![]);
        assert_eq!(app.mode(), Mode::Detail);
        assert!(app.editor().is_some(), "typo can be fixed instead of saved");
    }

    #[test]
    fn commit_unchanged_due_emits_nothing() {
        let mut app = with_due_sections();
        app.update(Message::SelectNext);
        begin_detail(&mut app);
        assert_eq!(app.update(Message::CommitEdit), vec![]);
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn help_overlay_toggles() {
        let mut app = three_tasks();
        assert!(!app.help_open());
        assert_eq!(app.update(Message::ToggleHelp), vec![]);
        assert!(app.help_open());
        assert_eq!(app.update(Message::ToggleHelp), vec![]);
        assert!(!app.help_open());
    }
}
