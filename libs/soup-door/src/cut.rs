//! **Cutting long text fields** to a `max_chars`, remembering what was cut.

use serde_json::{json, Value};

/// `--max-chars` / `max_chars` per field: cuts any longer string value and
/// remembers what it cut, for the answer's `truncated` list
/// (`{id, field, chars}`, `chars` the whole length).
#[derive(Debug, Default)]
pub struct Cut {
    max: Option<usize>,
    cut: Vec<Value>,
}

impl Cut {
    pub fn new(max: Option<usize>) -> Cut {
        Cut { max, cut: Vec::new() }
    }

    /// `v` cut to the limit when it is a longer string; anything else as is.
    pub fn value(&mut self, id: &str, field: &str, v: &Value) -> Value {
        match (self.max, v) {
            (Some(max), Value::String(s)) => Value::String(self.text(id, field, s, max)),
            _ => v.clone(),
        }
    }

    /// `s` cut to `max` chars, noting the cut.
    pub fn text(&mut self, id: &str, field: &str, s: &str, max: usize) -> String {
        let chars = s.chars().count();
        if chars <= max {
            return s.to_string();
        }
        self.cut.push(json!({"id": id, "field": field, "chars": chars}));
        s.chars().take(max).collect()
    }

    /// Note a cut made elsewhere (a search snippet).
    pub fn note(&mut self, entry: Value) {
        self.cut.push(entry);
    }

    /// Put the list into `doc` as `truncated`, when anything was cut.
    pub fn report(self, doc: &mut Value) {
        if !self.cut.is_empty() {
            doc["truncated"] = Value::Array(self.cut);
        }
    }
}
