//! Structured worker logs: one JSON object per line on standard error.
//!
//! Fields are identifiers, numbers and fixed labels only. A request value that could carry the
//! execution credential never reaches this module except through the redactor.

use serde_json::{Map, Value};
use std::io::Write;

/// Emits one event. The event name is fixed text.
pub fn event(name: &str, fields: &[(&str, Value)]) {
    let mut object = Map::new();
    object.insert("event".into(), Value::from(name));
    for (key, value) in fields {
        object.insert((*key).to_owned(), value.clone());
    }
    let mut line = Value::Object(object).to_string();
    line.push('\n');
    // A closed stderr must not stop the worker.
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}
