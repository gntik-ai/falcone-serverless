//! Hooks for tests and qualification builds only: this module does not exist in a release
//! build of the worker. They are keyed by artifact digest so that tests running in parallel
//! in one process do not see each other.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

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
