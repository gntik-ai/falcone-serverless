//! Supervised steps: work the execution can leave.
//!
//! Artifact fetch, validation, the cache operation and binding run on a worker thread while the
//! execution waits for the first of its result, the absolute deadline `until`, and the cancel
//! flag. When `until` passes or a cancel arrives the execution leaves at once, whatever the
//! step would later return, and the step's thread is abandoned. An abandoned thread is told to
//! stop through its own flag (a cancel) or through `until` (the deadline) where the step can
//! observe them, and the number of abandoned threads that are still running is bounded: past the
//! bound a new step is refused as busy.

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

/// The result for an exit that is not a result: the deadline or a cancel if either has
/// happened by now, and only otherwise a capacity result.
fn busy(until: Instant, cancelled: &AtomicBool) -> Left {
    expired(until, cancelled).unwrap_or(Left::Busy)
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
/// `work` receives a flag that is set when the execution leaves because of a cancel. A step
/// that can observe it (a supervised compile child, a cache publication) stops and discards its
/// result, and checks `until` itself for the deadline. The flag is never set for a timeout, so
/// the reason the step stopped is the reason the execution left.
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
        .map_err(|_| busy(until, cancelled))?;
    loop {
        // A result that arrives after the deadline or a cancel is not used.
        if let Some(left) = expired(until, cancelled) {
            abandon(bound, &state, &abandoned, left);
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
            // A step that set the cancel flag and then died is a cancellation, and one that died
            // past `until` is a timeout: the interruption is looked at again before the exit
            // is mapped to a capacity result.
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(busy(until, cancelled)),
        }
    }
}

/// Marks the step abandoned. The count rises here and falls when its thread ends, so a
/// thread that already ended is never counted.
///
/// The step's flag records the reason it was left: it is set for a cancellation and stays clear
/// for a timeout, where the step observes `until` itself. A compile child that is stopped at
/// the deadline is therefore never mistaken for a cancelled one.
fn abandon(bound: &Arc<Abandoned>, state: &AtomicU8, abandoned: &AtomicBool, reason: Left) {
    bound.running.fetch_add(1, Ordering::AcqRel);
    if state
        .compare_exchange(RUNNING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        if reason == Left::Cancelled {
            abandoned.store(true, Ordering::Release);
        }
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
    fn a_step_that_cancels_and_then_dies_is_cancelled_never_busy() {
        // The step sets the cancel flag and panics before it sends anything, so the waiting
        // execution sees a disconnected channel. Whichever wakes first, the answer is the
        // cancellation: the flag is set before the thread dies.
        let bound = Arc::new(Abandoned::new(2));
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let result: Result<(), Left> = step(
            &bound,
            Instant::now() + Duration::from_secs(30),
            &cancelled,
            move |_| {
                signal.store(true, Ordering::Release);
                panic!("step died after the cancel");
            },
        );
        assert_eq!(result, Err(Left::Cancelled));
        // Without a cancel, the same death is a capacity result.
        let result: Result<(), Left> = step(
            &bound,
            Instant::now() + Duration::from_secs(30),
            &flag(),
            |_| panic!("step died"),
        );
        assert_eq!(result, Err(Left::Busy));
    }

    #[test]
    fn a_step_that_dies_past_until_is_a_timeout_never_busy() {
        let bound = Arc::new(Abandoned::new(2));
        let until = Instant::now() + Duration::from_millis(30);
        let result: Result<(), Left> = step(&bound, until, &flag(), move |_| {
            while Instant::now() < until {
                std::thread::yield_now();
            }
            panic!("step died after until");
        });
        assert_eq!(result, Err(Left::Timeout));
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
    fn a_timeout_leaves_the_stop_flag_clear_so_the_deadline_stays_the_reason() {
        // The flag handed to a step means "cancelled" only. A step that is left because
        // `until` passed observes `until` itself, and must never read a cancellation.
        let bound = Arc::new(Abandoned::new(2));
        let (release, hold) = mpsc::channel::<()>();
        let (seen, observed) = mpsc::channel();
        let until = Instant::now() + Duration::from_millis(20);
        let left = step(&bound, until, &flag(), move |stop| {
            let _ = hold.recv();
            let _ = seen.send(stop.load(Ordering::Acquire));
        });
        assert_eq!(left, Err(Left::Timeout));
        drop(release);
        assert_eq!(observed.recv_timeout(Duration::from_secs(30)), Ok(false));
    }

    #[test]
    fn a_cancel_sets_the_stop_flag_of_the_abandoned_step() {
        let bound = Arc::new(Abandoned::new(2));
        let (release, hold) = mpsc::channel::<()>();
        let (seen, observed) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let left = step(
            &bound,
            Instant::now() + Duration::from_secs(30),
            &cancelled,
            move |stop| {
                signal.store(true, Ordering::Release);
                let _ = hold.recv();
                let _ = seen.send(stop.load(Ordering::Acquire));
            },
        );
        assert_eq!(left, Err(Left::Cancelled));
        drop(release);
        assert_eq!(observed.recv_timeout(Duration::from_secs(30)), Ok(true));
    }

    #[test]
    fn a_finished_step_returns_its_value() {
        let bound = Arc::new(Abandoned::new(1));
        let until = Instant::now() + Duration::from_secs(30);
        assert_eq!(step(&bound, until, &flag(), |_| 41 + 1), Ok(42));
        assert_eq!(bound.running(), 0);
    }
}
