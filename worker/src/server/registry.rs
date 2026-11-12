//! The executions in flight, by `(activation, attempt)`: what cancel and update look up.

use super::binding::Binding;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

type Key = (String, String);

/// One execution in flight.
pub struct Entry {
    requested: AtomicBool,
    attached: Mutex<Option<(Arc<AtomicBool>, Arc<Binding>)>>,
    finished: Mutex<Option<bool>>,
    signal: Condvar,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Entry {
    /// Asks the guest to stop. A request that arrives before the guest exists is kept and
    /// applied the moment the host is attached.
    pub fn request_cancel(&self) {
        self.requested.store(true, Ordering::Release);
        if let Some((flag, _)) = &*locked(&self.attached) {
            flag.store(true, Ordering::Release);
        }
    }

    pub fn cancel_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    /// Connects the host's cancellation flag and binding.
    pub fn attach(&self, flag: Arc<AtomicBool>, binding: Arc<Binding>) {
        if self.cancel_requested() {
            flag.store(true, Ordering::Release);
        }
        *locked(&self.attached) = Some((flag, binding));
    }

    pub fn binding(&self) -> Option<Arc<Binding>> {
        locked(&self.attached)
            .as_ref()
            .map(|(_, binding)| binding.clone())
    }

    /// Records the end of the execution: `uncertain` when a detached host call may still commit.
    pub fn finish(&self, uncertain: bool) {
        let mut finished = locked(&self.finished);
        finished.get_or_insert(uncertain);
        self.signal.notify_all();
    }

    /// Waits up to `deadline` for the execution to end; `Some(uncertain)` once it has.
    pub fn wait(&self, deadline: Duration) -> Option<bool> {
        let until = Instant::now() + deadline;
        let mut finished = locked(&self.finished);
        loop {
            if let Some(uncertain) = *finished {
                return Some(uncertain);
            }
            let left = until.checked_duration_since(Instant::now())?;
            finished = self
                .signal
                .wait_timeout(finished, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

#[derive(Default)]
pub struct Registry {
    entries: Mutex<HashMap<Key, Arc<Entry>>>,
}

impl Registry {
    /// Registers an execution; `None` when the same attempt is already running.
    pub fn register(self: &Arc<Self>, activation: &str, attempt: &str) -> Option<Registered> {
        let key = (activation.to_owned(), attempt.to_owned());
        let mut entries = locked(&self.entries);
        if entries.contains_key(&key) {
            return None;
        }
        let entry = Arc::new(Entry {
            requested: AtomicBool::new(false),
            attached: Mutex::new(None),
            finished: Mutex::new(None),
            signal: Condvar::new(),
        });
        entries.insert(key.clone(), entry.clone());
        Some(Registered {
            registry: self.clone(),
            key,
            entry,
        })
    }

    pub fn get(&self, activation: &str, attempt: &str) -> Option<Arc<Entry>> {
        locked(&self.entries)
            .get(&(activation.to_owned(), attempt.to_owned()))
            .cloned()
    }

    /// Asks every execution to stop; used when the service shuts down.
    pub fn cancel_all(&self) {
        for entry in locked(&self.entries).values() {
            entry.request_cancel();
        }
    }
}

/// The registration of one execution; dropping it removes the entry and releases cancel waiters.
pub struct Registered {
    registry: Arc<Registry>,
    key: Key,
    pub entry: Arc<Entry>,
}

impl Drop for Registered {
    fn drop(&mut self) {
        locked(&self.registry.entries).remove(&self.key);
        self.entry.finish(false);
    }
}
