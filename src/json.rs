//! Internal JSON writer: the one place the CLI renders machine-readable
//! shapes, with no serialisation dependency.
//!
//! It escapes a string, renders a single [`Task`] as an object, and renders a
//! collection as an array. The whole-database `export --format json` dump is
//! byte-for-byte what this writer produced when the helpers lived in `cli`.

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
}
