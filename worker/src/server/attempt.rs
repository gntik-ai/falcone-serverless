//! Cancellation ownership is separate from both the compiler lock and connection admission.
use crate::host::outcome::InvocationOutcome;
use std::collections::BTreeMap;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub(super) type Key = (String, String);
pub(super) type Attempts = Mutex<BTreeMap<Key, Arc<Attempt>>>;

pub(super) struct Attempt {
    pub cancelled: Arc<AtomicBool>,
    pub running: AtomicBool,
    done: Mutex<Option<bool>>,
    changed: Condvar,
}
impl Attempt {
    pub fn new(cancelled: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            cancelled,
            running: AtomicBool::new(false),
            done: Mutex::new(None),
            changed: Condvar::new(),
        })
    }
    pub fn cancel(&self, deadline: Duration) -> bool {
        self.cancelled.store(true, Ordering::Release);
        let done = self
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (done, _) = self
            .changed
            .wait_timeout_while(done, deadline, |done| done.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *done == Some(true)
    }
    pub fn complete(&self, outcome: InvocationOutcome) {
        // A late cancel flag cannot turn an already-final result into an
        // interruption. Completion owns this rule so callers cannot infer an
        // acknowledgement from the flag alone.
        let acknowledged = self.cancelled.load(Ordering::Acquire)
            && matches!(
                outcome,
                InvocationOutcome::GuestInterrupted | InvocationOutcome::Cancelled
            );
        *self
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(acknowledged);
        self.changed.notify_all();
    }
}

pub(super) struct Registration<'a> {
    pub attempt: Arc<Attempt>,
    key: Key,
    attempts: &'a Attempts,
}
impl<'a> Registration<'a> {
    pub fn new(attempts: &'a Attempts, key: Key, attempt: Arc<Attempt>) -> Option<Self> {
        let mut map = attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.contains_key(&key) {
            return None;
        }
        map.insert(key.clone(), attempt.clone());
        Some(Self {
            attempt,
            key,
            attempts,
        })
    }
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        let mut map = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.remove(&self.key);
        // Also wakes a cancellation waiter after an unwind or pre-execution refusal.
        let mut done = self
            .attempt
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if done.is_none() {
            *done = Some(false);
        }
        self.attempt.changed.notify_all();
    }
}
