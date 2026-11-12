//! The host binding layer of one execution. It is the only holder of the execution credential:
//! the credential is never given to Wasmtime, to a WASI context, to a preopened file, to guest
//! memory, to a log line or to an error string. Guest calls reach it only through
//! `host::Bindings`, and it answers only what this task wires: logging, which it records for the
//! result, and progress, which it records for the update operation. Every capability that needs
//! the credential to reach the platform (storage, events, workflows, HTTP) is answered `denied`
//! until the host binding work that follows this task adds it behind this same type.

use crate::api::{Credential, Latest, MAX_LINE_CHARS, MAX_LINES, MAX_SECRET_BYTES, Redactor};
use crate::host::{Bindings, Context, HostError, Level, Operation, Reply};
use std::sync::{Mutex, PoisonError};

/// A line is kept up to the result's line length plus what the longest secret can reach past it,
/// so a secret straddling the cut is still found; the redactor cuts to the result length.
const KEPT_CHARS: usize = MAX_LINE_CHARS + MAX_SECRET_BYTES;

#[derive(Default)]
struct State {
    lines: Vec<String>,
    /// What the guest last reported through the progress interface.
    progress: Option<Latest>,
    reports: u64,
}

pub struct Binding {
    credential: Credential,
    state: Mutex<State>,
}

impl Binding {
    pub fn new(credential: Credential) -> Self {
        Self {
            credential,
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The redaction of this execution's logs, built once from the credential's strings.
    pub fn redactor(&self) -> Redactor {
        Redactor::new(self.credential.strings())
    }

    /// The recorded log lines, leaving none behind.
    pub fn take_lines(&self) -> Vec<String> {
        std::mem::take(&mut self.state().lines)
    }

    /// The latest progress report and the number of reports so far.
    pub fn progress(&self) -> (Option<Latest>, u64) {
        let state = self.state();
        (state.progress.clone(), state.reports)
    }

    fn record(&self, progress: Latest) {
        let mut state = self.state();
        state.progress = Some(progress);
        state.reports += 1;
    }
}

fn level(level: Level) -> &'static str {
    match level {
        Level::Debug => "debug",
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Error => "error",
    }
}

impl Bindings for Binding {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        match operation {
            Operation::Log {
                severity,
                message,
                fields,
            } => {
                let mut state = self.state();
                if state.lines.len() < MAX_LINES {
                    let mut line = format!("{} {message}", level(severity));
                    for field in fields {
                        line.push_str(&format!(" {}={}", field.name, field.value));
                    }
                    if let Some((at, _)) = line.char_indices().nth(KEPT_CHARS) {
                        line.truncate(at);
                    }
                    state.lines.push(line);
                }
                Ok(Reply::Empty)
            }
            Operation::Progress {
                completed, total, ..
            } => {
                self.record(Latest::Progress {
                    completed,
                    total: Some(total),
                });
                Ok(Reply::Empty)
            }
            Operation::ProgressCheckpoint { key } => {
                self.record(Latest::Checkpoint(key));
                Ok(Reply::Empty)
            }
            _ => Err(HostError::Denied),
        }
    }
}
