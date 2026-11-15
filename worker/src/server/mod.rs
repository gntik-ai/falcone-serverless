//! The worker's HTTP execution API (`ctr-worker-execution-api` v1).
//!
//! Two pod-local listeners with independent, fixed limits and deadlines:
//!
//! - the **execute listener** serves only `POST /v1/execute`;
//! - the **control listener** serves only `POST /v1/cancel` and `POST /v1/update`.
//!
//! Each refuses the other's operations, and every other path, with `404 not_found`. Capacity
//! is by construction separate: no execute traffic can take control capacity, so a cancel
//! reaches a running execution however full the execute listener is. Nothing is evicted.
//!
//! Admission order on the execute listener, for a request whose headers arrived: a connection
//! over the listener's fixed limit is answered `503 busy` at once. Otherwise, with every
//! execution slot busy, the body is not waited for: only bytes already received are looked
//! at, a non-`wasm` runtime mode is still refused `409 runtime_mode_not_supported`, and any
//! other request gets `503 busy`.

mod http;
mod platform;
mod redact;
mod run;
mod supervise;

pub use http::Deadlines;
pub use platform::{LocalPlatform, Platform, Secret, Unavailable};
pub use redact::{LINE_CHARS, MARKER, MAX_LINES, Redactor, SCAN_BUDGET_CHARS};
pub use supervise::Abandoned;

use crate::api::{self, Ceilings, Code, Reason, Refusal};
use crate::cache::Cache;
use http::{Head, Unreadable};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// The documented termination budget: how long a cancel waits for its execution to end
/// before it answers `uncertain`, and the bound on releasing a slot after `until` or a cancel.
pub const TERMINATION_BUDGET: Duration = Duration::from_millis(500);

/// Connections that may be in admission beyond the running executions on the execute listener.
pub const ADMISSION_CONNECTIONS: usize = 4;
/// The control listener's fixed connection limit.
pub const CONTROL_CONNECTIONS: usize = 16;

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub execute_addr: SocketAddr,
    pub control_addr: SocketAddr,
    /// At most this many executions run at once.
    pub concurrency: usize,
    pub ceilings: Ceilings,
    pub cache_dir: std::path::PathBuf,
    /// The trusted compile child, beside the worker binary.
    pub compiler: std::path::PathBuf,
    pub execute_deadlines: Deadlines,
    pub control_deadlines: Deadlines,
    pub admission_connections: usize,
    pub control_connections: usize,
    /// Abandoned supervised steps that may still be running.
    pub abandoned_steps: usize,
    pub termination_budget: Duration,
}

impl ServerConfig {
    pub fn new(
        execute_addr: SocketAddr,
        control_addr: SocketAddr,
        concurrency: usize,
        ceilings: Ceilings,
        cache_dir: std::path::PathBuf,
        compiler: std::path::PathBuf,
    ) -> Self {
        Self {
            execute_addr,
            control_addr,
            concurrency: concurrency.max(1),
            ceilings,
            cache_dir,
            compiler,
            execute_deadlines: Deadlines::EXECUTE,
            control_deadlines: Deadlines::CONTROL,
            admission_connections: ADMISSION_CONNECTIONS,
            control_connections: CONTROL_CONNECTIONS,
            abandoned_steps: concurrency.max(4),
            termination_budget: TERMINATION_BUDGET,
        }
    }
}

/// A counter with a fixed maximum. A permit releases on drop and wakes waiters.
struct Gauge {
    count: Mutex<usize>,
    changed: Condvar,
    max: usize,
}

struct Permit(Arc<Gauge>);

impl Gauge {
    fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            count: Mutex::new(0),
            changed: Condvar::new(),
            max,
        })
    }

    fn acquire(self: &Arc<Self>) -> Option<Permit> {
        let mut count = self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *count >= self.max {
            return None;
        }
        *count += 1;
        self.changed.notify_all();
        Some(Permit(self.clone()))
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut count = self
            .0
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *count -= 1;
        self.0.changed.notify_all();
    }
}

