//! Hooks for tests and qualification builds only: this module does not exist in a release
//! build of the worker. They are keyed by artifact digest so that tests running in parallel
//! in one process do not see each other.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Entry {
    started: usize,
    finished: usize,
    delay: Duration,
}

fn table() -> std::sync::MutexGuard<'static, HashMap<[u8; 32], Entry>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<[u8; 32], Entry>>> = std::sync::OnceLock::new();
    TABLE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Makes validation of the artifact with this digest take `delay` longer, on the helper
/// thread, the way a slow validator would. The helper does not observe cancellation while it
/// waits: the execution must leave it.
pub fn delay_validation(digest: [u8; 32], delay: Duration) {
    table().entry(digest).or_default().delay = delay;
}

/// How many times validation of this artifact started and finished.
pub fn validations(digest: &[u8; 32]) -> (usize, usize) {
    table()
        .get(digest)
        .map_or((0, 0), |entry| (entry.started, entry.finished))
}

pub(super) fn validation_started(digest: &[u8; 32]) {
    let delay = {
        let mut table = table();
        let entry = table.entry(*digest).or_default();
        entry.started += 1;
        entry.delay
    };
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
}

pub(super) fn validation_finished(digest: &[u8; 32]) {
    table().entry(*digest).or_default().finished += 1;
}

/// Guest-to-host entries that started at or after `until` and were refused, by attempt.
fn late() -> std::sync::MutexGuard<'static, HashMap<String, usize>> {
    static LATE: std::sync::OnceLock<Mutex<HashMap<String, usize>>> = std::sync::OnceLock::new();
    LATE.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn late_entry(attempt: &str) {
    *late().entry(attempt.to_owned()).or_default() += 1;
}

/// How many guest-to-host entries of this attempt were refused for starting at or after the
/// execution's deadline. The count is final once the attempt's answer has been received.
pub fn late_entries(attempt: &str) -> usize {
    late().get(attempt).copied().unwrap_or_default()
}

/// A count of connections in a state, with a way to wait until it is reached.
#[derive(Default)]
pub struct Gauge {
    value: Mutex<usize>,
    changed: Condvar,
}

impl Gauge {
    fn change(&self, update: impl FnOnce(usize) -> usize) {
        let mut value = self
            .value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *value = update(*value);
        self.changed.notify_all();
    }

    pub fn get(&self) -> usize {
        *self
            .value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits until at least `n` are counted; false if `within` passes first.
    pub fn wait_at_least(&self, n: usize, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let mut value = self
            .value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *value < n {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            value = self
                .changed
                .wait_timeout(value, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        true
    }
}

/// Where the connections of a server are waiting on their peers: the server-side events that
/// tests synchronise on instead of sleeping.
#[derive(Default)]
pub struct Waiting {
    /// Connections reading a request line or headers.
    pub in_headers: Gauge,
    /// Connections reading a request body.
    pub in_body: Gauge,
}

/// The waiting connections of each listener.
#[derive(Default)]
pub struct Observed {
    pub execute: Arc<Waiting>,
    pub control: Arc<Waiting>,
}

/// Counts one connection as waiting in a state until it is dropped.
pub(super) struct Enter(Arc<Waiting>, fn(&Waiting) -> &Gauge);

impl Enter {
    pub(super) fn headers(waiting: &Arc<Waiting>) -> Self {
        Self::new(waiting, |waiting| &waiting.in_headers)
    }

    pub(super) fn body(waiting: &Arc<Waiting>) -> Self {
        Self::new(waiting, |waiting| &waiting.in_body)
    }

    fn new(waiting: &Arc<Waiting>, gauge: fn(&Waiting) -> &Gauge) -> Self {
        gauge(waiting).change(|value| value + 1);
        Self(waiting.clone(), gauge)
    }
}

impl Drop for Enter {
    fn drop(&mut self) {
        (self.1)(&self.0).change(|value| value.saturating_sub(1));
    }
}
