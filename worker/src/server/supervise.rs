//! Supervised steps: work the execution can leave.
//!
//! Artifact fetch, validation, the cache operation and binding run on a worker thread while the
//! execution waits for the first of its result, the absolute deadline `until`, and the cancel
//! flag. When `until` passes or a cancel arrives the execution leaves at once, whatever the
//! step would later return, and the step's thread is abandoned. An abandoned thread is told to
//! stop through its own flag where the step can observe it, and the number of abandoned
//! threads that are still running is bounded: past the bound a new step is refused as busy.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// How often a waiting execution looks at the cancel flag and the clock.
const POLL: Duration = Duration::from_millis(2);

/// Why an execution left a step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Left {
    /// `until` passed.
    Timeout,
    /// A cancel arrived.
    Cancelled,
    /// Too many abandoned steps are still running, or no thread could be started.
    Busy,
}

/// The bound on abandoned steps that are still running.
pub struct Abandoned {
    running: AtomicUsize,
    max: usize,
}

impl Abandoned {
    pub fn new(max: usize) -> Self {
        Self {
            running: AtomicUsize::new(0),
            max,
        }
    }

    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn running(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }
}

/// Whether the execution must stop now. The deadline and the cancel flag come before every
/// capacity or admission result.
pub fn expired(until: Instant, cancelled: &AtomicBool) -> Option<Left> {
    if cancelled.load(Ordering::Acquire) {
        Some(Left::Cancelled)
    } else if Instant::now() >= until {
        Some(Left::Timeout)
    } else {
        None
    }
}

const RUNNING: u8 = 0;
const ABANDONED: u8 = 1;
const FINISHED: u8 = 2;

/// Counts a running step that was abandoned; the count falls when its thread ends.
struct Tally {
    bound: Arc<Abandoned>,
    state: Arc<AtomicU8>,
}

impl Drop for Tally {
    fn drop(&mut self) {
        if self.state.swap(FINISHED, Ordering::AcqRel) == ABANDONED {
            self.bound.running.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Runs `work` on its own thread and waits for it under `until` and `cancelled`.
///
/// `work` receives a flag that is set when the execution leaves; a step that can observe it
/// (a supervised compile child, a cache publication) stops and discards its result.
pub fn step<T: Send + 'static>(
    bound: &Arc<Abandoned>,
    until: Instant,
    cancelled: &AtomicBool,
    work: impl FnOnce(Arc<AtomicBool>) -> T + Send + 'static,
) -> Result<T, Left> {
    // The deadline and the cancel flag outrank a capacity result.
    if let Some(left) = expired(until, cancelled) {
        return Err(left);
    }
    if bound.running.load(Ordering::Acquire) >= bound.max {
        return Err(Left::Busy);
    }
    let abandoned = Arc::new(AtomicBool::new(false));
    let state = Arc::new(AtomicU8::new(RUNNING));
    let tally = Tally {
        bound: bound.clone(),
        state: state.clone(),
    };
    let (send, receive) = mpsc::sync_channel(1);
    let flag = abandoned.clone();
    std::thread::Builder::new()
        .name("supervised-step".into())
        .spawn(move || {
            let _tally = tally;
            let _ = send.send(work(flag));
        })
        .map_err(|_| Left::Busy)?;
    loop {
        // A result that arrives after the deadline or a cancel is not used.
        if let Some(left) = expired(until, cancelled) {
            abandon(bound, &state, &abandoned);
            return Err(left);
        }
        let wait = until.saturating_duration_since(Instant::now()).min(POLL);
        match receive.recv_timeout(wait) {
            Ok(value) => {
                return match expired(until, cancelled) {
                    Some(left) => Err(left),
                    None => Ok(value),
                };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Left::Busy),
        }
    }
}

/// Marks the step abandoned. The count rises here and falls when its thread ends, so a
/// thread that already ended is never counted.
fn abandon(bound: &Arc<Abandoned>, state: &AtomicU8, abandoned: &AtomicBool) {
    bound.running.fetch_add(1, Ordering::AcqRel);
    if state
        .compare_exchange(RUNNING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        abandoned.store(true, Ordering::Release);
    } else {
        // The thread ended first: nothing is left running.
        bound.running.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn a_step_past_until_is_a_timeout_never_busy() {
        // Isolated, exhausted capacity: only this test's bound is involved.
        let bound = Arc::new(Abandoned::new(0));
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(step(&bound, past, &flag(), |_| ()), Err(Left::Timeout));
        let cancelled = AtomicBool::new(true);
        let future = Instant::now() + Duration::from_secs(60);
        assert_eq!(
            step(&bound, future, &cancelled, |_| ()),
            Err(Left::Cancelled)
        );
        // Before until, with no capacity, the answer is busy.
        assert_eq!(step(&bound, future, &flag(), |_| ()), Err(Left::Busy));
    }

    #[test]
    fn a_stalled_step_is_left_at_until_and_its_result_is_not_used() {
        let bound = Arc::new(Abandoned::new(2));
        let (release, hold) = mpsc::channel::<()>();
        let until = Instant::now() + Duration::from_millis(50);
        let started = Instant::now();
        let left = step(&bound, until, &flag(), move |abandoned| {
            let _ = hold.recv();
            abandoned.load(Ordering::Acquire)
        });
        assert_eq!(left, Err(Left::Timeout));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(bound.running(), 1);
        drop(release);
        while bound.running() != 0 {
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_result_that_arrives_after_a_cancel_is_not_used() {
        let bound = Arc::new(Abandoned::new(2));
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let result = step(
            &bound,
            Instant::now() + Duration::from_secs(30),
            &cancelled,
            move |_| {
                signal.store(true, Ordering::Release);
                7
            },
        );
        assert_eq!(result, Err(Left::Cancelled));
    }

    #[test]
    fn a_finished_step_returns_its_value() {
        let bound = Arc::new(Abandoned::new(1));
        let until = Instant::now() + Duration::from_secs(30);
        assert_eq!(step(&bound, until, &flag(), |_| 41 + 1), Ok(42));
        assert_eq!(bound.running(), 0);
    }
}
