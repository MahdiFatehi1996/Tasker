//! App state seam: pure state transitions, no terminal or database I/O.
//!
//! The event loop translates keys to [`Message`]. [`App::update`] evolves the
//! in-memory state and returns [`Effect`]s; the runtime performs each effect
//! against the [`Database`](crate::db::Database) and reloads the day list.
//! Tests assert on states and effects only.

use chrono::NaiveDate;

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
    Quit,
}

/// Persistence work the runtime must perform after an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    SaveNew(NewTask),
    Update(Task),
}

/// In-memory state of the day-list view.
#[derive(Debug)]
pub struct App {
    viewed: NaiveDate,
    tasks: Vec<Task>,
    selected: usize,
    mode: Mode,
    draft: String,
    should_quit: bool,
}

impl App {
    /// Day list for `viewed`, freshly loaded.
    pub fn new(viewed: NaiveDate, tasks: Vec<Task>) -> App {
        let mut app = App {
            viewed,
            tasks: Vec::new(),
            selected: 0,
            mode: Mode::Browse,
            draft: String::new(),
            should_quit: false,
        };
        app.set_tasks(tasks);
        app
    }

    pub fn viewed(&self) -> NaiveDate {
        self.viewed
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Task under the selection cursor, if the list is non-empty.
    /// Used by detail flows from ticket #6; tests are the first caller.
    #[allow(dead_code)]
    pub fn selected_task(&self) -> Option<&Task> {
        self.tasks.get(self.selected)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Replace the task list (e.g. after reloading from storage),
    /// keeping the selection on a valid row.
    pub fn set_tasks(&mut self, tasks: Vec<Task>) {
        self.tasks = tasks;
        self.selected = self.selected.min(self.tasks.len().saturating_sub(1));
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.draft.clear();
    }

    fn move_selection(&mut self, step: isize) {
        if self.tasks.is_empty() {
            return;
        }
        let len = self.tasks.len() as isize;
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
            Message::ToggleSelected => match self.tasks.get_mut(self.selected) {
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
        )
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
        let mut app = App::new(day(), vec![]);
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
        let mut app = App::new(day(), vec![]);
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
}
