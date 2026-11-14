//! The worker's execution service: ctr-worker-execution-api v1 over two pod-local listeners.
//!
//! - The **execute listener** serves only `POST /v1/execute`. At most `concurrency` executions
//!   run at once, beside a small fixed number of connections still in admission. A connection
//!   or execution over capacity is answered at once with `503 busy`; nothing is evicted.
//! - The **control listener** serves only `POST /v1/cancel` and `POST /v1/update`. It has its
//!   own fixed connection limit and its own deadlines, so no execute traffic, stalled or busy,
//!   can take control capacity: the two share nothing but the registry of running executions.
//!
//! Each listener answers `404` for the other's operations. Both read requests under fixed
//! size and time bounds (`http`), and write each response under one absolute deadline.
//!
//! One absolute `until` instant is fixed when an execution is admitted and is the only
//! deadline: it bounds artifact loading, validation, compilation and publication, the platform
//! binding, instantiation, every host call and the guest itself (`run`).

pub mod http;
pub mod platform;
pub mod redact;
mod run;

use crate::api::{self, CancelState, Code, Reason, Refusal, WorkerLimits};
use crate::cache::Cache;
use http::{MAX_CONTROL_BODY, MAX_REQUEST, ReadError, Timing};
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Connections the execute listener holds beside its running executions: requests whose head
/// or body is still arriving.
pub const ADMISSION_CONNECTIONS: usize = 16;
/// The control listener's own connection limit.
pub const CONTROL_CONNECTIONS: usize = 16;

/// A probe called by test and qualification builds at a fixed point of the service.
#[cfg(any(test, feature = "qualification"))]
pub type Probe = Arc<dyn Fn() + Send + Sync>;

/// Test and qualification builds only: pause or observe the service at fixed points. Release
/// builds contain none of this.
#[cfg(any(test, feature = "qualification"))]
#[derive(Clone, Default)]
pub struct Hooks {
    /// Called when platform binding starts, inside the supervised setup.
    pub bind: Option<Probe>,
    /// Called for every operation that reaches the platform bindings.
    pub call: Option<Probe>,
    /// Called at every guest-to-host entry with the entries counted so far.
    pub entry: Option<crate::host::EntryHook>,
    /// Called at each step of the cache operation.
    pub stage: Option<crate::cache::StageHook>,
    /// Called after instantiation, before the guest-start deadline check.
    pub start: Option<crate::host::StartHook>,
}

pub struct Settings {
    pub worker: WorkerLimits,
    /// Executions that may run at once.
    pub concurrency: usize,
    /// Pod-local directory of raw artifacts, `<sha256 hex>.wasm`.
    pub artifact_dir: PathBuf,
    pub cache_dir: PathBuf,
    /// The trusted compile child, beside the worker executable.
    pub compiler: PathBuf,
    pub execute_timing: Timing,
    pub control_timing: Timing,
    /// How long a cancel waits for the execution to end before it answers `uncertain`.
    pub termination_budget: Duration,
    #[cfg(any(test, feature = "qualification"))]
    pub hooks: Hooks,
}

impl Settings {
    pub fn new(
        worker: WorkerLimits,
        concurrency: usize,
        artifact_dir: PathBuf,
        cache_dir: PathBuf,
        compiler: PathBuf,
    ) -> Self {
        Self {
            worker,
            concurrency: concurrency.max(1),
            artifact_dir,
            cache_dir,
            compiler,
            execute_timing: Timing::EXECUTE,
            control_timing: Timing::CONTROL,
            termination_budget: crate::limits::Limits::compatibility()
                .duration(crate::limits::Field::TerminationBudgetMs),
            #[cfg(any(test, feature = "qualification"))]
            hooks: Hooks::default(),
        }
    }
}

/// A counted capacity: `try_acquire` never waits.
struct Slots {
    limit: usize,
    used: AtomicUsize,
}

struct Slot(Arc<Slots>);

