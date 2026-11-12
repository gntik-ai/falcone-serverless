//! The worker execution API over HTTP (`docs/contracts/worker-execution-api/execution-api-v1.md`).
//!
//! Two pod-local listeners with independent capacity:
//!
//! - the **execute listener** (`listen_port`) accepts only `POST /v1/execute`;
//! - the **control listener** (`control_port`) accepts only `POST /v1/cancel` and
//!   `POST /v1/update`.
//!
//! Each refuses the other's operations with 404. A connection is bound to its listener by the
//! port it arrived on, so control capacity never depends on classifying a connection after
//! accept. Connections are one request each, with hard size ceilings and deadlines (`http.rs`).
//!
//! Redaction by construction: nothing here formats a request value. Worker events are static
//! text and numbers (`log.rs`); guest log lines leave only through `redact.rs`.

mod control;
mod http;
mod log;
mod redact;
mod run;

use crate::config::RuntimeConfig;
use falcone_serverless_worker::{cache::Cache, cache::Loaded};
use http::{Fault, Stream};
use log::Value;
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Execute connections beyond `concurrency` that may be in admission (reading a request, or
/// finished and writing a response) before a new connection is answered `503 busy` at once.
pub const ADMISSION_ALLOWANCE: usize = 4;
/// The execute listener's time budgets, fixed: complete head from accept, then each body read
/// and the whole body.
pub const EXECUTE_HEADER_DEADLINE: Duration = Duration::from_secs(5);
pub const EXECUTE_BODY_IDLE: Duration = Duration::from_secs(5);
pub const EXECUTE_BODY_TOTAL: Duration = Duration::from_secs(30);
/// Guest threads need more than the default 2 MiB for the engine's host frames.
const HANDLER_STACK: usize = 8 * 1024 * 1024;

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the server needs from the validated runtime config, plus the trusted compiler path.
pub struct Settings {
    pub concurrency: usize,
    pub control_connections: usize,
    pub control_header: Duration,
    pub control_body: Duration,
    /// How long cancel waits for an interrupted execution to finish before reporting uncertain.
    pub cancel_wait: Duration,
    pub max_wall_clock_ms: u64,
    pub max_memory_bytes: u64,
    pub cache_path: std::path::PathBuf,
    pub compiler: std::path::PathBuf,
}

impl Settings {
    pub fn from_config(config: &RuntimeConfig, compiler: std::path::PathBuf) -> Self {
        Self {
            concurrency: config.concurrency as usize,
            control_connections: config.control_max_connections as usize,
            control_header: Duration::from_millis(config.control_header_deadline_ms),
            control_body: Duration::from_millis(config.control_body_deadline_ms),
            cancel_wait: Duration::from_millis(config.cancel_reserve_ms),
            max_wall_clock_ms: config.max_wall_clock_ms,
            max_memory_bytes: config.max_memory_bytes,
            cache_path: config.cache_path.clone().into(),
            compiler,
        }
    }
}

/// How a finished execution answers a waiting cancel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// The guest was interrupted or completed; the cancellation is acknowledged.
    Acknowledged,
    /// A mutating host call may still commit; the outcome is not known.
    Uncertain,
}

/// One running execution as the control listener sees it.
pub struct Running {
    cancel: Arc<std::sync::atomic::AtomicBool>,
    finished: Mutex<Option<Finish>>,
    signal: Condvar,
    last_update: AtomicU64,
}

