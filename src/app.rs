//! App state seam: pure state transitions, no terminal or database I/O.
//!
//! The event loop translates keys to [`Message`]. [`App::update`] evolves the
//! in-memory state and returns [`Effect`]s; the runtime performs each effect
//! against the [`Database`](crate::db::Database) and reloads the day list.
//! Tests assert on states and effects only.

use chrono::{Duration, NaiveDate, NaiveDateTime};

use crate::model::{
    NewTask, Priority, Task, format_due_input, format_tags_input, is_blank_title, parse_due_input,
    parse_tags_input,
};

/// Input mode: browsing the day list, composing a new task, editing one, or
/// searching across days. Search is a distinct view, not a day-scoped one:
/// while active it has two phases — typing the query, then navigating and
/// acting on the grouped results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Browse,
    Adding,
    Detail,
    Search,
}

/// Field focused in the detail editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    Title,
    Priority,
    Notes,
    Due,
    Tags,
}

impl EditField {
    fn next(self) -> EditField {
        match self {
            EditField::Title => EditField::Priority,
            EditField::Priority => EditField::Notes,
            EditField::Notes => EditField::Due,
            EditField::Due => EditField::Tags,
            EditField::Tags => EditField::Title,
        }
    }

    fn prev(self) -> EditField {
        match self {
            EditField::Title => EditField::Tags,
            EditField::Priority => EditField::Title,
            EditField::Notes => EditField::Priority,
            EditField::Due => EditField::Notes,
            EditField::Tags => EditField::Due,
        }
    }
}

/// The single in-memory running timer: which task, and the wall-clock moment
/// it started. Only accumulated seconds persist; the live segment lives here
/// and is banked on stop, on switch, and on quit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunningTimer {
    task_id: i64,
    started: NaiveDateTime,
}

/// Whole seconds between a timer start and `now`, never negative.
fn elapsed_secs(started: NaiveDateTime, now: NaiveDateTime) -> i64 {
    (now - started).num_seconds().max(0)
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

/// Single-level, in-memory undo buffer: the most recently deleted task and
/// where it was deleted from, so a restore puts it back where it was.
/// Replaced on the next delete, dropped when the session ends. Never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UndoEntry {
    task: Task,
    origin: UndoOrigin,
}