impl Slots {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
        })
    }

    fn try_acquire(self: &Arc<Self>) -> Option<Slot> {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            if used >= self.limit {
                return None;
            }
            match self.used.compare_exchange_weak(
                used,
                used + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Slot(self.clone())),
                Err(current) => used = current,
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.used.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Why a supervised step ended without a result.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stop {
    Timeout,
    Cancelled,
    /// No capacity for another abandonable step.
    Busy,
    /// The step panicked or could not start.
    Failed,
}

/// Steps the execution can abandon: artifact loading, validation and cache work, and platform
/// binding. A step runs on its own thread; the execution waits for it only until `until` or
/// cancellation. An abandoned step keeps its capacity until it ends, so abandoned work is
/// bounded and cannot pile up.
struct Jobs {
    slots: Arc<Slots>,
}

impl Jobs {
    fn run<T: Send + 'static>(
        &self,
        until: Instant,
        cancelled: &AtomicBool,
        step: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Stop> {
        let slot = self.slots.try_acquire().ok_or(Stop::Busy)?;
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("execution-step".into())
            .spawn(move || {
                let _slot = slot;
                let _ = send.send(step());
            })
            .map_err(|_| Stop::Busy)?;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(Stop::Cancelled);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Stop::Timeout);
            }
            match receive.recv_timeout(left.min(Duration::from_millis(1))) {
                // Whatever the step returned, an expired or cancelled execution is reported as
                // such: the step's own verdict never replaces it.
                Ok(value) => {
                    return if cancelled.load(Ordering::Acquire) {
                        Err(Stop::Cancelled)
                    } else if Instant::now() >= until {
                        Err(Stop::Timeout)
                    } else {
                        Ok(value)
                    };
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(Stop::Failed),
            }
        }
    }

    fn active(&self) -> usize {
        self.slots.used.load(Ordering::Acquire)
    }
}

struct EntryState {
    finished: bool,
    uncertain: bool,
    last_sequence: u64,
}

/// One running execution, as cancel and update see it.
struct Entry {
    cancel: Arc<AtomicBool>,
    state: Mutex<EntryState>,
    done: Condvar,
}

type Key = (String, String);

/// The running executions by activation and attempt.
#[derive(Default)]
struct Registry {
    entries: Mutex<HashMap<Key, Arc<Entry>>>,
}

/// Ownership of a registration: dropping it unregisters the execution and releases any cancel
/// waiting on it.
struct Running {
    registry: Arc<Registry>,
    key: Key,
    entry: Arc<Entry>,
}

impl Registry {
    fn register(self: &Arc<Self>, activation: &str, attempt: &str) -> Option<Running> {
        let key = (activation.to_owned(), attempt.to_owned());
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.contains_key(&key) {
            return None;
        }
        let entry = Arc::new(Entry {
            cancel: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(EntryState {
                finished: false,
                uncertain: false,
                last_sequence: 0,
            }),
            done: Condvar::new(),
        });
        entries.insert(key.clone(), entry.clone());
        Some(Running {
            registry: self.clone(),
            key,
            entry,
        })
    }

    fn find(&self, activation: &str, attempt: &str) -> Option<Arc<Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(activation.to_owned(), attempt.to_owned()))
            .cloned()
    }

    /// Cancels an execution and waits at most `budget` for it to end.
    fn cancel(&self, activation: &str, attempt: &str, budget: Duration) -> Option<CancelState> {
        let entry = self.find(activation, attempt)?;
        entry.cancel.store(true, Ordering::Release);
        let deadline = Instant::now() + budget;
        let mut state = entry.state.lock().unwrap_or_else(|e| e.into_inner());
        while !state.finished {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Some(CancelState::Uncertain);
            }
            state = entry
                .done
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        Some(if state.uncertain {
            CancelState::Uncertain
        } else {
            CancelState::Acknowledged
        })
    }

    fn update(&self, activation: &str, attempt: &str, sequence: u64) -> Result<(), Code> {
        let entry = self.find(activation, attempt).ok_or(Code::NotFound)?;
        let mut state = entry.state.lock().unwrap_or_else(|e| e.into_inner());
        if sequence <= state.last_sequence {
            return Err(Code::StaleUpdate);
        }
        state.last_sequence = sequence;
        Ok(())
    }
}

