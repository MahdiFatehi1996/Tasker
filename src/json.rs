//! Internal JSON writer: the one place the CLI renders machine-readable
//! shapes, with no serialisation dependency.
//!
//! It escapes a string, renders a single [`Task`] as an object, and renders a
//! collection as an array. The whole-database `export --format json` dump is
//! byte-for-byte what this writer produced when the helpers lived in `cli`.
//! [`envelope`] wraps a payload with the Agent interface's contract and
//! schema versions; the legacy export stays a bare top-level array.

use crate::model::Task;

/// Escape `value` as a JSON string literal, including the surrounding quotes.
/// Quotes, backslashes, `\n`, `\r`, and `\t` use their short forms; every
/// other control character below `0x20` becomes `\uXXXX`.
pub(crate) fn string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render `values` as a single-line JSON array of strings.
pub(crate) fn string_array(values: &[&str]) -> String {
    let items = values
        .iter()
        .copied()
        .map(string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

/// Wrap a rendered `payload` in the Agent interface envelope: the interface
/// `contract` version and the storage `schema` version around the body, so a
/// caller can detect an incompatible Tasker before trusting the payload. The
/// whole-database `export --format json` dump is deliberately *not* wrapped.
pub(crate) fn envelope(contract: i64, schema: i64, payload: &str) -> String {
    let indented = payload.replace('\n', "\n  ");
    format!(
        "{{\n  \"contract\": {contract},\n  \"schema\": {schema},\n  \"payload\": {indented}\n}}"
    )
}

/// The internal JSON writer.
#[derive(Debug, Default)]
pub(crate) struct JsonWriter {
    out: String,
}

impl JsonWriter {
    /// Start an empty writer.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Append `task` as an object, indenting its braces by `indent` spaces.
    pub(crate) fn task(&mut self, task: &Task, indent: usize) {
        let pad = " ".repeat(indent);
        let field = " ".repeat(indent + 2);
        let due = match task.due {
            Some(due) => string(&due.format("%Y-%m-%dT%H:%M:%S").to_string()),
            None => "null".to_string(),
        };
        let tags = task
            .tags
            .iter()
            .map(|tag| string(tag))
            .collect::<Vec<_>>()
            .join(", ");
        self.out.push_str(&format!("{pad}{{\n"));
        self.out.push_str(&format!("{field}\"id\": {},\n", task.id));
        self.out.push_str(&format!(
            "{field}\"day\": {},\n",
            string(&task.day.format("%Y-%m-%d").to_string())
        ));
        self.out
            .push_str(&format!("{field}\"title\": {},\n", string(&task.title)));
        self.out
            .push_str(&format!("{field}\"done\": {},\n", task.done));
        self.out.push_str(&format!(
            "{field}\"priority\": {},\n",
            string(task.priority.as_str())
        ));
        self.out
            .push_str(&format!("{field}\"notes\": {},\n", string(&task.notes)));
        self.out.push_str(&format!("{field}\"due\": {due},\n"));
        self.out.push_str(&format!(
            "{field}\"tracked_secs\": {},\n",
            task.tracked_secs
        ));
        self.out.push_str(&format!("{field}\"tags\": [{tags}]\n"));
        self.out.push_str(&format!("{pad}}}"));
    }

    /// Append `tasks` as an array of objects, one per line.
    pub(crate) fn collection(&mut self, tasks: &[Task]) {
        self.out.push_str("[\n");
        for (i, task) in tasks.iter().enumerate() {
            self.task(task, 2);
            if i + 1 < tasks.len() {
                self.out.push(',');
            }
            self.out.push('\n');
        }
        self.out.push(']');
    }

    /// Consume the writer, returning the accumulated JSON text.
    pub(crate) fn finish(self) -> String {
        self.out
    }
}

/// A flat JSON object whose members are pre-rendered: numbers, escaped
/// strings, or arrays of [`Task`]. The section payload behind `list --json`
/// and the aggregate figures behind `stats --json` are the two callers; the
/// legacy export stays a bare array written by [`JsonWriter::collection`].
#[derive(Debug, Default)]
pub(crate) struct JsonObject {
    members: Vec<(String, String)>,
}

impl JsonObject {
    /// Start an empty object.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add a numeric member.
    pub(crate) fn number(&mut self, name: &str, value: i64) -> &mut Self {
        self.members.push((name.to_string(), value.to_string()));
        self
    }

    /// Add a string member (escaped including its quotes).
    pub(crate) fn text(&mut self, name: &str, value: &str) -> &mut Self {
        self.members.push((name.to_string(), string(value)));
        self
    }

    /// Add an array-of-objects member: `name` maps to `tasks`, each rendered
    /// as a task object and indented under the member. An empty slice stays
    /// `[]` so a caller still sees every key.
    pub(crate) fn tasks(&mut self, name: &str, tasks: &[Task]) -> &mut Self {
        self.members.push((name.to_string(), task_array(tasks)));
        self
    }

    /// Add a member whose value is an array of already-rendered JSON objects,
    /// one per line and indented under the member. An empty slice stays the
    /// inline `[]` so a caller still sees the key. `search --json` uses it to
    /// nest one object per day group.
    pub(crate) fn objects(&mut self, name: &str, items: &[String]) -> &mut Self {
        let rendered = if items.is_empty() {
            "[]".to_string()
        } else {
            let body = items
                .iter()
                .map(|item| indent(item, 4))
                .collect::<Vec<_>>()
                .join(",\n");
            format!("[\n{body}\n  ]")
        };
        self.members.push((name.to_string(), rendered));
        self
    }

    /// Render the object, one member per line.
    pub(crate) fn finish(&self) -> String {
        if self.members.is_empty() {
            return "{}".to_string();
        }
        let body = self
            .members
            .iter()
            .map(|(name, value)| format!("  {}: {value}", string(name)))
            .collect::<Vec<_>>()
            .join(",\n");
        format!("{{\n{body}\n}}")
    }
}

/// Render `tasks` as an array nested inside an object member: the brackets sit
/// at two spaces, each task at four. Empty input is the inline `[]`.
fn task_array(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "[]".to_string();
    }
    let items = tasks
        .iter()
        .map(|task| {
            let mut writer = JsonWriter::new();
            writer.task(task, 4);
            writer.finish()
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!("[\n{items}\n  ]")
}

/// Prefix every line of `text` with `spaces` spaces.
fn indent(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.split('\n')
        .map(|line| format!("{pad}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Priority;
    use chrono::NaiveDate;

    fn task() -> Task {
        Task {
            id: 7,
            day: NaiveDate::from_ymd_opt(2026, 10, 10).expect("valid date"),
            title: "Solo".to_string(),
            done: true,
            priority: Priority::Low,
            notes: String::new(),
            due: None,
            tracked_secs: 0,
            tags: Vec::new(),
        }
    }

    #[test]
    fn escapes_quotes_backslashes_and_control_characters() {
        assert_eq!(string("plain"), r#""plain""#);
        assert_eq!(
            string("quote \" and \\ slash"),
            r#""quote \" and \\ slash""#
        );
        assert_eq!(string("a\nb\rc\td"), r#""a\nb\rc\td""#);
        assert_eq!(string("\u{1}\u{1f}"), r#""\u0001\u001f""#);
    }

    #[test]
    fn renders_a_task_object_standalone() {
        let mut writer = JsonWriter::new();
        writer.task(&task(), 0);
        let expected = r#"{
  "id": 7,
  "day": "2026-10-10",
  "title": "Solo",
  "done": true,
  "priority": "low",
  "notes": "",
  "due": null,
  "tracked_secs": 0,
  "tags": []
}"#;
        assert_eq!(writer.finish(), expected);
    }

    #[test]
    fn renders_an_object_with_number_string_and_task_members() {
        let mut object = JsonObject::new();
        object
            .text("day", "2026-10-10")
            .number("total", 1)
            .tasks("today", &[task()]);
        let expected = r#"{
  "day": "2026-10-10",
  "total": 1,
  "today": [
    {
      "id": 7,
      "day": "2026-10-10",
      "title": "Solo",
      "done": true,
      "priority": "low",
      "notes": "",
      "due": null,
      "tracked_secs": 0,
      "tags": []
    }
  ]
}"#;
        assert_eq!(object.finish(), expected);
    }

    #[test]
    fn empty_task_member_is_an_inline_array() {
        let mut object = JsonObject::new();
        object.tasks("overdue", &[]);
        assert_eq!(
            object.finish(),
            "{\n  \"overdue\": []\n}",
            "an empty section still carries its key"
        );
    }

    #[test]
    fn empty_object_renders_as_empty_braces() {
        assert_eq!(JsonObject::new().finish(), "{}");
    }

    #[test]
    fn renders_an_array_of_objects_member() {
        let mut group = JsonObject::new();
        group.text("day", "2026-10-10").tasks("tasks", &[task()]);
        let mut object = JsonObject::new();
        object
            .text("query", "solo")
            .objects("days", &[group.finish()]);
        let expected = r#"{
  "query": "solo",
  "days": [
    {
      "day": "2026-10-10",
      "tasks": [
        {
          "id": 7,
          "day": "2026-10-10",
          "title": "Solo",
          "done": true,
          "priority": "low",
          "notes": "",
          "due": null,
          "tracked_secs": 0,
          "tags": []
        }
      ]
    }
  ]
}"#;
        assert_eq!(object.finish(), expected);
    }

    #[test]
    fn empty_objects_member_is_an_inline_array() {
        let mut object = JsonObject::new();
        object.objects("days", &[]);
        assert_eq!(object.finish(), "{\n  \"days\": []\n}");
    }
}
