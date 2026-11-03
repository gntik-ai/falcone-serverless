//! The system boundary behind the gate: storage, event, workflow, log and progress bindings.
//! Production bindings (SeaweedFS, Kafka, Temporal) are supplied by the embedder; tests use
//! in-process mocks. A binding is only ever called after the gate authorized the call.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::HostError;

/// Why a binding call produced no result. Details stay inside the binding layer: the guest
/// only ever sees a `host-error`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingError {
    /// The binding failed or refused the operation.
    Failed,
    /// The binding observed cancellation or its deadline and stopped.
    Interrupted,
}

impl BindingError {
    /// The guest-visible error: a failure is opaque, an interruption reports why it stopped.
    pub fn into_host(self, control: &CallControl) -> HostError {
        match self {
            Self::Failed => HostError::Denied,
            Self::Interrupted => control.stop_reason().unwrap_or(HostError::Timeout),
        }
    }
}

/// Invocation-wide cancellation, shared with the embedder. Independent of fuel and epochs.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Handed to a binding for one call. A binding must stop promptly once `should_stop` is true;
/// the host never waits for it beyond the deadline.
#[derive(Clone, Debug)]
pub struct CallControl {
    cancel: CancelToken,
    expired: CancelToken,
    deadline: Instant,
}

impl CallControl {
    pub fn new(cancel: CancelToken, deadline: Instant) -> Self {
        Self {
            cancel,
            expired: CancelToken::default(),
            deadline,
        }
    }

    pub(crate) fn expire(&self) {
        self.expired.cancel();
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub fn should_stop(&self) -> bool {
        self.stop_reason().is_some()
    }

    /// `Cancelled` takes precedence over `Timeout`, as in the host-error contract.
    pub fn stop_reason(&self) -> Option<HostError> {
        if self.cancel.is_cancelled() {
            Some(HostError::Cancelled)
        } else if self.expired.is_cancelled() || Instant::now() >= self.deadline {
            Some(HostError::Timeout)
        } else {
            None
        }
    }

    /// Sleeps at most `slice`, in short steps; false once the call should stop.
    pub fn pause(&self, slice: Duration) -> bool {
        let end = Instant::now() + slice;
        while Instant::now() < end {
            if self.should_stop() {
                return false;
            }
            thread::sleep(Duration::from_millis(2).min(slice));
        }
        !self.should_stop()
    }
}

pub trait StorageBinding: Send + Sync {
    fn read(&self, key: &str, control: &CallControl) -> Result<Vec<u8>, BindingError>;
    fn write(&self, key: &str, data: &[u8], control: &CallControl) -> Result<(), BindingError>;
}

pub trait EventBinding: Send + Sync {
    fn publish(
        &self,
        topic: &str,
        payload: &[u8],
        control: &CallControl,
    ) -> Result<(), BindingError>;
}

pub trait WorkflowBinding: Send + Sync {
    /// Returns the opaque execution id of the started workflow.
    fn start(
        &self,
        target: &str,
        input: &[u8],
        control: &CallControl,
    ) -> Result<String, BindingError>;
    fn signal(
        &self,
        target: &str,
        execution: &str,
        name: &str,
        payload: &[u8],
        control: &CallControl,
    ) -> Result<(), BindingError>;
}

/// Structured, redacted logs inheriting verified invocation scope.
pub trait LogSink: Send + Sync {
    fn log(&self, level: &str, message: &str, fields: &[(String, String)]);
}

pub trait ProgressSink: Send + Sync {
    fn report(&self, completed: u64, total: u64, message: &str);
    fn checkpoint(&self, key: &str);
}

pub(crate) struct Discard;

impl LogSink for Discard {
    fn log(&self, _: &str, _: &str, _: &[(String, String)]) {}
}

impl ProgressSink for Discard {
    fn report(&self, _: u64, _: u64, _: &str) {}
    fn checkpoint(&self, _: &str) {}
}