#[cfg(any(test, feature = "qualification-hooks"))]
impl Gauge {
    fn get(&self) -> usize {
        *self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits until the count is `wanted`; returns the time it took, or `None` on timeout.
    fn wait_for(&self, wanted: usize, within: Duration) -> Option<Duration> {
        let started = std::time::Instant::now();
        let mut count = self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if *count == wanted {
                return Some(started.elapsed());
            }
            let remaining = within.checked_sub(started.elapsed())?;
            count = self
                .changed
                .wait_timeout(count, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// One running execution, as the control listener sees it.
pub(crate) struct Entry {
    pub(crate) cancel: Arc<AtomicBool>,
    last_sequence: AtomicU64,
    done: Mutex<bool>,
    finished: Condvar,
}

impl Entry {
    fn wait_finished(&self, within: Duration) -> bool {
        let done = self
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *self
            .finished
            .wait_timeout_while(done, within, |done| !*done)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
    }
}

/// Test-only events and counters. They exist only under `cfg(test)` or the
/// `qualification-hooks` feature and are absent from every release build.
#[cfg(any(test, feature = "qualification-hooks"))]
pub mod hooks {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::{Arc, Mutex};

    type Callback = Arc<dyn Fn() + Send + Sync>;
    type BuildCallback = Arc<dyn Fn(&AtomicBool) + Send + Sync>;

    #[derive(Default)]
    pub struct Hooks {
        setup: Mutex<Option<Callback>>,
        build: Mutex<Option<BuildCallback>>,
        guest_start: Mutex<Option<Callback>>,
        /// Executions that instantiated their guest and were about to invoke its handler. This is
        /// not proof that guest code ran; tests wait for the guest's own first action.
        pub guests_started: AtomicUsize,
    }

    impl Hooks {
        /// Runs in the supervised setup step, before the credential is bound.
        pub fn on_setup(&self, callback: impl Fn() + Send + Sync + 'static) {
            *self
                .setup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(callback));
        }

        /// Runs while the redaction matcher is being built, each time the build takes the next
        /// credential claim, with the execution's cancel flag.
        pub fn on_build(&self, callback: impl Fn(&AtomicBool) + Send + Sync + 'static) {
            *self
                .build
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(callback));
        }

        /// Runs after instantiation, right before the handler is invoked.
        pub fn on_guest_start(&self, callback: impl Fn() + Send + Sync + 'static) {
            *self
                .guest_start
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(callback));
        }

        pub(crate) fn setup(&self) {
            let callback = self
                .setup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(callback) = callback {
                callback();
            }
        }

        pub(crate) fn build(&self, cancelled: &AtomicBool) {
            let callback = self
                .build
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(callback) = callback {
                callback(cancelled);
            }
        }

        pub(crate) fn guest_start(&self) {
            self.guests_started
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let callback = self
                .guest_start
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(callback) = callback {
                callback();
            }
        }
    }
}

pub(crate) struct Shared {
    pub(crate) config: ServerConfig,
    pub(crate) platform: Arc<dyn Platform>,
    cache: Mutex<Cache>,
    pub(crate) abandoned: Arc<Abandoned>,
    running: Arc<Gauge>,
    execute_connections: Arc<Gauge>,
    control_connections: Arc<Gauge>,
    registry: Mutex<HashMap<(String, String), Arc<Entry>>>,
    stop: AtomicBool,
    #[cfg(any(test, feature = "qualification-hooks"))]
    pub(crate) hooks: Arc<hooks::Hooks>,
}

impl Shared {
    pub(crate) fn cache(&self) -> Cache {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .fork()
    }

    /// Registers an execution for the control listener. `None` when that attempt is already
    /// running.
    pub(crate) fn register(self: &Arc<Self>, key: (String, String)) -> Option<Registration> {
        let entry = Arc::new(Entry {
            cancel: Arc::new(AtomicBool::new(false)),
            last_sequence: AtomicU64::new(0),
            done: Mutex::new(false),
            finished: Condvar::new(),
        });
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if registry.contains_key(&key) {
            return None;
        }
        registry.insert(key.clone(), entry.clone());
        Some(Registration {
            shared: self.clone(),
            key,
            entry,
        })
    }

    fn lookup(&self, activation: &str, attempt: &str) -> Option<Arc<Entry>> {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(activation.to_owned(), attempt.to_owned()))
            .cloned()
    }
}

/// An execution's entry in the registry; dropping it ends the execution for the control
/// listener and wakes a waiting cancel.
pub(crate) struct Registration {
    shared: Arc<Shared>,
    key: (String, String),
    pub(crate) entry: Arc<Entry>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.shared
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
        *self
            .entry
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.entry.finished.notify_all();
    }
}