/// Where a deleted task was showing: a day-list section, or the cross-day
/// Search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UndoOrigin {
    Section(Section),
    Search,
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
    tags_text: String,
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
            tags_text: format_tags_input(&task.tags),
            field: EditField::Title,
        }
    }

    /// The due draft parsed: `None` means the text is not a valid due
    /// moment (the commit path keeps the editor open to fix it).
    fn parsed_due(&self) -> Option<Option<chrono::NaiveDateTime>> {
        parse_due_input(&self.due_text)
    }

    /// The tags draft parsed (comma-separated, normalized; never fails).
    fn parsed_tags(&self) -> Vec<String> {
        parse_tags_input(&self.tags_text)
    }

    fn has_changes(&self) -> bool {
        self.title.trim() != self.original.title
            || self.priority != self.original.priority
            || self.notes != self.original.notes
            || self.parsed_due() != Some(self.original.due)
            || self.parsed_tags() != self.original.tags
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

    fn updated_task(&self, due: Option<chrono::NaiveDateTime>, tags: Vec<String>) -> Task {
        Task {
            id: self.original.id,
            day: self.original.day,
            title: self.title.trim().to_string(),
            done: self.original.done,
            priority: self.priority,
            notes: self.notes.clone(),
            due,
            tracked_secs: self.original.tracked_secs,
            tags,
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Raw text of the due draft (`YYYY-MM-DD HH:MM`, blank for none).
    pub fn due_text(&self) -> &str {
        &self.due_text
    }

    /// Raw text of the tags draft (`work, admin`, blank for none).
    pub fn tags_text(&self) -> &str {
        &self.tags_text
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

    /// The task snapshot the edit was opened from (id, day, done, and the
    /// stored tracked total).
    pub fn original(&self) -> &Task {
        &self.original
    }
}

/// Every event the app reacts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    SelectNext,
    SelectPrev,
    ToggleSelected,
    /// Delete the selected task (`d`); the removed task is held for undo.
    /// A no-op with nothing selected.
    DeleteSelected,
    /// Restore the most recently deleted task (`u`). A no-op with nothing
    /// to restore — never an error.
    UndoDelete,
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
    /// Cycle the tag filter to the next tag in use (`f`); wraps around.
    /// No-op when nothing is tagged.
    CycleTagFilter,
    /// Clear the tag filter and restore the full list (`Esc`).
    ClearTagFilter,
    /// Open cross-day Search (`/`): a fresh, empty query with the query
    /// field focused. Search is not tag-scoped, so any tag filter clears.
    BeginSearch,
    /// Focus the Search query field again (`/` while navigating results).
    RefocusSearch,
    /// Type into the Search query (narrows the results live).
    SearchChar(char),
    /// Delete the last character of the Search query.
    SearchBackspace,
    /// Finish typing the query and start navigating the results (Enter).
    /// A no-op while the query is blank, so there is never a half-state.
    SearchSubmit,
    /// Leave Search for the Day list and clear the query (`Esc`).
    ExitSearch,
    /// Move the selection down the Search results.
    SearchSelectNext,
    /// Move the selection up the Search results.
    SearchSelectPrev,
    /// Toggle the work timer on the selected task (`w`). `now` rides along so
    /// pure state never reads a clock. Starting while another task runs banks
    /// the running task's elapsed seconds first (single running timer).
    ToggleTimer(NaiveDateTime),
    /// Quit, banking any running timer's elapsed seconds first.
    Quit(NaiveDateTime),
}

/// Persistence work the runtime must perform after an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    SaveNew(NewTask),
    Update(Task),
    /// Bank tracked seconds on a task (timer stop/switch/quit).
    AddTrackedSecs {
        task_id: i64,
        delta_secs: i64,
    },
    /// Delete a task by id.
    Delete {
        task_id: i64,
    },
    /// Re-insert a previously deleted task exactly as it was (undo).
    Restore(Task),
    /// Reload the viewed day's list from storage (after navigation).
    Reload,
}

/// In-memory state of the day-list view: today's tasks plus the derived
/// sections — Missed-due (open + due before now), Due-today (open + due
/// within today), and Overdue (open tasks from past days, still pinned to
/// their original day). Selection spans the combined list, most urgent
/// first: missed-due, due-today, overdue, then today. An optional tag
/// filter narrows every section to one tag; clearing restores the full
/// lists, which are always kept intact underneath.
#[derive(Debug)]
pub struct App {
    viewed: NaiveDate,
    tasks: Vec<Task>,
    overdue: Vec<Task>,
    due_today: Vec<Task>,
    missed_due: Vec<Task>,
    tag_filter: Option<String>,
    all_tags: Vec<String>,
    selected: usize,
    mode: Mode,
    draft: String,
    editor: Option<EditorState>,
    running: Option<RunningTimer>,
    undo: Option<UndoEntry>,
    calendar_focused: bool,
    cursor: NaiveDate,
    help_open: bool,
    should_quit: bool,
    /// Cross-day Search state: the query, whether the query field is focused,
    /// the results (ordered by day then id), and the selection over them.
    search_query: String,
    search_editing: bool,
    search_results: Vec<Task>,
    search_selected: usize,
    /// Mode to restore when the detail editor closes: Browse from the Day
    /// list, Search from the results.
    editor_return: Mode,
}

impl App {
    /// Day list for `viewed`, freshly loaded with its overdue section.
    /// Due sections start empty; the runtime fills them via
    /// [`App::set_due_today`] and [`App::set_missed_due`] after loading.
    /// The tag filter starts cleared.
    pub fn new(viewed: NaiveDate, tasks: Vec<Task>, overdue: Vec<Task>) -> App {
        let mut app = App {
            viewed,
            cursor: viewed,
            tasks: Vec::new(),
            overdue: Vec::new(),
            due_today: Vec::new(),
            missed_due: Vec::new(),
            tag_filter: None,
            all_tags: Vec::new(),
            selected: 0,
            mode: Mode::Browse,
            draft: String::new(),
            editor: None,
            running: None,
            undo: None,
            calendar_focused: false,
            help_open: false,
            should_quit: false,
            search_query: String::new(),
            search_editing: false,
            search_results: Vec::new(),
            search_selected: 0,
            editor_return: Mode::Browse,
        };
        app.set_tasks(tasks);
        app.set_overdue(overdue);
        app
    }

    pub fn viewed(&self) -> NaiveDate {
        self.viewed
    }

    /// Active tag filter, if focusing on one tag.
    pub fn tag_filter(&self) -> Option<&str> {
        self.tag_filter.as_deref()
    }

    /// Whether a task passes the active filter (everything passes unfiltered).
    fn matches_filter(&self, task: &Task) -> bool {
        match &self.tag_filter {
            None => true,
            Some(tag) => task.tags.iter().any(|t| t == tag),
        }
    }

    /// Raw (unfiltered) rows of one section.
    fn section_tasks(&self, section: Section) -> &[Task] {
        match section {
            Section::MissedDue => &self.missed_due,
            Section::DueToday => &self.due_today,
            Section::Overdue => &self.overdue,
            Section::Today => &self.tasks,
        }
    }

    /// Mutable raw rows of one section (delete/restore).
    fn section_tasks_mut(&mut self, section: Section) -> &mut Vec<Task> {
        match section {
            Section::MissedDue => &mut self.missed_due,
            Section::DueToday => &mut self.due_today,
            Section::Overdue => &mut self.overdue,
            Section::Today => &mut self.tasks,
        }
    }

    /// Filtered rows of one section, in stored order.
    fn filtered_in(&self, section: Section) -> Vec<&Task> {
        self.section_tasks(section)
            .iter()
            .filter(|task| self.matches_filter(task))
            .collect()
    }

    /// Filtered rows of one section, owned (for rendering).
    fn visible_in(&self, section: Section) -> Vec<Task> {
        self.filtered_in(section).into_iter().cloned().collect()
    }

    /// Visible missed-due rows under the active filter.
    pub fn visible_missed_due(&self) -> Vec<Task> {
        self.visible_in(Section::MissedDue)
    }

    /// Visible due-today rows under the active filter.
    pub fn visible_due_today(&self) -> Vec<Task> {
        self.visible_in(Section::DueToday)
    }

    /// Visible overdue rows under the active filter.
    pub fn visible_overdue(&self) -> Vec<Task> {
        self.visible_in(Section::Overdue)
    }

    /// Visible today rows under the active filter.
    pub fn visible_tasks(&self) -> Vec<Task> {
        self.visible_in(Section::Today)
    }

    /// Raw cursor over the combined list. Prefer the section cursors
    /// ([`App::overdue_cursor`], [`App::today_cursor`]) in new code;
    /// kept for tests asserting wrap-around positions.
    #[allow(dead_code)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Cursor row within a section of the visible (filtered) combined
    /// list, if that section holds the selection.
    fn cursor_in(&self, section: Section) -> Option<usize> {
        match self.split_index(self.selected) {
            (found, row) if found == section && row < self.filtered_in(section).len() => Some(row),
            _ => None,
        }
    }

    /// Rows in the visible (filtered) combined list
    /// (missed-due, due-today, overdue, today).
    fn combined_len(&self) -> usize {
        [
            Section::MissedDue,
            Section::DueToday,
            Section::Overdue,
            Section::Today,
        ]
        .into_iter()
        .map(|section| self.filtered_in(section).len())
        .sum()
    }

    /// Split a visible-list index into its section plus visible row.
    fn split_index(&self, index: usize) -> (Section, usize) {
        let mut rest = index;
        for section in [
            Section::MissedDue,
            Section::DueToday,
            Section::Overdue,
            Section::Today,
        ] {
            let len = self.filtered_in(section).len();
            if rest < len {
                return (section, rest);
            }
            rest -= len;
        }
        (Section::Today, rest)
    }

    /// Cursor row within the missed-due section, if it holds the selection.
    pub fn missed_due_cursor(&self) -> Option<usize> {
        self.cursor_in(Section::MissedDue)
    }

    /// Cursor row within the due-today section, if it holds the selection.
    pub fn due_today_cursor(&self) -> Option<usize> {
        self.cursor_in(Section::DueToday)
    }

    /// Cursor row within the overdue section, if it holds the selection.
    pub fn overdue_cursor(&self) -> Option<usize> {
        self.cursor_in(Section::Overdue)
    }

    /// Cursor row within today's list, if it holds the selection.
    pub fn today_cursor(&self) -> Option<usize> {
        self.cursor_in(Section::Today)
    }

    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.combined_len().saturating_sub(1));
    }

    /// Task under the selection cursor in the active view, if any: the
    /// selected Search result while Search is active, else the selected row
    /// of the visible (filtered) combined day list.
    pub fn selected_task(&self) -> Option<&Task> {
        if self.mode == Mode::Search {
            self.search_results.get(self.search_selected)
        } else {
            self.day_selected_task()
        }
    }

    /// Day-list selection cursor resolution (Search has its own indexing).
    fn day_selected_task(&self) -> Option<&Task> {
        let (section, row) = self.split_index(self.selected);
        self.filtered_in(section).into_iter().nth(row)
    }

    fn selected_task_mut(&mut self) -> Option<&mut Task> {
        if self.mode == Mode::Search {
            return self.search_results.get_mut(self.search_selected);
        }
        let id = self.day_selected_task().map(|task| task.id)?;
        [
            &mut self.missed_due,
            &mut self.due_today,
            &mut self.overdue,
            &mut self.tasks,
        ]
        .into_iter()
        .flatten()
        .find(|task| task.id == id)
    }

    /// Mutable text of the focused editor field (title/notes/due/tags);
    /// None when the editor is closed or the priority field is focused.
    fn focused_text_mut(&mut self) -> Option<&mut String> {
        match self.editor.as_mut() {
            Some(editor) if editor.field == EditField::Title => Some(&mut editor.title),
            Some(editor) if editor.field == EditField::Notes => Some(&mut editor.notes),
            Some(editor) if editor.field == EditField::Due => Some(&mut editor.due_text),
            Some(editor) if editor.field == EditField::Tags => Some(&mut editor.tags_text),
            _ => None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The active Search query (`""` when not searching).
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Whether Search is active and the query field holds focus (typing
    /// narrows the results); `false` means the results are navigable.
    pub fn search_editing(&self) -> bool {
        self.search_editing
    }

    /// The cross-day Search results, ordered by day then id.
    pub fn search_results(&self) -> &[Task] {
        &self.search_results
    }

    /// Selection index over the Search results.
    #[allow(dead_code)]
    pub fn search_selected(&self) -> usize {
        self.search_selected
    }

    /// Cursor row within the Search group for `day`, if the selection points
    /// into it.
    pub fn search_cursor_in(&self, day: NaiveDate) -> Option<usize> {
        let offset = self
            .search_results
            .iter()
            .filter(|task| task.day < day)
            .count();
        let in_day = self
            .search_results
            .iter()
            .filter(|task| task.day == day)
            .count();
        if in_day == 0 {
            return None;
        }
        let local = self.search_selected.checked_sub(offset)?;
        (local < in_day).then_some(local)
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

    /// Id of the task currently being timed, if any.
    pub fn running_task_id(&self) -> Option<i64> {
        self.running.as_ref().map(|running| running.task_id)
    }

    /// Tracked seconds to display for `task`: its stored total plus, when it
    /// is the running task, the live elapsed segment. Never negative.
    pub fn tracked_display(&self, task: &Task, now: NaiveDateTime) -> i64 {
        let mut secs = task.tracked_secs.max(0);
        if let Some(running) = &self.running
            && running.task_id == task.id
        {
            secs += elapsed_secs(running.started, now);
        }
        secs
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

    /// Replace the Search results (storage order: day then id), keeping the
    /// Search selection on a valid row.
    pub fn set_search_results(&mut self, results: Vec<Task>) {
        self.search_results = results;
        self.clamp_search_selection();
    }

    fn clamp_search_selection(&mut self) {
        self.search_selected = self
            .search_selected
            .min(self.search_results.len().saturating_sub(1));
    }

    /// Move the Search selection by `step`, wrapping over the results.
    fn move_search_selection(&mut self, step: isize) {
        if self.search_results.is_empty() {
            return;
        }
        let len = self.search_results.len() as isize;
        self.search_selected = (self.search_selected as isize + step).rem_euclid(len) as usize;
    }

    /// Replace the known tag names (the filter cycle order). A filter
    /// pointing at a vanished tag clears, so focus mode never strands the
    /// list on an empty tag.
    pub fn set_all_tags(&mut self, tags: Vec<String>) {
        self.all_tags = tags;
        if let Some(filter) = &self.tag_filter
            && !self.all_tags.iter().any(|tag| tag == filter)
        {
            self.tag_filter = None;
        }
        self.clamp_selection();
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.draft.clear();
    }

    /// Close the detail editor and return to the view it was opened from:
    /// the Day list, or the Search results (in result-navigation phase).
    fn finish_edit(&mut self) {
        let target = self.editor_return;
        if target == Mode::Search {
            self.search_editing = false;
        }
        self.set_mode(target);
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
        self.finish_edit();
        if is_blank_title(&editor.title) || !editor.has_changes() {
            return vec![];
        }
        vec![Effect::Update(
            editor.updated_task(due, editor.parsed_tags()),
        )]
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

    /// Stop any running timer, emitting an effect that banks its elapsed
    /// seconds. Idempotent: stopping when nothing runs emits nothing.
    fn bank_running_timer(&mut self, now: NaiveDateTime) -> Vec<Effect> {
        match self.running.take() {
            Some(running) => vec![Effect::AddTrackedSecs {
                task_id: running.task_id,
                delta_secs: elapsed_secs(running.started, now),
            }],
            None => vec![],
        }
    }

    /// Toggle timing on the selected task: stop if it is the running one;
    /// otherwise bank the previous task's elapsed seconds and start here.
    fn toggle_timer(&mut self, now: NaiveDateTime) -> Vec<Effect> {
        let Some(selected_id) = self.selected_task().map(|task| task.id) else {
            return vec![];
        };
        match self.running.take() {
            Some(running) if running.task_id == selected_id => vec![Effect::AddTrackedSecs {
                task_id: selected_id,
                delta_secs: elapsed_secs(running.started, now),
            }],
            Some(running) => {
                let banked = Effect::AddTrackedSecs {
                    task_id: running.task_id,
                    delta_secs: elapsed_secs(running.started, now),
                };
                self.running = Some(RunningTimer {
                    task_id: selected_id,
                    started: now,
                });
                vec![banked]
            }
            None => {
                self.running = Some(RunningTimer {
                    task_id: selected_id,
                    started: now,
                });
                vec![]
            }
        }
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
            Message::DeleteSelected => {
                if self.mode == Mode::Search {
                    if self.search_selected >= self.search_results.len() {
                        return vec![];
                    }
                    let task = self.search_results.remove(self.search_selected);
                    let task_id = task.id;
                    self.undo = Some(UndoEntry {
                        task,
                        origin: UndoOrigin::Search,
                    });
                    self.clamp_search_selection();
                    return vec![Effect::Delete { task_id }];
                }
                let Some(task) = self.selected_task().cloned() else {
                    return vec![];
                };
                let (section, _) = self.split_index(self.selected);
                let task_id = task.id;
                self.section_tasks_mut(section).retain(|t| t.id != task_id);
                self.undo = Some(UndoEntry {
                    task,
                    origin: UndoOrigin::Section(section),
                });
                self.clamp_selection();
                vec![Effect::Delete { task_id }]
            }
            Message::UndoDelete => match self.undo.take() {
                Some(entry) => {
                    match entry.origin {
                        UndoOrigin::Section(section) => {
                            self.section_tasks_mut(section).push(entry.task.clone())
                        }
                        UndoOrigin::Search => self.search_results.push(entry.task.clone()),
                    }
                    self.clamp_selection();
                    self.clamp_search_selection();
                    vec![Effect::Restore(entry.task)]
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
                    self.editor_return = self.mode;
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
                    self.finish_edit();
                }
                vec![]
            }
            Message::ToggleHelp => {
                self.help_open = !self.help_open;
                vec![]
            }
            Message::CycleTagFilter => {
                if self.all_tags.is_empty() {
                    self.tag_filter = None;
                } else {
                    let next = self.tag_filter.as_ref().and_then(|current| {
                        self.all_tags
                            .iter()
                            .skip_while(|tag| *tag != current)
                            .nth(1)
                            .cloned()
                    });
                    self.tag_filter = Some(next.unwrap_or_else(|| self.all_tags[0].clone()));
                }
                self.clamp_selection();
                vec![]
            }
            Message::ClearTagFilter => {
                self.tag_filter = None;
                self.clamp_selection();
                vec![]
            }
            Message::BeginSearch => {
                self.mode = Mode::Search;
                self.search_query.clear();
                self.search_editing = true;
                self.search_results.clear();
                self.search_selected = 0;
                self.calendar_focused = false;
                // Search is deliberately not day- or tag-scoped.
                self.tag_filter = None;
                vec![]
            }
            Message::RefocusSearch => {
                if self.mode == Mode::Search {
                    self.search_editing = true;
                }
                vec![]
            }
            Message::SearchChar(c) => {
                if self.mode == Mode::Search && self.search_editing {
                    self.search_query.push(c);
                    self.search_selected = 0;
                    return vec![Effect::Reload];
                }
                vec![]
            }
            Message::SearchBackspace => {
                if self.mode == Mode::Search && self.search_editing {
                    self.search_query.pop();
                    self.search_selected = 0;
                    return vec![Effect::Reload];
                }
                vec![]
            }
            Message::SearchSubmit => {
                if self.mode == Mode::Search
                    && self.search_editing
                    && !self.search_query.trim().is_empty()
                {
                    self.search_editing = false;
                    self.search_selected = 0;
                }
                vec![]
            }
            Message::ExitSearch => {
                if self.mode == Mode::Search {
                    self.mode = Mode::Browse;
                    self.search_query.clear();
                    self.search_results.clear();
                    self.search_editing = false;
                    self.search_selected = 0;
                }
                vec![]
            }
            Message::SearchSelectNext => {
                self.move_search_selection(1);
                vec![]
            }
            Message::SearchSelectPrev => {
                self.move_search_selection(-1);
                vec![]
            }
            Message::ToggleTimer(now) => self.toggle_timer(now),
            Message::Quit(now) => {
                let effects = self.bank_running_timer(now);
                self.should_quit = true;
                effects
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

    fn focus_tags(app: &mut App) {
        for _ in 0..4 {
            app.update(Message::NextField);
        }
        assert_eq!(app.editor().expect("editor open").field, EditField::Tags);
    }

    fn tagged_task(id: i64, title: &str, tags: &[&str]) -> Task {
        Task {
            tags: tags.iter().map(|t| t.to_string()).collect(),
            ..task(id, title)
        }
    }

    fn with_tags() -> App {
        let mut app = App::new(
            day(),
            vec![
                tagged_task(1, "one", &["work"]),
                tagged_task(2, "two", &["admin"]),
                task(3, "three"),
            ],
            vec![tagged_task(9, "old", &["work"])],
        );
        app.set_all_tags(vec!["admin".to_string(), "work".to_string()]);
        app
    }

    #[test]
    fn overdue_section_is_exposed_separately() {
        let app = with_overdue();
        assert_eq!(app.visible_overdue().len(), 1);
        assert_eq!(app.visible_overdue()[0].title, "old unfinished");
        assert_eq!(app.visible_tasks().len(), 2);
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

        assert!(app.visible_overdue()[0].done);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], Effect::Update(app.visible_overdue()[0].clone()));
    }

    #[test]
    fn completing_overdue_then_reloading_clears_the_section() {
        let mut app = with_overdue();
        app.update(Message::ToggleSelected);

        app.set_overdue(vec![]);
        app.set_tasks(app.visible_tasks());

        assert!(app.visible_overdue().is_empty());
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
    fn delete_removes_the_selected_task_and_emits_delete() {
        let mut app = three_tasks();
        app.update(Message::SelectNext);
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("two"));

        let effects = app.update(Message::DeleteSelected);

        assert_eq!(effects, vec![Effect::Delete { task_id: 2 }]);
        assert_eq!(app.visible_tasks().len(), 2);
        assert!(
            app.visible_tasks().iter().all(|t| t.id != 2),
            "deleted task leaves the list immediately"
        );
        assert_eq!(
            app.selected_task().map(|t| t.title.as_str()),
            Some("three"),
            "selection stays on a valid row"
        );
    }

    #[test]
    fn delete_on_empty_list_emits_nothing() {
        let mut app = App::new(day(), vec![], vec![]);
        assert_eq!(app.update(Message::DeleteSelected), vec![]);
    }

    #[test]
    fn undo_restores_the_deleted_task_with_every_field() {
        let mut original = tagged_task(2, "two", &["work"]);
        original.due = Some(noon());
        original.notes = "the body".to_string();
        original.tracked_secs = 90;
        let mut app = App::new(day(), vec![task(1, "one"), original.clone()], vec![]);
        app.update(Message::SelectNext);
        app.update(Message::DeleteSelected);

        let effects = app.update(Message::UndoDelete);

        assert_eq!(effects, vec![Effect::Restore(original.clone())]);
        assert!(
            app.visible_tasks().contains(&original),
            "restored task is back in the list with its fields intact"
        );
    }

    #[test]
    fn undo_with_nothing_to_restore_is_a_noop() {
        let mut app = three_tasks();
        assert_eq!(app.update(Message::UndoDelete), vec![]);
    }

    #[test]
    fn a_second_delete_replaces_the_undo_buffer() {
        let mut app = three_tasks();
        app.update(Message::DeleteSelected);
        app.update(Message::DeleteSelected);

        let effects = app.update(Message::UndoDelete);

        assert_eq!(
            effects,
            vec![Effect::Restore(task(2, "two"))],
            "undo holds only the most recent delete"
        );
        assert!(
            app.visible_tasks().iter().all(|t| t.id != 1),
            "the first delete is not resurrected"
        );
    }

    #[test]
    fn undo_restores_into_the_section_it_came_from() {
        let mut app = with_overdue();
        app.update(Message::DeleteSelected);
        assert!(app.visible_overdue().is_empty());

        app.update(Message::UndoDelete);

        assert_eq!(app.visible_overdue().len(), 1);
        assert_eq!(app.visible_overdue()[0].title, "old unfinished");
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
        app.update(Message::Quit(noon()));
        assert!(app.should_quit());
    }

    #[test]
    fn timer_starts_and_emits_no_effect() {
        let mut app = three_tasks();
        assert_eq!(app.update(Message::ToggleTimer(noon())), vec![]);
        assert_eq!(app.running_task_id(), Some(1));
    }

    #[test]
    fn timer_on_empty_list_is_noop() {
        let mut app = App::new(day(), vec![], vec![]);
        assert_eq!(app.update(Message::ToggleTimer(noon())), vec![]);
        assert_eq!(app.running_task_id(), None);
    }

    #[test]
    fn timer_stop_banks_elapsed_seconds() {
        let mut app = three_tasks();
        app.update(Message::ToggleTimer(noon()));
        let later = noon() + Duration::seconds(90);

        let effects = app.update(Message::ToggleTimer(later));

        assert_eq!(app.running_task_id(), None);
        assert_eq!(
            effects,
            vec![Effect::AddTrackedSecs {
                task_id: 1,
                delta_secs: 90,
            }]
        );
    }

    #[test]
    fn timer_switch_banks_previous_and_starts_new() {
        let mut app = three_tasks();
        app.update(Message::ToggleTimer(noon()));
        app.update(Message::SelectNext);
        let later = noon() + Duration::seconds(30);

        let effects = app.update(Message::ToggleTimer(later));

        assert_eq!(app.running_task_id(), Some(2));
        assert_eq!(
            effects,
            vec![Effect::AddTrackedSecs {
                task_id: 1,
                delta_secs: 30,
            }]
        );
    }

    #[test]
    fn timer_display_adds_live_elapsed_to_base_total() {
        let mut app = three_tasks();
        let mut stored = task(1, "one");
        stored.tracked_secs = 60;
        app.update(Message::ToggleTimer(noon()));
        let later = noon() + Duration::seconds(45);

        assert_eq!(app.tracked_display(&stored, later), 105);
    }

    #[test]
    fn tracked_display_uses_stored_total_when_not_running() {
        let app = three_tasks();
        let mut stored = task(1, "one");
        stored.tracked_secs = 120;
        assert_eq!(app.tracked_display(&stored, noon()), 120);
    }

    #[test]
    fn quit_banks_running_timer() {
        let mut app = three_tasks();
        app.update(Message::ToggleTimer(noon()));
        let later = noon() + Duration::seconds(15);

        let effects = app.update(Message::Quit(later));

        assert!(app.should_quit());
        assert_eq!(app.running_task_id(), None);
        assert_eq!(
            effects,
            vec![Effect::AddTrackedSecs {
                task_id: 1,
                delta_secs: 15,
            }]
        );
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
        assert_eq!(app.editor().expect("editor open").field, EditField::Tags);
        app.update(Message::NextField);
        assert_eq!(app.editor().expect("editor open").field, EditField::Title);
        app.update(Message::PrevField);
        assert_eq!(app.editor().expect("editor open").field, EditField::Tags);
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
        assert_eq!(app.visible_missed_due().len(), 1);
        assert_eq!(app.visible_missed_due()[0].title, "missed work");
        assert_eq!(app.visible_due_today().len(), 1);
        assert_eq!(app.visible_due_today()[0].title, "due work");
        assert_eq!(app.visible_overdue().len(), 1);
        assert_eq!(app.visible_tasks().len(), 1);
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

        assert!(app.visible_missed_due()[0].done);
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0],
            Effect::Update(app.visible_missed_due()[0].clone())
        );
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

        assert!(app.visible_missed_due().is_empty());
        assert!(app.visible_due_today().is_empty());
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
    fn begin_edit_shows_tags_draft() {
        let mut app = with_tags();
        assert_eq!(app.update(Message::BeginEdit), vec![]);
        // Selection starts on the overdue "old" task tagged work.
        assert_eq!(app.editor().expect("editor open").tags_text(), "work");
    }

    #[test]
    fn typing_in_tags_field_edits_the_draft() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        focus_tags(&mut app);
        for c in "work, admin".chars() {
            app.update(Message::EditChar(c));
        }
        assert_eq!(
            app.editor().expect("editor open").tags_text(),
            "work, admin"
        );
        app.update(Message::EditBackspace);
        assert_eq!(app.editor().expect("editor open").tags_text(), "work, admi");
    }

    #[test]
    fn commit_with_tags_attaches_them_normalized() {
        let mut app = three_tasks();
        begin_detail(&mut app);
        focus_tags(&mut app);
        for c in " Work ,, ADMIN ".chars() {
            app.update(Message::EditChar(c));
        }

        let effects = app.update(Message::CommitEdit);

        assert_eq!(
            effects,
            vec![Effect::Update(Task {
                tags: vec!["admin".to_string(), "work".to_string()],
                ..task(1, "one")
            })]
        );
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn commit_with_blank_tags_clears_them() {
        let mut app = with_tags();
        begin_detail(&mut app);
        focus_tags(&mut app);
        for _ in 0.."work".len() {
            app.update(Message::EditBackspace);
        }
        assert_eq!(app.editor().expect("editor open").tags_text(), "");

        let effects = app.update(Message::CommitEdit);

        assert_eq!(
            effects,
            vec![Effect::Update(Task {
                tags: Vec::new(),
                ..tagged_task(9, "old", &["work"])
            })]
        );
    }

    #[test]
    fn filter_cycles_through_tags_and_wraps() {
        let mut app = with_tags();
        assert_eq!(app.tag_filter(), None);
        assert_eq!(app.update(Message::CycleTagFilter), vec![]);
        assert_eq!(app.tag_filter(), Some("admin"));
        assert_eq!(app.update(Message::CycleTagFilter), vec![]);
        assert_eq!(app.tag_filter(), Some("work"));
        assert_eq!(app.update(Message::CycleTagFilter), vec![]);
        assert_eq!(app.tag_filter(), Some("admin"), "cycle wraps around");
    }

    #[test]
    fn filter_with_no_tags_is_noop() {
        let mut app = three_tasks();
        assert_eq!(app.update(Message::CycleTagFilter), vec![]);
        assert_eq!(app.tag_filter(), None);
        assert_eq!(app.update(Message::ClearTagFilter), vec![]);
        assert_eq!(app.tag_filter(), None);
    }

    #[test]
    fn filter_narrows_every_section_and_clear_restores() {
        let mut app = with_tags();
        app.update(Message::CycleTagFilter);
        assert_eq!(app.tag_filter(), Some("admin"));

        assert_eq!(app.visible_tasks().len(), 1);
        assert_eq!(app.visible_tasks()[0].title, "two");
        assert!(app.visible_overdue().is_empty());
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("two"));

        assert_eq!(app.update(Message::ClearTagFilter), vec![]);
        assert_eq!(app.tag_filter(), None);
        assert_eq!(app.visible_tasks().len(), 3);
        assert_eq!(app.visible_overdue().len(), 1);
    }

    #[test]
    fn filter_clamps_selection_into_view() {
        let mut app = with_tags();
        app.update(Message::SelectNext);
        app.update(Message::SelectNext);
        app.update(Message::SelectNext);
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("three"));

        app.update(Message::CycleTagFilter);
        assert_eq!(app.selected_task().map(|t| t.title.as_str()), Some("two"));
    }

    #[test]
    fn set_all_tags_prunes_a_vanished_filter() {
        let mut app = with_tags();
        app.update(Message::CycleTagFilter);
        app.update(Message::CycleTagFilter);
        assert_eq!(app.tag_filter(), Some("work"));

        app.set_all_tags(vec!["admin".to_string()]);
        assert_eq!(app.tag_filter(), None);
        assert_eq!(app.visible_tasks().len(), 3);
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

    fn search_app() -> App {
        let mut app = three_tasks();
        app.update(Message::BeginSearch);
        app.set_search_results(vec![task(1, "one"), task(2, "two"), task(3, "three")]);
        app
    }

    #[test]
    fn begin_search_enters_search_with_an_empty_query() {
        let mut app = three_tasks();
        assert_eq!(app.update(Message::BeginSearch), vec![]);
        assert_eq!(app.mode(), Mode::Search);
        assert_eq!(app.search_query(), "");
        assert!(app.search_editing(), "the query field is focused first");
        assert!(!app.calendar_focused(), "search leaves the calendar");
    }

    #[test]
    fn typing_a_query_narrows_live_and_backspace_undoes() {
        let mut app = three_tasks();
        app.update(Message::BeginSearch);
        assert_eq!(
            app.update(Message::SearchChar('r')),
            vec![Effect::Reload],
            "each keystroke asks the runtime to re-run the query"
        );
        app.update(Message::SearchChar('e'));
        assert_eq!(app.search_query(), "re");
        assert_eq!(app.update(Message::SearchBackspace), vec![Effect::Reload]);
        assert_eq!(app.search_query(), "r");
    }

    #[test]
    fn search_char_is_ignored_outside_query_entry() {
        let mut app = search_app();
        app.update(Message::SearchChar('x'));
        app.update(Message::SearchSubmit); // stop editing
        assert_eq!(app.update(Message::SearchChar('z')), vec![]);
        assert_eq!(app.search_query(), "x");
    }

    #[test]
    fn search_submit_starts_result_navigation_only_for_a_non_blank_query() {
        let mut app = search_app();
        assert_eq!(
            app.update(Message::SearchSubmit),
            vec![],
            "blank is a no-op"
        );
        assert!(app.search_editing(), "the query field stays focused");

        app.update(Message::SearchChar('x'));
        assert_eq!(app.update(Message::SearchSubmit), vec![]);
        assert!(!app.search_editing(), "results are now navigable");
    }

    #[test]
    fn exit_search_returns_to_the_day_list_and_clears_the_query() {
        let mut app = search_app();
        app.update(Message::SearchChar('x'));
        assert_eq!(app.update(Message::ExitSearch), vec![]);
        assert_eq!(app.mode(), Mode::Browse);
        assert_eq!(app.search_query(), "");
        assert!(app.search_results().is_empty());
        assert_eq!(app.search_selected(), 0);
    }

    #[test]
    fn search_selection_wraps_over_the_results() {
        let mut app = search_app();
        assert_eq!(app.search_selected(), 0);
        assert_eq!(app.update(Message::SearchSelectNext), vec![]);
        assert_eq!(app.search_selected(), 1);
        app.update(Message::SearchSelectPrev);
        assert_eq!(app.search_selected(), 0);
        app.update(Message::SearchSelectPrev);
        assert_eq!(app.search_selected(), 2, "wraps to the last result");
    }

    #[test]
    fn search_selection_clamps_when_results_shrink() {
        let mut app = search_app();
        app.update(Message::SearchSelectNext);
        app.update(Message::SearchSelectNext);
        assert_eq!(app.search_selected(), 2);
        app.set_search_results(vec![task(1, "one")]);
        assert_eq!(app.search_selected(), 0);
    }

    #[test]
    fn search_result_toggles_done_like_a_day_list_row() {
        let mut app = search_app();
        app.update(Message::SearchSelectNext);
        let effects = app.update(Message::ToggleSelected);

        assert!(app.search_results()[1].done);
        assert_eq!(
            effects,
            vec![Effect::Update(Task {
                done: true,
                ..task(2, "two")
            })]
        );
    }

    #[test]
    fn search_result_deletes_and_undoes() {
        let mut app = search_app();
        app.update(Message::SearchSelectNext);

        let effects = app.update(Message::DeleteSelected);

        assert_eq!(effects, vec![Effect::Delete { task_id: 2 }]);
        assert_eq!(app.search_results().len(), 2);
        assert!(app.search_results().iter().all(|t| t.id != 2));

        let effects = app.update(Message::UndoDelete);
        assert_eq!(effects, vec![Effect::Restore(task(2, "two"))]);
        assert!(
            app.search_results().iter().any(|t| t.id == 2),
            "undo puts the result back where it was found"
        );
    }

    #[test]
    fn search_result_edits_and_returns_to_the_results() {
        let mut app = search_app();
        assert_eq!(app.update(Message::BeginEdit), vec![]);
        assert_eq!(app.mode(), Mode::Detail);
        assert_eq!(app.editor().expect("editor open").title, "one");

        app.update(Message::EditChar('!'));
        let effects = app.update(Message::CommitEdit);

        assert_eq!(
            effects,
            vec![Effect::Update(Task {
                title: "one!".to_string(),
                ..task(1, "one")
            })]
        );
        assert_eq!(app.mode(), Mode::Search, "edit returns to the results");
        assert!(!app.search_editing(), "and lands ready to act on them");
    }

    #[test]
    fn search_result_edit_cancel_returns_to_the_results() {
        let mut app = search_app();
        app.update(Message::BeginEdit);
        app.update(Message::EditChar('!'));
        assert_eq!(app.update(Message::CancelEdit), vec![]);
        assert_eq!(app.mode(), Mode::Search);
    }

    #[test]
    fn search_result_runs_the_timer() {
        let mut app = search_app();
        app.update(Message::SearchSelectNext);
        assert_eq!(app.update(Message::ToggleTimer(noon())), vec![]);
        assert_eq!(app.running_task_id(), Some(2));
    }

    #[test]
    fn day_list_edit_still_returns_to_browse() {
        let mut app = three_tasks();
        app.update(Message::BeginEdit);
        app.update(Message::CancelEdit);
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn refocus_search_reopens_the_query_field() {
        let mut app = search_app();
        app.update(Message::SearchChar('x'));
        app.update(Message::SearchSubmit);
        assert!(!app.search_editing());
        assert_eq!(app.update(Message::RefocusSearch), vec![]);
        assert!(app.search_editing());
    }

    #[test]
    fn search_cursor_locates_a_day_group() {
        let mut app = App::new(day(), vec![], vec![]);
        app.update(Message::BeginSearch);
        let earlier = NaiveDate::from_ymd_opt(2026, 10, 8).expect("valid date");
        let mut old = task(9, "old");
        old.day = earlier;
        app.set_search_results(vec![old, task(1, "one"), task(2, "two")]);
        app.update(Message::SearchSelectNext);
        app.update(Message::SearchSelectNext);
        assert_eq!(app.search_selected(), 2);
        assert_eq!(app.search_cursor_in(earlier), None);
        assert_eq!(app.search_cursor_in(day()), Some(1));
    }

    #[test]
    fn search_ignores_the_tag_filter() {
        let mut app = with_tags();
        app.update(Message::CycleTagFilter);
        assert_eq!(app.tag_filter(), Some("admin"));
        app.update(Message::BeginSearch);
        assert_eq!(app.tag_filter(), None, "search is not tag-scoped");
    }
}
