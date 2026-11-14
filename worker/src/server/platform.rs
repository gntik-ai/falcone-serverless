//! The host binding layer of one execution: the only holder of the execution credential.
//!
//! A `Platform` is bound once per execution, under that execution's deadline and cancellation.
//! It is the `Bindings` the capability host calls into. It captures guest log lines for the
//! result and keeps the credential's strings so they can be scrubbed from anything that leaves.
//! It does not implement `Debug` or `Display` with its contents, and no error or log path in
//! this module formats a credential value.
//!
//! The transport to Falcone's storage, event, workflow and HTTP services is not part of this
//! module: every operation other than logging and progress is denied until that transport lands
//! (the dispatcher and host-binding work of INV-01), so the credential is held but not yet
//! presented to any service.

use super::redact::Capture;
use crate::host::{Bindings, Context, HostError, Operation, Reply};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct Platform {
    /// The credential's strings: the host-only secret set, used to scrub guest-visible text.
    secrets: Vec<String>,
    /// The execution's absolute deadline and cancel flag, which bound everything done through
    /// this platform.
    until: Instant,
    cancelled: Arc<AtomicBool>,
    capture: Mutex<Capture>,
    #[cfg(any(test, feature = "qualification"))]
    on_call: Option<super::Probe>,
}

impl fmt::Debug for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Platform(<redacted>)")
    }
}

impl Platform {
    /// Binds the platform for one execution under its absolute deadline `until` and its cancel
    /// flag. `secrets` are the credential's strings.
    pub fn bind(secrets: Vec<String>, until: Instant, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            secrets,
            until,
            cancelled,
            capture: Mutex::new(Capture::default()),
            #[cfg(any(test, feature = "qualification"))]
            on_call: None,
        }
    }

    #[cfg(any(test, feature = "qualification"))]
    pub fn with_call_probe(mut self, probe: super::Probe) -> Self {
        self.on_call = Some(probe);
        self
    }

    /// The credential's strings, for the redactor.
    pub(super) fn secrets(&self) -> &[String] {
        &self.secrets
    }

    /// The log lines captured so far, taken out of the platform.
    pub(super) fn take_logs(&self) -> Capture {
        std::mem::take(&mut *self.capture.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

fn severity(level: crate::host::Level) -> &'static str {
    match level {
        crate::host::Level::Debug => "debug",
        crate::host::Level::Info => "info",
        crate::host::Level::Warn => "warn",
        crate::host::Level::Error => "error",
    }
}

impl Bindings for Platform {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        #[cfg(any(test, feature = "qualification"))]
        if let Some(probe) = &self.on_call {
            probe();
        }
        // Nothing is done for an execution whose deadline has passed or that was cancelled.
        if self.cancelled.load(Ordering::Acquire) {
            return Err(HostError::Cancelled);
        }
        if Instant::now() >= self.until {
            return Err(HostError::Timeout);
        }
        match operation {
            Operation::Log {
                severity: level,
                message,
                fields,
            } => {
                let mut line = format!("{} {message}", severity(level));
                for field in fields {
                    line.push_str(&format!(" {}={}", field.name, field.value));
                }
                self.capture
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(line);
                Ok(Reply::Empty)
            }
            Operation::Progress { .. } | Operation::ProgressCheckpoint { .. } => Ok(Reply::Empty),
            _ => Err(HostError::Denied),
        }
    }
}
