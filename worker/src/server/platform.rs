//! The host binding layer: where the execution credential lives.
//!
//! The credential is bound here and nowhere else. A guest never receives it: the guest
//! sees only the invocation context and the capability interfaces the host links, and the
//! capability host reaches the platform through [`Bindings`], which a [`Platform`] builds per
//! execution and which alone retains the credential.

use crate::host::{Bindings, Context, HostError, Level, Operation, Reply};
use std::fmt;
use std::sync::{Arc, Mutex};

/// The execution credential's claims. It prints as a marker and is read only by the
/// platform that binds it.
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The claims, for the binding layer only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Why the platform could not supply something.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unavailable {
    NotFound,
    Failed,
}

/// The system boundary of the worker: where artifacts come from and how the credential is
/// bound to capabilities. Both calls may block; the server supervises them under the
/// execution's deadline and leaves them at `until` or on cancel.
pub trait Platform: Send + Sync + 'static {
    /// The artifact bytes of the revision, by digest (`sha256:<hex>`). The server verifies
    /// the digest of what it gets.
    fn artifact(&self, digest: &str) -> Result<Vec<u8>, Unavailable>;

    /// Binds the credential for one execution and returns the bindings for the capability
    /// host. Log operations never reach them: the server captures and redacts logs.
    fn bind(&self, context: &Context, credential: Secret)
    -> Result<Arc<dyn Bindings>, Unavailable>;
}

/// The worker's own platform until the dispatcher adopts the execution API: artifacts come
/// from a pod-local directory (`<dir>/<hex digest>.wasm`), and every capability is denied.
/// The credential is held by the bindings and used for nothing.
pub struct LocalPlatform {
    directory: std::path::PathBuf,
}

impl LocalPlatform {
    pub fn new(directory: impl Into<std::path::PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
}

/// Bindings that deny every capability and retain the credential.
struct Denied {
    _credential: Secret,
}

impl Bindings for Denied {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Err(HostError::Denied)
    }
}

impl Platform for LocalPlatform {
    fn artifact(&self, digest: &str) -> Result<Vec<u8>, Unavailable> {
        use std::io::Read;
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or(Unavailable::NotFound)?;
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Unavailable::NotFound);
        }
        let file = std::fs::File::open(self.directory.join(format!("{hex}.wasm")))
            .map_err(|_| Unavailable::NotFound)?;
        let mut bytes = Vec::new();
        file.take(MAX_ARTIFACT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Unavailable::Failed)?;
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(Unavailable::Failed);
        }
        Ok(bytes)
    }

    fn bind(&self, _: &Context, credential: Secret) -> Result<Arc<dyn Bindings>, Unavailable> {
        Ok(Arc::new(Denied {
            _credential: credential,
        }))
    }
}

/// The largest artifact: the validation input bound of the compile path.
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

/// Wraps the platform's bindings. Log operations are captured (redacted) for the result and
/// never forwarded; every other operation goes to the platform's bindings.
pub(super) struct Capturing {
    pub(super) inner: Arc<dyn Bindings>,
    pub(super) logs: Arc<Mutex<super::redact::Logs>>,
}

impl Bindings for Capturing {
    fn call(&self, context: &Context, operation: Operation) -> Result<Reply, HostError> {
        match operation {
            Operation::Log {
                severity,
                message,
                fields,
            } => {
                let mut line = format!("{} {message}", level(severity));
                for field in &fields {
                    line.push_str(&format!(" {}={}", field.name, field.value));
                }
                self.logs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(&line);
                Ok(Reply::Empty)
            }
            // Stdout lines join the same record; a dropped line is reported, so the host
            // flags truncation. Guest log calls keep their existing behaviour.
            Operation::Stdout { text } => {
                let kept = self
                    .logs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(&format!("info {text} stream=stdout"));
                if kept {
                    Ok(Reply::Empty)
                } else {
                    Err(HostError::LimitExceeded)
                }
            }
            other => self.inner.call(context, other),
        }
    }

    fn resolve(&self, host: &str) -> Result<Vec<std::net::IpAddr>, HostError> {
        self.inner.resolve(host)
    }

    fn http(
        &self,
        context: &Context,
        request: &crate::host::net::PinnedRequest,
    ) -> Result<crate::host::net::Response, HostError> {
        self.inner.http(context, request)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::redact::{Logs, MAX_LINES, Redactor};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    struct Unused;
    impl Bindings for Unused {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            panic!("logging and stdout never reach the platform's bindings")
        }
    }

    fn capturing() -> (Capturing, Arc<Mutex<Logs>>) {
        let until = Instant::now() + Duration::from_secs(600);
        let redactor = Redactor::new(["not-in-output"], until, &AtomicBool::new(false));
        let logs = Arc::new(Mutex::new(Logs::new(redactor)));
        let capturing = Capturing {
            inner: Arc::new(Unused),
            logs: logs.clone(),
        };
        (capturing, logs)
    }

    #[test]
    fn stdout_dropped_by_the_record_is_reported_and_guest_logging_is_unchanged() {
        let context = Context::default();
        let (capturing, logs) = capturing();
        let stdout = |text: &str| Operation::Stdout { text: text.into() };
        assert!(capturing.call(&context, stdout("hello")).is_ok());
        for _ in 1..MAX_LINES {
            let log = Operation::Log {
                severity: Level::Info,
                message: "guest".into(),
                fields: Vec::new(),
            };
            assert!(capturing.call(&context, log).is_ok());
        }
        // The record is full: stdout is refused, a guest log call still succeeds.
        assert!(matches!(
            capturing.call(&context, stdout("lost")),
            Err(HostError::LimitExceeded)
        ));
        let log = Operation::Log {
            severity: Level::Info,
            message: "guest".into(),
            fields: Vec::new(),
        };
        assert!(capturing.call(&context, log).is_ok());
        let lines = logs.lock().unwrap().finish();
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[0], "info hello stream=stdout");
    }
}