/// The running worker API. Dropping it stops accepting; running executions finish on their own
/// threads.
pub struct Server {
    shared: Arc<Shared>,
    execute_addr: SocketAddr,
    control_addr: SocketAddr,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn start(config: ServerConfig, platform: Arc<dyn Platform>) -> std::io::Result<Self> {
        let execute = TcpListener::bind(config.execute_addr)?;
        let control = TcpListener::bind(config.control_addr)?;
        let cache = Cache::new(&config.cache_dir, &config.compiler)
            .map_err(|error| std::io::Error::other(error.code))?;
        let shared = Arc::new(Shared {
            platform,
            cache: Mutex::new(cache),
            abandoned: Arc::new(Abandoned::new(config.abandoned_steps)),
            running: Gauge::new(config.concurrency),
            execute_connections: Gauge::new(config.concurrency + config.admission_connections),
            control_connections: Gauge::new(config.control_connections),
            registry: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            #[cfg(any(test, feature = "qualification-hooks"))]
            hooks: Arc::default(),
            config,
        });
        let execute_addr = execute.local_addr()?;
        let control_addr = control.local_addr()?;
        let mut threads = Vec::new();
        for (listener, control) in [(execute, false), (control, true)] {
            let shared = shared.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(
                        if control {
                            "control-accept"
                        } else {
                            "execute-accept"
                        }
                        .into(),
                    )
                    .spawn(move || accept(listener, shared, control))?,
            );
        }
        Ok(Self {
            shared,
            execute_addr,
            control_addr,
            threads,
        })
    }

    pub fn execute_addr(&self) -> SocketAddr {
        self.execute_addr
    }

    pub fn control_addr(&self) -> SocketAddr {
        self.control_addr
    }

    /// The cache directory's stage hooks and validation counter.
    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn cache_hooks(&self) -> Arc<crate::cache::hooks::Hooks> {
        self.shared.cache().hooks()
    }

    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn hooks(&self) -> Arc<hooks::Hooks> {
        self.shared.hooks.clone()
    }

    /// Executions running now.
    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn running(&self) -> usize {
        self.shared.running.get()
    }

    /// Connections held now on the execute and the control listener.
    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn connections(&self) -> (usize, usize) {
        (
            self.shared.execute_connections.get(),
            self.shared.control_connections.get(),
        )
    }

    /// Abandoned supervised steps still running.
    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn abandoned_running(&self) -> usize {
        self.shared.abandoned.running()
    }

    /// Waits for exactly `wanted` running executions; the time it took, or `None`.
    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn wait_running(&self, wanted: usize, within: Duration) -> Option<Duration> {
        self.shared.running.wait_for(wanted, within)
    }

    /// Waits for exactly `wanted` connections on (execute, control).
    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn wait_connections(&self, wanted: (usize, usize), within: Duration) -> bool {
        self.shared
            .execute_connections
            .wait_for(wanted.0, within)
            .is_some()
            && self
                .shared
                .control_connections
                .wait_for(wanted.1, within)
                .is_some()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // Wake each accept loop so it observes the stop flag.
        for addr in [self.execute_addr, self.control_addr] {
            let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn accept(listener: TcpListener, shared: Arc<Shared>, control: bool) {
    let (connections, deadlines) = if control {
        (
            shared.control_connections.clone(),
            shared.config.control_deadlines,
        )
    } else {
        (
            shared.execute_connections.clone(),
            shared.config.execute_deadlines,
        )
    };
    for stream in listener.incoming() {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_nodelay(true);
        let Some(permit) = connections.acquire() else {
            // Over the listener's fixed limit: answered at once, nothing is read.
            let busy = Refusal::new(Code::Busy, Reason::AtCapacity);
            let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));
            let body = serde_json::to_vec(&busy).unwrap_or_default();
            let _ = http::respond_now(&mut stream, busy.status(), &body);
            continue;
        };
        let shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name(
                if control {
                    "control-connection"
                } else {
                    "execute-connection"
                }
                .into(),
            )
            .spawn(move || {
                let _permit = permit;
                // A panic is contained to its connection and answers with no detail.
                let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if control {
                        serve_control(&shared, &mut stream, deadlines);
                    } else {
                        serve_execute(&shared, &mut stream, deadlines);
                    }
                }));
                if handled.is_err() {
                    let failure = Refusal::new(Code::ExecutionFailed, Reason::InternalError);
                    http::refuse(&mut stream, &failure, deadlines.response);
                }
            });
        // A thread that cannot start drops the stream, and with it the connection.
        drop(spawned);
    }
}

fn not_found(stream: &mut TcpStream, deadlines: Deadlines) {
    let refusal = Refusal::new(Code::NotFound, Reason::UnknownOperation);
    http::refuse(stream, &refusal, deadlines.response);
}

fn unreadable(stream: &mut TcpStream, error: Unreadable, deadlines: Deadlines) {
    if let Some(refusal) = error.refusal() {
        http::refuse(stream, &refusal, deadlines.response);
    }
}

