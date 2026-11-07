//! Guest interruption on a shared engine.
//!
//! Epochs are engine-wide and Wasmtime measures a store's deadline from the epoch at the
//! moment it is set. A single-shot per-store timer cannot be relied on: another store's
//! tick can run this store's callback early, and if this store's own tick then lands
//! between the wall-clock check and the re-arm, the new deadline lies past the last tick
//! anyone will ever send, so a guest that never yields runs forever.
//!
//! This module closes that race by construction. One recurring ticker per engine keeps
//! incrementing the epoch for as long as any store holds a pending-deadline registration.
//! Every re-arm is therefore followed by another tick, however the ticks interleave, and
//! the callback interrupts once the wall-clock deadline has passed. The clock and the
//! probe below exist only so a test can force that interleaving; production uses the
//! system clock and no probe.
use super::Host;
use std::io;
use std::sync::{
    Arc, Condvar, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wasmtime::{Config, Engine, Store, UpdateDeadline};

/// Interval of the recurring ticker. It bounds the interruption latency added to the
/// guest deadline and, being coarse, bounds the CPU wakeups the ticker costs.
pub const EPOCH_TICK: Duration = Duration::from_millis(10);

/// An engine with epoch interruption compiled in. Guest interruption is disabled on any
/// engine built without it, such as `Engine::default()`.
pub fn epoch_engine() -> wasmtime::Result<Engine> {
    Engine::new(Config::new().epoch_interruption(true))
}

pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Test seam, called between the wall-clock check and the re-arm. Production has none.
pub trait Probe: Send + Sync {
    fn between_check_and_rearm(&self);
}

struct NoProbe;

impl Probe for NoProbe {
    fn between_check_and_rearm(&self) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Interrupt,
    Rearm,
}

/// The deadline state machine. Expired, or no longer armed, interrupts: a deadline that
/// is not held by a live registration is not backed by the ticker, so it fails closed.
pub fn decide(now: Instant, limit: Instant, armed: bool) -> Decision {
    if armed && now < limit {
        Decision::Rearm
    } else {
        Decision::Interrupt
    }
}

struct Ticking {
    pending: usize,
    closed: bool,
}

struct Shared {
    engine: Engine,
    state: Mutex<Ticking>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Ticking> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The recurring ticker of one engine. Dropping it stops and joins the thread.
pub struct EpochClock {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

struct Registration(Arc<Shared>);

impl Registration {
    fn new(shared: &Arc<Shared>) -> Self {
        let mut state = shared.lock();
        state.pending += 1;
        if state.pending == 1 {
            shared.wake.notify_all();
        }
        Self(shared.clone())
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.0.lock().pending -= 1;
    }
}

/// Holds one pending-deadline registration and the armed flag the callback reads. Keep it
/// alive for the whole guest call; dropping it disarms the store and, when it was the last
/// registration, lets the ticker go idle.
pub struct DeadlineGuard {
    armed: Arc<AtomicBool>,
    _registration: Registration,
}

impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        self.armed.store(false, Ordering::Release);
    }
}

impl EpochClock {
    pub fn start(engine: &Engine) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            engine: engine.clone(),
            state: Mutex::new(Ticking {
                pending: 0,
                closed: false,
            }),
            wake: Condvar::new(),
        });
        let ticker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("falcone-epoch".into())
            .spawn(move || tick(&ticker))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Registrations currently holding the ticker awake; zero means it is idle.
    pub fn pending(&self) -> usize {
        self.shared.lock().pending
    }

    /// Arm the guest deadline of `store` for the call that follows, on the system clock.
    pub fn arm(&self, store: &mut Store<Host>, deadline: Duration) -> DeadlineGuard {
        self.arm_with(store, deadline, Arc::new(SystemClock), Arc::new(NoProbe))
    }

    pub fn arm_with(
        &self,
        store: &mut Store<Host>,
        deadline: Duration,
        clock: Arc<dyn Clock>,
        probe: Arc<dyn Probe>,
    ) -> DeadlineGuard {
        // Register before the first tick can be requested, so the ticker is running for
        // every moment the deadline is pending.
        let registration = Registration::new(&self.shared);
        let armed = Arc::new(AtomicBool::new(true));
        let limit = clock.now() + deadline;
        let live = armed.clone();
        store.epoch_deadline_callback(move |_| {
            Ok(
                match decide(clock.now(), limit, live.load(Ordering::Acquire)) {
                    Decision::Interrupt => UpdateDeadline::Interrupt,
                    Decision::Rearm => {
                        probe.between_check_and_rearm();
                        UpdateDeadline::Continue(1)
                    }
                },
            )
        });
        store.set_epoch_deadline(1);
        DeadlineGuard {
            armed,
            _registration: registration,
        }
    }
}

impl Drop for EpochClock {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn tick(shared: &Shared) {
    let mut state = shared.lock();
    loop {
        if state.closed {
            return;
        }
        if state.pending == 0 {
            state = shared
                .wake
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        let (next, timeout) = shared
            .wake
            .wait_timeout(state, EPOCH_TICK)
            .unwrap_or_else(PoisonError::into_inner);
        state = next;
        if timeout.timed_out() && !state.closed && state.pending > 0 {
            shared.engine.increment_epoch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_or_disarmed_interrupts_and_only_a_live_unexpired_deadline_rearms() {
        let start = Instant::now();
        let limit = start + Duration::from_secs(5);
        assert_eq!(decide(start, limit, true), Decision::Rearm);
        assert_eq!(
            decide(limit - Duration::from_nanos(1), limit, true),
            Decision::Rearm
        );
        assert_eq!(decide(limit, limit, true), Decision::Interrupt);
        assert_eq!(
            decide(limit + Duration::from_secs(1), limit, true),
            Decision::Interrupt
        );
        // Not backed by a registration: no tick is guaranteed, so never re-arm.
        assert_eq!(decide(start, limit, false), Decision::Interrupt);
    }

    #[test]
    fn the_ticker_is_idle_without_registrations_and_wakes_for_each_one() {
        let engine = epoch_engine().unwrap();
        let clock = EpochClock::start(&engine).unwrap();
        assert_eq!(clock.pending(), 0);
        let first = Registration::new(&clock.shared);
        let second = Registration::new(&clock.shared);
        assert_eq!(clock.pending(), 2);
        drop(first);
        assert_eq!(clock.pending(), 1);
        drop(second);
        assert_eq!(clock.pending(), 0);
    }
}
