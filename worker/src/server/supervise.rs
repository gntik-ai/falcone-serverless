//! Supervised steps: work the execution can leave.
//!
//! Artifact fetch, validation, the cache operation and binding run on a worker thread while the
//! execution waits for the first of its result, the absolute deadline `until`, and the cancel
//! flag. When `until` passes or a cancel arrives the execution leaves at once, whatever the
//! step would later return, and the step's thread is abandoned. An abandoned thread is told to
//! stop through its own flag (a cancel) or through `until` (the deadline) where the step can
//! observe them. Abandoned plus in-flight steps share a bounded reservation: without a free
//! unit a new step is refused as busy before its thread starts.

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
    /// Too many abandoned or in-flight steps, or no thread could be started.
    Busy,
}

/// The bound on abandoned plus in-flight supervised steps.
pub struct Abandoned {
    running: AtomicUsize,
    max: usize,
    #[cfg(any(test, feature = "qualification-hooks"))]
    peak: AtomicUsize,
}

impl Abandoned {
    pub fn new(max: usize) -> Self {
        Self {
            running: AtomicUsize::new(0),
            max,
            #[cfg(any(test, feature = "qualification-hooks"))]
            peak: AtomicUsize::new(0),
        }
    }

    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn running(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }

    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Acquire)
    }

    fn reserve(&self) -> bool {
        match self
            .running
            .try_update(Ordering::AcqRel, Ordering::Acquire, |running| {
                if running < self.max {
                    Some(running + 1)
                } else {
                    None
                }
            }) {
            Ok(_previous) => {
                #[cfg(any(test, feature = "qualification-hooks"))]
                self.peak.fetch_max(_previous + 1, Ordering::AcqRel);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(any(test, feature = "qualification-hooks"))]
impl super::Server {
    /// Peak abandoned plus in-flight supervised steps in this server.
    pub fn supervised_steps_peak(&self) -> usize {
        self.shared.abandoned.peak()
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

/// Transfers release to an abandoned thread; otherwise the waiter releases its reservation.
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
    if !bound.reserve() {
        return Err(busy(until, cancelled));
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
        .map_err(|_| {
            // A failed spawn drops the closure (and its RUNNING tally), so release here.
            bound.running.fetch_sub(1, Ordering::AcqRel);
            busy(until, cancelled)
        })?;
    loop {
        // A result that arrives after the deadline or a cancel is not used.
        if let Some(left) = expired(until, cancelled) {
            abandon(bound, &state, &abandoned, left);
            return Err(left);
        }
        let wait = until.saturating_duration_since(Instant::now()).min(POLL);
        match receive.recv_timeout(wait) {
            Ok(value) => {
                // Work ended, but its tally may not have dropped yet. The waiter owns this
                // unit while RUNNING, and must return it before the next sequential step.
                bound.running.fetch_sub(1, Ordering::AcqRel);
                return match expired(until, cancelled) {
                    Some(left) => Err(left),
                    None => Ok(value),
                };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // A step that set the cancel flag and then died is a cancellation, and one that died
            // past `until` is a timeout: the interruption is looked at again before the exit
            // is mapped to a capacity result.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bound.running.fetch_sub(1, Ordering::AcqRel);
                return Err(busy(until, cancelled));
            }
        }
    }
}

/// Transfers the existing reservation to the thread, or releases it if the thread ended first.
///
/// The step's flag records the reason it was left: it is set for a cancellation and stays clear
/// for a timeout, where the step observes `until` itself. A compile child that is stopped at
/// the deadline is therefore never mistaken for a cancelled one.
fn abandon(bound: &Arc<Abandoned>, state: &AtomicU8, abandoned: &AtomicBool, reason: Left) {
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
    fn concurrent_steps_reserve_capacity_before_their_work_runs() {
        let bound = Arc::new(Abandoned::new(4));
        let until = Instant::now() + Duration::from_secs(30);
        let mut releases = Vec::new();
        for _ in 0..3 {
            let (release, hold) = mpsc::channel::<()>();
            releases.push(release);
            let cancelled = Arc::new(flag());
            let signal = cancelled.clone();
            assert_eq!(
                step(&bound, until, &cancelled, move |_| {
                    signal.store(true, Ordering::Release);
                    let _ = hold.recv();
                }),
                Err(Left::Cancelled)
            );
        }
        assert_eq!(bound.running(), 3);

        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (events, observed) = mpsc::channel();
        let mut pending = Vec::new();
        let mut cancels = Vec::new();
        let mut ran = Vec::new();
        for _ in 0..2 {
            let (release, hold) = mpsc::channel::<()>();
            releases.push(release);
            let cancelled = Arc::new(flag());
            cancels.push(cancelled.clone());
            let started = Arc::new(flag());
            ran.push(started.clone());
            let bound = bound.clone();
            let barrier = barrier.clone();
            let events = events.clone();
            pending.push(std::thread::spawn(move || {
                let began = events.clone();
                barrier.wait();
                let result = step(&bound, until, &cancelled, move |_| {
                    started.store(true, Ordering::Release);
                    began.send(true).unwrap();
                    let _ = hold.recv();
                });
                if result == Err(Left::Busy) {
                    events.send(false).unwrap();
                }
                result
            }));
        }
        barrier.wait();
        let admitted = (0..2)
            .filter(|_| observed.recv_timeout(Duration::from_secs(30)).unwrap())
            .count();
        let in_flight = bound.running();
        for cancel in cancels {
            cancel.store(true, Ordering::Release);
        }
        let results: Vec<_> = pending.into_iter().map(|p| p.join().unwrap()).collect();
        let retained = bound.running();
        let peak = bound.peak();
        drop(releases);
        let limit = Instant::now() + Duration::from_secs(30);
        while bound.running() != 0 {
            assert!(Instant::now() < limit, "steps never released their units");
            std::thread::yield_now();
        }
        assert_eq!(admitted, 1, "only one of the two closures may run");
        assert_eq!(in_flight, 4);
        assert_eq!(retained, 4);
        assert_eq!(peak, 4);
        for (result, started) in results.into_iter().zip(ran) {
            assert_eq!(
                result,
                if started.load(Ordering::Acquire) {
                    Err(Left::Cancelled)
                } else {
                    Err(Left::Busy)
                }
            );
        }
    }

    #[test]
    fn concurrent_spawn_abandon_and_finish_cycles_stay_within_the_bound() {
        let bound = Arc::new(Abandoned::new(2));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut pending = Vec::new();
        for caller in 0..8 {
            let bound = bound.clone();
            let barrier = barrier.clone();
            pending.push(std::thread::spawn(move || {
                barrier.wait();
                for cycle in 0..100 {
                    let cancelled = Arc::new(flag());
                    let signal = cancelled.clone();
                    let (release, hold) = mpsc::channel::<()>();
                    let cancel = (caller + cycle) % 2 == 0;
                    let result = step(
                        &bound,
                        Instant::now() + Duration::from_secs(30),
                        &cancelled,
                        move |_| {
                            if cancel {
                                signal.store(true, Ordering::Release);
                                let _ = hold.recv();
                            } else {
                                std::thread::yield_now();
                            }
                            42
                        },
                    );
                    drop(release);
                    assert!(
                        result == Err(Left::Busy)
                            || result == if cancel { Err(Left::Cancelled) } else { Ok(42) }
                    );
                    assert!(bound.running() <= 2);
                }
            }));
        }
        for thread in pending {
            thread.join().unwrap();
        }
        let until = Instant::now() + Duration::from_secs(30);
        while bound.running() != 0 {
            assert!(Instant::now() < until, "steps never released their units");
            std::thread::yield_now();
        }
        assert!(bound.peak() <= 2);
    }

    #[test]
    fn sequential_steps_never_keep_their_earlier_unit() {
        let bound = Arc::new(Abandoned::new(1));
        let until = Instant::now() + Duration::from_secs(30);
        for _ in 0..1000 {
            assert_eq!(step(&bound, until, &flag(), |_| 42), Ok(42));
            assert_eq!(bound.running(), 0);
            assert_eq!(step(&bound, until, &flag(), |_| 7), Ok(7));
            assert_eq!(bound.running(), 0);
        }
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
