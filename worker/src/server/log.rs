//! Structured worker logs: one JSON object per line on stderr.
//!
//! Redaction by construction: a field value is a number or a `&'static str`, so no request value,
//! credential or guest text can be passed to the logger. Guest log lines travel only in the
//! result, after redaction (`redact.rs`).

use std::io::Write;

#[derive(Clone, Copy)]
pub enum Value {
    Number(u64),
    Text(&'static str),
}

/// The line for an event, without the trailing newline.
pub fn render(event: &'static str, fields: &[(&'static str, Value)]) -> String {
    let mut object = serde_json::Map::new();
    object.insert("event".into(), event.into());
    for (name, value) in fields {
        object.insert(
            (*name).into(),
            match value {
                Value::Number(number) => (*number).into(),
                Value::Text(text) => (*text).into(),
            },
        );
    }
    serde_json::Value::Object(object).to_string()
}

pub fn event(event: &'static str, fields: &[(&'static str, Value)]) {
    // Logging must never fail an execution.
    let _ = writeln!(std::io::stderr().lock(), "{}", render(event, fields));
}

/// Replaces the default panic hook, which prints the panic message and so could print a request
/// value. Only the source location, which is static text, is logged.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let line = info
            .location()
            .map_or(0, |location| u64::from(location.line()));
        event("panic", &[("line", Value::Number(line))]);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_renders_as_one_json_line() {
        let line = render(
            "execution_started",
            &[
                ("execution", Value::Number(7)),
                ("mode", Value::Text("wasm")),
            ],
        );
        assert_eq!(
            line,
            r#"{"event":"execution_started","execution":7,"mode":"wasm"}"#
        );
        assert!(!line.contains('\n'));
    }
}
