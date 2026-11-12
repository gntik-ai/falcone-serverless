//! The attempts running on this pod: bounded admission and cancellation by identity.
//!
//! Identities are request values held in memory only; nothing here formats or logs them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

type Key = (String, String);

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// As many attempts are running as the configured concurrency.
    Busy,
    /// The same activation and attempt is already running.
    Duplicate,
}

enum State {
    Running,
    /// `uncertain` when a host call may still commit.
    Finished {
        uncertain: bool,
    },
}

struct Attempt {
    cancel: Arc<AtomicBool>,
    state: Mutex<State>,
    finished: Condvar,
}

pub struct Attempts {
    running: Mutex<HashMap<Key, Arc<Attempt>>>,
    limit: usize,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Attempts {
    pub fn new(limit: usize) -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
            limit: limit.max(1),
        }
    }

    /// Registers a running attempt whose `cancel` flag is the host's cancellation flag.
    pub fn admit(
        &self,
        activation: &str,
        attempt: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<Running<'_>, Refusal> {
        let key = (activation.to_owned(), attempt.to_owned());
        let mut running = locked(&self.running);
        if running.contains_key(&key) {
            return Err(Refusal::Duplicate);
        }
        if running.len() >= self.limit {
            return Err(Refusal::Busy);
        }
        let entry = Arc::new(Attempt {
            cancel,
            state: Mutex::new(State::Running),
            finished: Condvar::new(),
        });
        running.insert(key.clone(), entry.clone());
        Ok(Running {
            attempts: self,
            key,
            entry,
        })
    }

    pub fn is_running(&self, activation: &str, attempt: &str) -> bool {
        locked(&self.running).contains_key(&(activation.to_owned(), attempt.to_owned()))
    }

    /// Stops a running attempt and waits up to `wait` for it to finish. `None` when no such
    /// attempt is running, with no effect. `Some(true)` when the outcome is uncertain: a host
    /// call may have committed, or the attempt did not stop in time.
    pub fn cancel(&self, activation: &str, attempt: &str, wait: Duration) -> Option<bool> {
        let entry = locked(&self.running)
            .get(&(activation.to_owned(), attempt.to_owned()))?
            .clone();
        entry.cancel.store(true, Ordering::Release);
        let state = locked(&entry.state);
        let (state, _) = entry
            .finished
            .wait_timeout_while(state, wait, |state| matches!(state, State::Running))
            .unwrap_or_else(PoisonError::into_inner);
        Some(match *state {
            State::Running => true,
            State::Finished { uncertain } => uncertain,
        })
    }
}

/// An admitted attempt; dropping it without `finish` records an uncertain end.
pub struct Running<'a> {
    attempts: &'a Attempts,
    key: Key,
    entry: Arc<Attempt>,
}

impl Running<'_> {
    pub fn finish(self, uncertain: bool) {
        self.end(uncertain);
    }

    fn end(&self, uncertain: bool) {
        locked(&self.attempts.running).remove(&self.key);
        *locked(&self.entry.state) = State::Finished { uncertain };
        self.entry.finished.notify_all();
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if matches!(*locked(&self.entry.state), State::Running) {
            self.end(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn admission_is_bounded_and_refuses_a_duplicate() {
        let attempts = Attempts::new(1);
        let first = attempts.admit("a", "1", flag()).unwrap();
        assert_eq!(
            attempts.admit("a", "1", flag()).err(),
            Some(Refusal::Duplicate)
        );
        assert_eq!(attempts.admit("a", "2", flag()).err(), Some(Refusal::Busy));
        first.finish(false);
        assert!(attempts.admit("a", "2", flag()).is_ok());
    }

    #[test]
    fn cancelling_an_unknown_attempt_has_no_effect() {
        let attempts = Attempts::new(1);
        let running = attempts.admit("a", "1", flag()).unwrap();
        assert_eq!(attempts.cancel("a", "2", Duration::ZERO), None);
        assert_eq!(attempts.cancel("b", "1", Duration::ZERO), None);
        assert!(attempts.is_running("a", "1"));
        running.finish(false);
    }

    #[test]
    fn cancel_is_acknowledged_when_the_attempt_stops_and_uncertain_otherwise() {
        for (uncertain, expected) in [(false, Some(false)), (true, Some(true))] {
            let attempts = Attempts::new(1);
            let cancel = flag();
            let running = attempts.admit("a", "1", cancel.clone()).unwrap();
            std::thread::scope(|scope| {
                scope.spawn(move || {
                    while !cancel.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    running.finish(uncertain);
                });
                assert_eq!(attempts.cancel("a", "1", Duration::from_secs(10)), expected);
            });
            assert_eq!(attempts.cancel("a", "1", Duration::ZERO), None);
        }
    }

    #[test]
    fn an_attempt_that_does_not_stop_in_time_is_uncertain() {
        let attempts = Attempts::new(1);
        let _running = attempts.admit("a", "1", flag()).unwrap();
        assert_eq!(
            attempts.cancel("a", "1", Duration::from_millis(20)),
            Some(true)
        );
    }
}
