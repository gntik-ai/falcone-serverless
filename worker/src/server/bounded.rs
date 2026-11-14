//! Work an execution can leave: it runs on a helper thread and the execution waits for it
//! only until its deadline or its cancellation, whichever comes first.
//!
//! A helper that outlives its execution cannot hold the execution slot, and a fixed cap on
//! live helpers bounds what abandoned ones can cost.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// How often a waiting execution looks at its deadline and its cancellation.
const POLL: Duration = Duration::from_millis(2);

/// Why an execution stopped waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stop {
    Timeout,
    Cancelled,
    /// No helper could be started: the cap was reached or the thread could not be spawned.
    Unavailable,
}

/// The live helpers of one server, abandoned or not.
pub(crate) struct Helpers {
    live: Arc<AtomicUsize>,
    cap: usize,
}

struct Live(Arc<AtomicUsize>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Helpers {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            live: Arc::new(AtomicUsize::new(0)),
            cap,
        }
    }

    /// The first stop condition that holds now, cancellation before the deadline.
    pub(super) fn stopped(until: Instant, cancel: &AtomicBool) -> Option<Stop> {
        if cancel.load(Ordering::Acquire) {
            Some(Stop::Cancelled)
        } else if Instant::now() >= until {
            Some(Stop::Timeout)
        } else {
            None
        }
    }

    /// Runs `work` on a helper and returns its value, or why the wait ended first. The work
    /// is not interrupted when the wait ends; it finishes on its own and its value is dropped.
    pub(super) fn call<T: Send + 'static>(
        &self,
        until: Instant,
        cancel: &AtomicBool,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Stop> {
        if let Some(stop) = Self::stopped(until, cancel) {
            return Err(stop);
        }
        if self.live.fetch_add(1, Ordering::AcqRel) >= self.cap {
            self.live.fetch_sub(1, Ordering::AcqRel);
            return Err(Stop::Unavailable);
        }
        let live = Live(self.live.clone());
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("execution-helper".into())
            .spawn(move || {
                let _live = live;
                let _ = send.send(work());
            })
            .map_err(|_| Stop::Unavailable)?;
        loop {
            match receive.recv_timeout(POLL) {
                // Stop conditions come before any outcome, a failure included: an
                // execution that was cancelled or expired is `cancelled` or `timeout`,
                // whatever the helper returned.
                Ok(value) => {
                    return match Self::stopped(until, cancel) {
                        Some(stop) => Err(stop),
                        None => Ok(value),
                    };
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(stop) = Self::stopped(until, cancel) {
                        return Err(stop);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Self::stopped(until, cancel).unwrap_or(Stop::Unavailable));
                }
            }
        }
    }
}