impl Running {
    fn new(cancel: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            cancel,
            finished: Mutex::new(None),
            signal: Condvar::new(),
            last_update: AtomicU64::new(0),
        }
    }

    /// Records how the execution ended; the first answer stands.
    fn finish(&self, how: Finish) {
        locked(&self.finished).get_or_insert(how);
        self.signal.notify_all();
    }

    /// Waits until the execution finished or `limit` passed; `None` means it did not finish.
    fn wait(&self, limit: Duration) -> Option<Finish> {
        let deadline = Instant::now() + limit;
        let mut finished = locked(&self.finished);
        loop {
            if let Some(how) = *finished {
                return Some(how);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            finished = self
                .signal
                .wait_timeout(finished, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// A component the worker has validated and compiled, kept by component digest.
struct Entry {
    loaded: Arc<Loaded>,
    imports: Vec<String>,
}

/// The `Entry` cache holds at most this many components; a full cache is cleared.
const MAX_ENTRIES: usize = 64;

pub struct State {
    pub settings: Settings,
    running: AtomicUsize,
    executions: AtomicU64,
    registry: Mutex<HashMap<(String, String), Arc<Running>>>,
    compiler: Mutex<Cache>,
    entries: Mutex<HashMap<String, Arc<Entry>>>,
}

impl State {
    pub fn new(settings: Settings) -> Result<Self, String> {
        let compiled = settings.cache_path.join("compiled");
        std::fs::create_dir_all(settings.cache_path.join("artifacts"))
            .map_err(|_| "cache_path: cannot create the artifacts directory".to_owned())?;
        let cache = Cache::new(&compiled, &settings.compiler)
            .map_err(|error| format!("trusted compile cache: {}", error.code))?;
        Ok(Self {
            settings,
            running: AtomicUsize::new(0),
            executions: AtomicU64::new(0),
            registry: Mutex::new(HashMap::new()),
            compiler: Mutex::new(cache),
            entries: Mutex::new(HashMap::new()),
        })
    }

    fn running_for(&self, activation: &str, attempt: &str) -> Option<Arc<Running>> {
        locked(&self.registry)
            .get(&(activation.to_owned(), attempt.to_owned()))
            .cloned()
    }
}

/// A fixed number of simultaneous connections for one listener.
struct Gate {
    active: AtomicUsize,
    limit: usize,
}

struct Admitted(Arc<Gate>);

impl Gate {
    fn admit(self: &Arc<Self>) -> Option<Admitted> {
        let before = self.active.fetch_add(1, Ordering::AcqRel);
        if before >= self.limit {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Admitted(Arc::clone(self)))
    }
}

impl Drop for Admitted {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A body-less `503 busy`, written without reading anything from the peer.
fn busy(mut stream: TcpStream) {
    let _ = http::respond(&mut stream, 503, br#"{"error":"busy"}"#);
    let _ = stream.shutdown_write();
}

#[derive(Clone, Copy)]
struct Limits {
    header: Duration,
    idle: Duration,
    body: Duration,
    max_body: usize,
}

/// Which operations a listener serves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Execute,
    Control,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Control => "control",
        }
    }
}

fn error_body(code: &str) -> Vec<u8> {
    serde_json::json!({ "error": code })
        .to_string()
        .into_bytes()
}

/// Serves one connection of `role` to completion.
fn serve_connection(state: &State, role: Role, limits: Limits, stream: &mut TcpStream) {
    let _ = stream.set_nodelay(true);
    let accepted = Instant::now();
    let (head, early) = match http::read_head(stream, accepted + limits.header) {
        Ok(read) => read,
        Err(Fault::HeadersTooLarge) => {
            let _ = http::respond(stream, 431, &error_body("headers_too_large"));
            return http::finish(stream);
        }
        Err(Fault::Malformed(label)) => {
            let _ = http::respond(stream, 400, &error_body(label));
            return http::finish(stream);
        }
        // Silent, dripping or reset connections are closed without an answer.
        Err(_) => return,
    };
    let operation = match (role, head.path.as_str()) {
        (Role::Execute, "/v1/execute") => Operation::Execute,
        (Role::Control, "/v1/cancel") => Operation::Cancel,
        (Role::Control, "/v1/update") => Operation::Update,
        _ => return reply(stream, 404, &error_body("not_found")),
    };
    if head.method != "POST" {
        return reply(stream, 405, &error_body("method_not_allowed"));
    }
    let Some(length) = head.content_length else {
        return reply(stream, 411, &error_body("length_required"));
    };
    if length > limits.max_body {
        return reply(stream, 413, &error_body("request_too_large"));
    }
    let body = match http::read_body(stream, length, early, limits.idle, limits.body) {
        Ok(body) => body,
        Err(_) => return,
    };
    let (status, payload) = match operation {
        Operation::Execute => run::execute(state, &body),
        Operation::Cancel => control::cancel(state, &body),
        Operation::Update => control::update(state, &body),
    };
    reply(stream, status, &payload);
}

#[derive(Clone, Copy)]
enum Operation {
    Execute,
    Cancel,
    Update,
}

fn reply(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let _ = http::respond(stream, status, body);
    http::finish(stream);
}

/// Accepts connections until the process ends; each admitted connection gets its own thread.
fn accept_loop(
    state: Arc<State>,
    role: Role,
    listener: TcpListener,
    gate: Arc<Gate>,
    limits: Limits,
) {
    for connection in listener.incoming() {
        let Ok(stream) = connection else {
            // A transient accept error (for example descriptor exhaustion) must not spin.
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        let Some(admitted) = gate.admit() else {
            busy(stream);
            continue;
        };
        let state = Arc::clone(&state);
        let spawned = std::thread::Builder::new()
            .stack_size(HANDLER_STACK)
            .spawn(move || {
                let mut stream = stream;
                log::event(
                    "admitted",
                    &[
                        ("listener", Value::Text(role.name())),
                        (
                            "active",
                            Value::Number(admitted.0.active.load(Ordering::Acquire) as u64),
                        ),
                    ],
                );
                serve_connection(&state, role, limits, &mut stream);
                // Release the permit before the socket closes: a client that sees the close can
                // rely on the capacity being free again.
                drop(admitted);
                drop(stream);
            });
        // A refused thread drops the closure, and with it the connection and its permit.
        if spawned.is_err() {
            log::event("spawn_failed", &[("listener", Value::Text(role.name()))]);
        }
    }
}

/// The addresses the listeners are bound to.
pub struct Listening {
    pub execute: SocketAddr,
    pub control: SocketAddr,
}

/// Binds both listeners and serves them on background threads.
pub fn start(config: &RuntimeConfig, compiler: std::path::PathBuf) -> Result<Listening, String> {
    let state = Arc::new(State::new(Settings::from_config(config, compiler))?);
    let bind = |port: u16| {
        TcpListener::bind(("0.0.0.0", port))
            .map_err(|error| format!("bind {port}: {}", error.kind()))
    };
    let execute = bind(config.listen_port)?;
    let control = bind(config.control_port)?;
    let listening = Listening {
        execute: execute
            .local_addr()
            .map_err(|error| error.kind().to_string())?,
        control: control
            .local_addr()
            .map_err(|error| error.kind().to_string())?,
    };
    let execute_gate = Arc::new(Gate {
        active: AtomicUsize::new(0),
        limit: state.settings.concurrency + ADMISSION_ALLOWANCE,
    });
    let control_gate = Arc::new(Gate {
        active: AtomicUsize::new(0),
        limit: state.settings.control_connections,
    });
    let execute_limits = Limits {
        header: EXECUTE_HEADER_DEADLINE,
        idle: EXECUTE_BODY_IDLE,
        body: EXECUTE_BODY_TOTAL,
        max_body: http::MAX_REQUEST,
    };
    let control_limits = Limits {
        header: state.settings.control_header,
        idle: state.settings.control_body,
        body: state.settings.control_body,
        max_body: http::MAX_CONTROL_BODY,
    };
    for (name, role, listener, gate, limits) in [
        (
            "execute-accept",
            Role::Execute,
            execute,
            execute_gate,
            execute_limits,
        ),
        (
            "control-accept",
            Role::Control,
            control,
            control_gate,
            control_limits,
        ),
    ] {
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || accept_loop(state, role, listener, gate, limits))
            .map_err(|error| error.kind().to_string())?;
    }
    log::event(
        "listening",
        &[
            (
                "execute_port",
                Value::Number(u64::from(listening.execute.port())),
            ),
            (
                "control_port",
                Value::Number(u64::from(listening.control.port())),
            ),
            (
                "concurrency",
                Value::Number(state.settings.concurrency as u64),
            ),
        ],
    );
    Ok(listening)
}

/// Runs the worker service until the process is stopped.
pub fn serve(config: &RuntimeConfig) -> Result<(), String> {
    log::install_panic_hook();
    let compiler = std::env::current_exe()
        .map_err(|_| "cannot resolve the trusted compiler".to_owned())?
        .with_file_name("artifact-compile-child");
    start(config, compiler)?;
    loop {
        std::thread::park();
    }
}
