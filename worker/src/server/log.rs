//! Structured worker logs: one JSON object per line on stderr.
//!
//! Redaction by construction: the event name is a literal and every field value is a fixed
//! label, a number or the `Debug` of an api type, which prints `<redacted>` for a request value.
//! Nothing here takes a request, a credential or a guest value.

use std::io::Write;

pub fn event(name: &'static str, fields: &[(&'static str, &str)]) {
    let mut line = serde_json::Map::new();
    line.insert("event".into(), name.into());
    for (key, value) in fields {
        line.insert((*key).into(), (*value).into());
    }
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{}", serde_json::Value::Object(line));
}

/// Replaces the default panic output, which prints the payload and may quote input.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let place = info
            .location()
            .map(|at| format!("{}:{}", at.file(), at.line()))
            .unwrap_or_default();
        event("panic", &[("at", &place)]);
    }));
}