fn serve_execute(shared: &Arc<Shared>, stream: &mut TcpStream, deadlines: Deadlines) {
    let mut head = match http::read_head(stream, deadlines) {
        Ok(head) => head,
        Err(error) => return unreadable(stream, error, deadlines),
    };
    if head.method != "POST" || head.path != "/v1/execute" {
        return not_found(stream, deadlines);
    }
    let Some(length) = head.content_length else {
        return unreadable(stream, Unreadable::Malformed, deadlines);
    };
    if length > http::MAX_REQUEST {
        return unreadable(stream, Unreadable::BodyTooLarge, deadlines);
    }
    let Some(slot) = shared.running.acquire() else {
        return busy(stream, &mut head, length, deadlines);
    };
    let body = match http::read_body(stream, &mut head, length, deadlines) {
        Ok(body) => body,
        Err(error) => return unreadable(stream, error, deadlines),
    };
    let (status, response) = run::execute(shared, &body, slot);
    let _ = http::respond(stream, status, &response, deadlines.response);
}

/// Every execution slot is busy. The body is not waited for; only bytes that already arrived
/// are looked at, so a non-`wasm` runtime mode is still refused first.
fn busy(stream: &mut TcpStream, head: &mut Head, length: usize, deadlines: Deadlines) {
    http::drain_available(stream, head, length);
    let refusal =
        if head.buffered.len() >= length && api::refuses_runtime_mode(&head.buffered[..length]) {
            Refusal::new(Code::RuntimeModeNotSupported, Reason::RuntimeModeNotWasm)
        } else {
            Refusal::new(Code::Busy, Reason::AtCapacity)
        };
    http::refuse(stream, &refusal, deadlines.response);
}

fn serve_control(shared: &Arc<Shared>, stream: &mut TcpStream, deadlines: Deadlines) {
    let mut head = match http::read_head(stream, deadlines) {
        Ok(head) => head,
        Err(error) => return unreadable(stream, error, deadlines),
    };
    let operation = match (head.method.as_str(), head.path.as_str()) {
        ("POST", "/v1/cancel") => Control::Cancel,
        ("POST", "/v1/update") => Control::Update,
        _ => return not_found(stream, deadlines),
    };
    let length = match head.content_length {
        Some(length) if length <= http::MAX_CONTROL => length,
        Some(_) => return unreadable(stream, Unreadable::BodyTooLarge, deadlines),
        None => return unreadable(stream, Unreadable::Malformed, deadlines),
    };
    let body = match http::read_body(stream, &mut head, length, deadlines) {
        Ok(body) => body,
        Err(error) => return unreadable(stream, error, deadlines),
    };
    let invalid = Refusal::new(Code::InvalidRequest, Reason::MalformedMessage);
    match operation {
        Control::Cancel => {
            let Ok(message) = api::parse_cancel(&body) else {
                return http::refuse(stream, &invalid, deadlines.response);
            };
            if !message.is_requested() {
                return http::refuse(stream, &invalid, deadlines.response);
            }
            let Some(entry) = shared.lookup(message.activation_id(), message.attempt_id()) else {
                let unknown = Refusal::new(Code::NotFound, Reason::UnknownExecution);
                return http::refuse(stream, &unknown, deadlines.response);
            };
            entry.cancel.store(true, Ordering::Release);
            // Acknowledged once the execution ended; uncertain when it has not ended within
            // the termination budget (for example, blocked in a host call).
            let ended = entry.wait_finished(shared.config.termination_budget);
            let _ = http::respond(stream, 200, &message.answer(ended), deadlines.response);
        }
        Control::Update => {
            let Ok(message) = api::parse_update(&body) else {
                return http::refuse(stream, &invalid, deadlines.response);
            };
            let Some(entry) = shared.lookup(message.activation_id(), message.attempt_id()) else {
                let unknown = Refusal::new(Code::NotFound, Reason::UnknownExecution);
                return http::refuse(stream, &unknown, deadlines.response);
            };
            // Sequences only move forward; an older or repeated one is stale.
            let previous = entry
                .last_sequence
                .fetch_max(message.sequence(), Ordering::AcqRel);
            if message.sequence() <= previous {
                let stale = Refusal::new(Code::StaleUpdate, Reason::SequenceNotNewer);
                return http::refuse(stream, &stale, deadlines.response);
            }
            let _ = http::respond(stream, 202, &body, deadlines.response);
        }
    }
}

enum Control {
    Cancel,
    Update,
}