impl Running {
    fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.entry.cancel.clone()
    }

    /// Marks the execution ended; `uncertain` when a mutation may still commit.
    fn finish(&self, uncertain: bool) {
        let mut state = self.entry.state.lock().unwrap_or_else(|e| e.into_inner());
        state.finished = true;
        state.uncertain = uncertain;
        self.entry.done.notify_all();
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        {
            let mut state = self.entry.state.lock().unwrap_or_else(|e| e.into_inner());
            state.finished = true;
            self.entry.done.notify_all();
        }
        self.registry
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

/// Everything the connections share.
struct Shared {
    settings: Settings,
    cache: Arc<Cache>,
    registry: Arc<Registry>,
    executing: Arc<Slots>,
    jobs: Jobs,
    #[cfg(any(test, feature = "qualification"))]
    marks: Marks,
    /// Test and qualification builds only: responses cut off at their absolute deadline.
    #[cfg(any(test, feature = "qualification"))]
    cut_offs: AtomicUsize,
}

impl Shared {
    /// Writes one response under its absolute deadline. A peer that has not taken it by then is
    /// cut off (counted in test and qualification builds).
    fn respond(&self, stream: &mut TcpStream, status: u16, body: &[u8], deadline: Duration) {
        let result = http::respond(stream, status, body, deadline);
        #[cfg(any(test, feature = "qualification"))]
        if result.is_err() {
            self.cut_offs.fetch_add(1, Ordering::AcqRel);
        }
        #[cfg(not(any(test, feature = "qualification")))]
        let _ = result;
    }
}

/// Test and qualification builds only: the server's own clock readings for the last execution,
/// so a termination bound is measured on the server, apart from any transport delay.
#[cfg(any(test, feature = "qualification"))]
#[derive(Default)]
struct Marks {
    until: Mutex<Option<Instant>>,
    cancel: Mutex<Option<Instant>>,
    released: Mutex<Option<Instant>>,
}

#[cfg(any(test, feature = "qualification"))]
impl Marks {
    fn set(mark: &Mutex<Option<Instant>>, at: Instant) {
        *mark.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
    }

    fn get(mark: &Mutex<Option<Instant>>) -> Option<Instant> {
        *mark.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn unix_now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The refusal envelope on its own, for answers that have no activation to attribute to.
fn bare(refusal: &Refusal) -> Vec<u8> {
    serde_json::to_vec(refusal).unwrap_or_default()
}

fn refuse(code: Code, reason: Reason) -> (u16, Vec<u8>) {
    let refusal = Refusal::new(code, reason);
    (refusal.status(), bare(&refusal))
}

/// A running server: both listeners and their accept loops.
pub struct Server {
    shared: Arc<Shared>,
    execute_addr: SocketAddr,
    control_addr: SocketAddr,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    /// Test and qualification builds only: the execute listener's connection permits.
    #[cfg(any(test, feature = "qualification"))]
    execute_connections: Arc<Slots>,
}

impl Server {
    pub fn start(settings: Settings, execute: SocketAddr, control: SocketAddr) -> io::Result<Self> {
        let cache = Cache::new(&settings.cache_dir, &settings.compiler)
            .map_err(|error| io::Error::other(error.code))?;
        #[cfg(any(test, feature = "qualification"))]
        let cache = {
            let mut cache = cache;
            if let Some(hook) = settings.hooks.stage.clone() {
                cache.set_stage_hook(hook);
            }
            cache
        };
        let execute_listener = TcpListener::bind(execute)?;
        let control_listener = TcpListener::bind(control)?;
        let (execute_addr, control_addr) = (
            execute_listener.local_addr()?,
            control_listener.local_addr()?,
        );
        let concurrency = settings.concurrency;
        let shared = Arc::new(Shared {
            cache: Arc::new(cache),
            registry: Arc::new(Registry::default()),
            executing: Slots::new(concurrency),
            jobs: Jobs {
                slots: Slots::new(2 * concurrency + 2),
            },
            #[cfg(any(test, feature = "qualification"))]
            marks: Marks::default(),
            #[cfg(any(test, feature = "qualification"))]
            cut_offs: AtomicUsize::new(0),
            settings,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let execute_connections = Slots::new(concurrency + ADMISSION_CONNECTIONS);
        let control_connections = Slots::new(CONTROL_CONNECTIONS);
        let threads = vec![
            accept_loop(
                execute_listener,
                execute_connections.clone(),
                stop.clone(),
                shared.clone(),
                execute_connection,
            ),
            accept_loop(
                control_listener,
                control_connections,
                stop.clone(),
                shared.clone(),
                control_connection,
            ),
        ];
        Ok(Self {
            shared,
            execute_addr,
            control_addr,
            stop,
            threads,
            #[cfg(any(test, feature = "qualification"))]
            execute_connections,
        })
    }

    pub fn execute_addr(&self) -> SocketAddr {
        self.execute_addr
    }

    pub fn control_addr(&self) -> SocketAddr {
        self.control_addr
    }

    /// Executions currently holding an execution slot.
    pub fn running_executions(&self) -> usize {
        self.shared.executing.used.load(Ordering::Acquire)
    }

    /// Test and qualification builds only: connections currently holding an execute-listener
    /// permit, whatever they are doing.
    #[cfg(any(test, feature = "qualification"))]
    pub fn execute_connections(&self) -> usize {
        self.execute_connections.used.load(Ordering::Acquire)
    }

    /// Test and qualification builds only: responses the server cut off at their absolute
    /// deadline because the peer had not taken them in time.
    #[cfg(any(test, feature = "qualification"))]
    pub fn responses_cut_off(&self) -> usize {
        self.shared.cut_offs.load(Ordering::Acquire)
    }

    /// Abandonable steps (artifact, cache and binding work) that have not ended.
    pub fn active_steps(&self) -> usize {
        self.shared.jobs.active()
    }

    pub fn cache_dir(&self) -> &std::path::Path {
        &self.shared.settings.cache_dir
    }

    /// Test and qualification builds only: forgets the readings of earlier executions.
    #[cfg(any(test, feature = "qualification"))]
    pub fn reset_marks(&self) {
        let marks = &self.shared.marks;
        for mark in [&marks.until, &marks.cancel, &marks.released] {
            *mark.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// Test and qualification builds only: how long after its absolute deadline (`until`) the
    /// last execution released its slot, by the server's own clock. `None` until released.
    #[cfg(any(test, feature = "qualification"))]
    pub fn release_after_deadline(&self) -> Option<Duration> {
        let (until, released) = (
            Marks::get(&self.shared.marks.until)?,
            Marks::get(&self.shared.marks.released)?,
        );
        Some(released.saturating_duration_since(until))
    }

    /// Test and qualification builds only: how long after the last accepted cancel the slot was
    /// released, by the server's own clock. `None` until both have happened.
    #[cfg(any(test, feature = "qualification"))]
    pub fn release_after_cancel(&self) -> Option<Duration> {
        let (cancel, released) = (
            Marks::get(&self.shared.marks.cancel)?,
            Marks::get(&self.shared.marks.released)?,
        );
        Some(released.saturating_duration_since(cancel))
    }

    /// Test and qualification builds only: artifacts validated by this server's cache.
    #[cfg(any(test, feature = "qualification"))]
    pub fn validations(&self) -> usize {
        self.shared.cache.validations()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake each blocked accept so its loop sees the flag.
        let _ = TcpStream::connect(self.execute_addr);
        let _ = TcpStream::connect(self.control_addr);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

type Handler = fn(Arc<Shared>, TcpStream, Slot);

fn accept_loop(
    listener: TcpListener,
    connections: Arc<Slots>,
    stop: Arc<AtomicBool>,
    shared: Arc<Shared>,
    handler: Handler,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let Ok(mut stream) = stream else { continue };
            let Some(slot) = connections.try_acquire() else {
                // Over capacity: answered at once, nothing read, nothing evicted.
                let (status, body) = refuse(Code::Busy, Reason::AtCapacity);
                http::respond_now(&mut stream, status, &body);
                continue;
            };
            let shared = shared.clone();
            let _ = std::thread::Builder::new()
                .name("connection".into())
                .spawn(move || handler(shared, stream, slot));
        }
    })
}

/// Answers a head that could not be read as a request, or closes the connection.
fn head_error(stream: &mut TcpStream, error: ReadError, timeout: Duration) {
    match error {
        ReadError::Closed => {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        ReadError::HeadTooLarge => {
            let (status, body) = refuse(Code::RequestTooLarge, Reason::BodyTooLarge);
            let _ = http::respond(stream, status, &body, timeout);
        }
        ReadError::Malformed => {
            let (status, body) = refuse(Code::InvalidRequest, Reason::MalformedRequest);
            let _ = http::respond(stream, status, &body, timeout);
        }
    }
}

fn execute_connection(shared: Arc<Shared>, mut stream: TcpStream, _connection: Slot) {
    let timing = shared.settings.execute_timing;
    let (head, mut body) = match http::read_head(&mut stream, &timing) {
        Ok(read) => read,
        Err(error) => return head_error(&mut stream, error, timing.response_deadline),
    };
    let respond = |stream: &mut TcpStream, (status, body): (u16, Vec<u8>)| {
        shared.respond(stream, status, &body, timing.response_deadline);
    };
    if head.path != "/v1/execute" {
        return respond(
            &mut stream,
            refuse(Code::NotFound, Reason::ExecutionNotFound),
        );
    }
    if head.method != "POST" {
        return respond(
            &mut stream,
            refuse(Code::InvalidRequest, Reason::MalformedRequest),
        );
    }
    if head.content_length > MAX_REQUEST {
        return respond(
            &mut stream,
            refuse(Code::RequestTooLarge, Reason::BodyTooLarge),
        );
    }
    // Capacity is checked before the body is read.
    let Some(slot) = shared.executing.try_acquire() else {
        let reply = run::busy(&mut stream, &mut body, head.content_length);
        return respond(&mut stream, reply);
    };
    let body = match http::read_body(&mut stream, body, head.content_length, &timing) {
        Ok(body) => body,
        Err(error) => return head_error(&mut stream, error, timing.response_deadline),
    };
    // The execution slot is released inside `execute`, before it reports itself finished.
    let reply = run::execute(&shared, &body, slot);
    respond(&mut stream, reply);
}

fn control_connection(shared: Arc<Shared>, mut stream: TcpStream, _connection: Slot) {
    let timing = shared.settings.control_timing;
    let (head, prefix) = match http::read_head(&mut stream, &timing) {
        Ok(read) => read,
        Err(error) => return head_error(&mut stream, error, timing.response_deadline),
    };
    let respond = |stream: &mut TcpStream, (status, body): (u16, Vec<u8>)| {
        shared.respond(stream, status, &body, timing.response_deadline);
    };
    let operation = match (head.method.as_str(), head.path.as_str()) {
        ("POST", "/v1/cancel") => Control::Cancel,
        ("POST", "/v1/update") => Control::Update,
        _ => {
            return respond(
                &mut stream,
                refuse(Code::NotFound, Reason::ExecutionNotFound),
            );
        }
    };
    if head.content_length > MAX_CONTROL_BODY {
        return respond(
            &mut stream,
            refuse(Code::RequestTooLarge, Reason::BodyTooLarge),
        );
    }
    let body = match http::read_body(&mut stream, prefix, head.content_length, &timing) {
        Ok(body) => body,
        Err(error) => return head_error(&mut stream, error, timing.response_deadline),
    };
    respond(&mut stream, control(&shared, operation, &body));
}

enum Control {
    Cancel,
    Update,
}

fn control(shared: &Shared, operation: Control, body: &[u8]) -> (u16, Vec<u8>) {
    let malformed = || refuse(Code::InvalidRequest, Reason::MalformedRequest);
    match operation {
        Control::Cancel => {
            let Ok(message) = api::parse_cancel(body) else {
                return malformed();
            };
            if message.state() != CancelState::Requested {
                return malformed();
            }
            #[cfg(any(test, feature = "qualification"))]
            Marks::set(&shared.marks.cancel, Instant::now());
            match shared.registry.cancel(
                message.activation_id().as_str(),
                message.attempt_id().as_str(),
                shared.settings.termination_budget,
            ) {
                Some(state) => (
                    200,
                    serde_json::to_vec(&message.reply(state)).unwrap_or_default(),
                ),
                None => refuse(Code::NotFound, Reason::ExecutionNotFound),
            }
        }
        Control::Update => {
            let Ok(message) = api::parse_update(body) else {
                return malformed();
            };
            match shared.registry.update(
                message.activation_id().as_str(),
                message.attempt_id().as_str(),
                message.sequence(),
            ) {
                Ok(()) => (200, serde_json::to_vec(&message).unwrap_or_default()),
                Err(Code::StaleUpdate) => refuse(Code::StaleUpdate, Reason::SequenceNotNewer),
                Err(_) => refuse(Code::NotFound, Reason::ExecutionNotFound),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_never_wait_and_are_released_on_drop() {
        let slots = Slots::new(2);
        let first = slots.try_acquire().unwrap();
        let second = slots.try_acquire().unwrap();
        assert!(slots.try_acquire().is_none());
        drop(first);
        let third = slots.try_acquire().unwrap();
        assert!(slots.try_acquire().is_none());
        drop((second, third));
        assert_eq!(slots.used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn the_registry_refuses_duplicates_orders_updates_and_cancels() {
        let registry = Arc::new(Registry::default());
        assert!(registry.cancel("a", "1", Duration::ZERO).is_none());
        let running = registry.register("a", "1").unwrap();
        assert!(
            registry.register("a", "1").is_none(),
            "one running attempt per pair"
        );
        assert!(registry.register("a", "2").is_some());
        assert_eq!(registry.update("a", "1", 1), Ok(()));
        assert_eq!(registry.update("a", "1", 1), Err(Code::StaleUpdate));
        assert_eq!(registry.update("a", "1", 3), Ok(()));
        assert_eq!(registry.update("a", "1", 2), Err(Code::StaleUpdate));
        assert_eq!(registry.update("b", "1", 1), Err(Code::NotFound));
        // A cancel that outlives its budget is uncertain; one that sees the end is acknowledged.
        assert_eq!(
            registry.cancel("a", "1", Duration::from_millis(5)),
            Some(CancelState::Uncertain)
        );
        assert!(running.cancel_flag().load(Ordering::Acquire));
        let waiting = {
            let registry = registry.clone();
            std::thread::spawn(move || registry.cancel("a", "1", Duration::from_secs(30)))
        };
        while Arc::strong_count(&running.entry) < 3 {
            std::thread::yield_now();
        }
        running.finish(false);
        assert_eq!(waiting.join().unwrap(), Some(CancelState::Acknowledged));
        drop(running);
        assert!(
            registry.cancel("a", "1", Duration::ZERO).is_none(),
            "unregistered on drop"
        );
    }

    #[test]
    fn an_uncertain_finish_is_reported_as_uncertain() {
        let registry = Arc::new(Registry::default());
        let running = registry.register("a", "1").unwrap();
        running.finish(true);
        assert_eq!(
            registry.cancel("a", "1", Duration::from_secs(5)),
            Some(CancelState::Uncertain)
        );
    }

    #[test]
    fn steps_end_at_the_deadline_or_cancel_whatever_they_return_and_are_bounded() {
        let jobs = Jobs {
            slots: Slots::new(1),
        };
        let never = AtomicBool::new(false);
        assert_eq!(
            jobs.run(Instant::now() + Duration::from_secs(30), &never, || 5),
            Ok(5)
        );
        // A step that outlives the deadline is abandoned at it, and keeps its capacity until it
        // ends, so abandoned work is bounded.
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let started = Instant::now();
        let stopped = jobs.run(
            Instant::now() + Duration::from_millis(20),
            &never,
            move || {
                let _ = hold.recv();
                7
            },
        );
        assert_eq!(stopped, Err(Stop::Timeout));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(jobs.active(), 1);
        assert_eq!(
            jobs.run(Instant::now() + Duration::from_secs(30), &never, || 1),
            Err(Stop::Busy)
        );
        drop(release);
        while jobs.active() > 0 {
            std::thread::yield_now();
        }
        // Cancellation wins over a step's own result.
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            jobs.run(Instant::now() + Duration::from_secs(30), &cancelled, || 9),
            Err(Stop::Cancelled)
        );
        // An expired deadline wins over a step that has already returned.
        while jobs.active() > 0 {
            std::thread::yield_now();
        }
        assert_eq!(jobs.run(Instant::now(), &never, || 9), Err(Stop::Timeout));
    }
}
