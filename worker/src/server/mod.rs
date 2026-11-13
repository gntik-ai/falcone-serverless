//! The worker's HTTP service for ctr-worker-execution-api v1: two pod-local listeners.
//!
//! - The **execute listener** serves `POST /v1/execute` and answers every other path with 404.
//!   At most `concurrency` executions run; at most [`ADMISSION_ALLOWANCE`] more connections are
//!   in admission (reading headers or body, or writing a response). Anything beyond that is
//!   answered at once with `503 busy`, without its body being read. A request in admission while
//!   every execution slot runs is read in full: a contract refusal keeps its precedence, and
//!   only a request that would run is answered `503 busy`.
//! - The **control listener** serves `POST /v1/cancel` and `POST /v1/update` and answers every
//!   other path, including execute, with 404. It has its own fixed connection limit
//!   ([`CONTROL_CONNECTIONS`]) and its own deadlines, and no execute traffic can consume it by
//!   construction: it is a different socket with a different counter.
//!
//! Both listeners bind every address of the pod; only the execute port is meant for the
//! Service. The control port is reached by the pod address and is protected by network policy
//! (see `docs/contracts/worker-execution-api/execution-api-v1.md`).

mod http;
mod log;
mod run;

use crate::api::{self, Code, Reason, Refusal, WorkerCeilings};
use falcone_serverless_worker::cache::Cache;
use falcone_serverless_worker::host::Bindings;
use falcone_serverless_worker::redact;
use serde_json::json;
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub use http::Deadlines;

/// Connections of the execute listener beyond the running executions: those still being read
/// and those still writing a response.
pub const ADMISSION_ALLOWANCE: usize = 8;
/// Connections of the control listener.
pub const CONTROL_CONNECTIONS: usize = 16;

pub struct ServerConfig {
    pub execute_addr: SocketAddr,
    pub control_addr: SocketAddr,
    /// The most executions that run at once.
    pub concurrency: usize,
    pub ceilings: WorkerCeilings,
    /// How long a cancel waits for the execution to stop before it answers `uncertain`.
    pub cancel_deadline: Duration,
    /// Trusted store of raw artifacts, one `<sha256 hex>.wasm` file per artifact.
    pub artifact_dir: PathBuf,
    /// The trusted compile cache.
    pub cache_dir: PathBuf,
    /// The trusted compiler executable.
    pub compiler: PathBuf,
    pub deadlines: Deadlines,
}

/// The listeners' bound addresses.
pub struct Running {
    pub execute_addr: SocketAddr,
    pub control_addr: SocketAddr,
}

/// A held unit of a counter; released on drop.
pub(crate) struct Permit(Arc<AtomicUsize>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn acquire(counter: &Arc<AtomicUsize>, limit: usize) -> Option<Permit> {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
            (held < limit).then_some(held + 1)
        })
        .ok()
        .map(|_| Permit(counter.clone()))
}

/// How an execution ended, for a waiting cancel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    /// It stopped and no external effect is in doubt.
    Stopped,
    /// An external effect may or may not have happened.
    Uncertain,
}

/// A running execution: its cancellation flag and its end.
pub(crate) struct Entry {
    pub cancel: Arc<AtomicBool>,
    ended: Mutex<Option<Ended>>,
    done: Condvar,
}

impl Entry {
    fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            ended: Mutex::new(None),
            done: Condvar::new(),
        }
    }

    pub(crate) fn end(&self, how: Ended) {
        let mut ended = self.ended.lock().unwrap_or_else(|e| e.into_inner());
        ended.get_or_insert(how);
        self.done.notify_all();
    }

    /// Waits for the end of the execution, at most `deadline`.
    fn wait(&self, deadline: Duration) -> Option<Ended> {
        let ended = self.ended.lock().unwrap_or_else(|e| e.into_inner());
        let (ended, _) = self
            .done
            .wait_timeout_while(ended, deadline, |ended| ended.is_none())
            .unwrap_or_else(|e| e.into_inner());
        *ended
    }
}

type Key = (String, String);

pub(crate) struct Shared {
    pub config: ServerConfig,
    pub bindings: Arc<dyn Bindings>,
    pub cache: Mutex<Cache>,
    pub running: Arc<AtomicUsize>,
    registry: Mutex<HashMap<Key, Arc<Entry>>>,
}

impl Shared {
    /// Registers a running execution; `None` when the attempt is already running.
    pub(crate) fn register(self: &Arc<Self>, key: Key) -> Option<Registration> {
        let entry = Arc::new(Entry::new());
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
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

    fn lookup(&self, key: &Key) -> Option<Arc<Entry>> {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }
}

/// The registry entry of a running execution. Dropping it ends the execution; one that was
/// not ended explicitly (a panic) ends as `Uncertain`.
pub(crate) struct Registration {
    shared: Arc<Shared>,
    key: Key,
    pub entry: Arc<Entry>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.shared
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
        self.entry.end(Ended::Uncertain);
    }
}

/// Binds both listeners and starts serving. Returns the bound addresses.
pub fn start(config: ServerConfig, bindings: Arc<dyn Bindings>) -> std::io::Result<Running> {
    let execute = TcpListener::bind(config.execute_addr)?;
    let control = TcpListener::bind(config.control_addr)?;
    let running = Running {
        execute_addr: execute.local_addr()?,
        control_addr: control.local_addr()?,
    };
    let cache = Cache::new(&config.cache_dir, &config.compiler)
        .map_err(|error| std::io::Error::other(error.code))?;
    let concurrency = config.concurrency.max(1);
    let shared = Arc::new(Shared {
        config,
        bindings,
        cache: Mutex::new(cache),
        running: Arc::new(AtomicUsize::new(0)),
        registry: Mutex::new(HashMap::new()),
    });
    let (for_execute, for_control) = (shared.clone(), shared);
    std::thread::Builder::new()
        .name("execute-listener".into())
        .spawn(move || {
            accept(
                "execute",
                &execute,
                concurrency + ADMISSION_ALLOWANCE,
                move |stream, permit| execute_connection(&for_execute, stream, permit),
            );
        })?;
    std::thread::Builder::new()
        .name("control-listener".into())
        .spawn(move || {
            accept(
                "control",
                &control,
                CONTROL_CONNECTIONS,
                move |stream, permit| control_connection(&for_control, stream, permit),
            );
        })?;
    log::event(
        "listening",
        &[
            ("execute", json!(running.execute_addr.to_string())),
            ("control", json!(running.control_addr.to_string())),
            ("concurrency", json!(concurrency)),
        ],
    );
    Ok(running)
}

/// Serves until the process ends.
pub fn run(config: ServerConfig, bindings: Arc<dyn Bindings>) -> std::io::Result<()> {
    start(config, bindings)?;
    loop {
        std::thread::park();
    }
}

/// Accepts connections until the process ends. Each one is reported as it is admitted
/// (`connection_admitted`), refused for want of capacity (`connection_rejected`) and, once its
/// handler has returned and released its slot, finished (`connection_finished`): the events let
/// a test (or an operator) know exactly how many connections the listener holds.
fn accept(
    name: &'static str,
    listener: &TcpListener,
    limit: usize,
    handler: impl Fn(TcpStream, Permit) + Clone + Send + 'static,
) {
    let connections = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            // Out of descriptors, say: do not spin while the condition lasts.
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        let Some(permit) = acquire(&connections, limit) else {
            log::event(
                "connection_rejected",
                &[
                    ("listener", json!(name)),
                    ("reason", json!("listener_full")),
                ],
            );
            reject_busy(stream);
            continue;
        };
        log::event("connection_admitted", &[("listener", json!(name))]);
        let handler = handler.clone();
        // A guest runs on this thread: give it room beyond the platform default.
        let spawned = std::thread::Builder::new()
            .name("connection".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                handler(stream, permit);
                log::event("connection_finished", &[("listener", json!(name))]);
            });
        if spawned.is_err() {
            log::event("connection_spawn_failed", &[]);
        }
    }
}

/// Discards of refused connections that may run at once.
static DRAINERS: AtomicUsize = AtomicUsize::new(0);

struct Drainer;

impl Drainer {
    fn acquire() -> Option<Self> {
        DRAINERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < 16).then_some(held + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for Drainer {
    fn drop(&mut self) {
        DRAINERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Answers `503 busy` at once, before anything is read, and lets a bounded number of drainers
/// discard the client's request so that it can read the answer.
fn reject_busy(mut stream: TcpStream) {
    let body = Refusal::new(Code::Busy, Reason::AtCapacity).to_json();
    if http::write_response(&mut stream, 503, &body, Duration::from_secs(1)).is_err() {
        return;
    }
    let Some(drainer) = Drainer::acquire() else {
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("drain".into())
        .spawn(move || {
            let _drainer = drainer;
            http::finish(&mut stream);
        });
    if spawned.is_err() {
        log::event("connection_spawn_failed", &[]);
    }
}

/// What is sent for a request that is refused before an activation is known.
fn bare(code: Code, reason: Reason) -> (u16, Vec<u8>) {
    let refusal = Refusal::new(code, reason);
    (refusal.http_status(), refusal.to_json())
}

/// Sends the answer, then ends the connection.
fn respond(
    listener: &'static str,
    stream: &mut TcpStream,
    deadlines: &Deadlines,
    (status, body): (u16, Vec<u8>),
) {
    match http::write_response(stream, status, &body, deadlines.response) {
        Ok(()) => http::finish(stream),
        Err(http::WriteFailure::Deadline) => log::event(
            "response_deadline_exceeded",
            &[("listener", json!(listener))],
        ),
        Err(http::WriteFailure::Io) => {}
    }
}

/// Answers a request that could not be read in full, or closes the connection.
fn failed(
    listener: &'static str,
    stream: &mut TcpStream,
    deadlines: &Deadlines,
    why: http::Failure,
) {
    let answer = match why {
        http::Failure::Malformed => bare(Code::InvalidRequest, Reason::MalformedHttp),
        http::Failure::HeadersTooLarge => bare(Code::RequestTooLarge, Reason::HeadersTooLarge),
        http::Failure::BodyTooLarge => bare(Code::RequestTooLarge, Reason::RequestBodyTooLarge),
        http::Failure::Closed(reason) => {
            log::event(
                "connection_closed",
                &[("listener", json!(listener)), ("reason", json!(reason))],
            );
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
    };
    respond(listener, stream, deadlines, answer);
}

fn execute_connection(shared: &Arc<Shared>, mut stream: TcpStream, _connection: Permit) {
    let deadlines = shared.config.deadlines;
    let mut head = match http::read_head(&mut stream, &deadlines, http::MAX_REQUEST) {
        Ok(head) => head,
        Err(why) => return failed("execute", &mut stream, &deadlines, why),
    };
    if head.path != "/v1/execute" {
        let answer = bare(Code::NotFound, Reason::UnknownPath);
        return respond("execute", &mut stream, &deadlines, answer);
    }
    if head.method != "POST" {
        let answer = bare(Code::MethodNotAllowed, Reason::MethodNotAllowed);
        return respond("execute", &mut stream, &deadlines, answer);
    }
    // A connection that holds an admission slot reads its request even when every execution
    // slot is running: the contract refusals (`runtime_mode_not_supported` first) keep their
    // precedence, and only a request that would run is answered `busy` (in `run::handle`, when
    // it asks for an execution slot). A connection beyond the admission allowance never gets
    // here: `accept` answers it `busy` without reading anything.
    let body = match http::read_body(&mut stream, &mut head, &deadlines) {
        Ok(body) => body,
        Err(why) => return failed("execute", &mut stream, &deadlines, why),
    };
    let answer = run::handle(shared, &body);
    respond("execute", &mut stream, &deadlines, answer);
}

fn control_connection(shared: &Arc<Shared>, mut stream: TcpStream, _connection: Permit) {
    let deadlines = shared.config.deadlines;
    let mut head = match http::read_head(&mut stream, &deadlines, http::MAX_CONTROL_BODY) {
        Ok(head) => head,
        Err(why) => return failed("control", &mut stream, &deadlines, why),
    };
    if !matches!(head.path.as_str(), "/v1/cancel" | "/v1/update") {
        let answer = bare(Code::NotFound, Reason::UnknownPath);
        return respond("control", &mut stream, &deadlines, answer);
    }
    if head.method != "POST" {
        let answer = bare(Code::MethodNotAllowed, Reason::MethodNotAllowed);
        return respond("control", &mut stream, &deadlines, answer);
    }
    let body = match http::read_body(&mut stream, &mut head, &deadlines) {
        Ok(body) => body,
        Err(why) => return failed("control", &mut stream, &deadlines, why),
    };
    let answer = if head.path == "/v1/cancel" {
        cancel(shared, &body)
    } else {
        update(shared, &body)
    };
    respond("control", &mut stream, &deadlines, answer);
}

fn key((activation, attempt): (&str, &str)) -> Key {
    (activation.to_owned(), attempt.to_owned())
}

/// Cancels a running execution and waits, at most the cancel deadline, for it to stop.
/// `acknowledged`: it stopped and no external effect is in doubt. `uncertain`: it did not stop
/// in time, or an effect that was in flight may or may not have happened.
fn cancel(shared: &Arc<Shared>, body: &[u8]) -> (u16, Vec<u8>) {
    let Ok(message) = api::parse_cancel(body) else {
        return bare(Code::InvalidRequest, Reason::MalformedMessage);
    };
    if !message.is_request() {
        return bare(Code::InvalidRequest, Reason::MalformedMessage);
    }
    let Some(entry) = shared.lookup(&key(message.key())) else {
        return bare(Code::NotFound, Reason::UnknownExecution);
    };
    entry.cancel.store(true, Ordering::Release);
    let uncertain = entry.wait(shared.config.cancel_deadline) != Some(Ended::Stopped);
    log::event(
        "cancel_answered",
        &[
            ("activation_id", json!(message.key().0)),
            ("attempt_id", json!(message.key().1)),
            ("uncertain", json!(uncertain)),
        ],
    );
    let answer = serde_json::to_vec(&message.answer(uncertain)).unwrap_or_default();
    (200, answer)
}

/// Accepts an execution update (heartbeat, progress or checkpoint reference) for a running
/// execution. The worker does not forward it anywhere yet.
fn update(shared: &Arc<Shared>, body: &[u8]) -> (u16, Vec<u8>) {
    let Ok(message) = api::parse_update(body) else {
        return bare(Code::InvalidRequest, Reason::MalformedMessage);
    };
    match shared.lookup(&key(message.key())) {
        Some(_) => (204, Vec::new()),
        None => bare(Code::NotFound, Reason::UnknownExecution),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permit_is_held_until_it_is_dropped() {
        let counter = Arc::new(AtomicUsize::new(0));
        let first = acquire(&counter, 2).unwrap();
        let _second = acquire(&counter, 2).unwrap();
        assert!(acquire(&counter, 2).is_none());
        drop(first);
        assert!(acquire(&counter, 2).is_some());
    }

    #[test]
    fn a_waiting_cancel_learns_how_the_execution_ended() {
        let stopped = Entry::new();
        stopped.end(Ended::Stopped);
        assert!(stopped.wait(Duration::from_secs(1)) == Some(Ended::Stopped));
        // The first end stands: a later drop of the registration cannot turn it uncertain.
        stopped.end(Ended::Uncertain);
        assert!(stopped.wait(Duration::from_secs(1)) == Some(Ended::Stopped));
        let uncertain = Entry::new();
        uncertain.end(Ended::Uncertain);
        assert!(uncertain.wait(Duration::from_secs(1)) == Some(Ended::Uncertain));
    }

    #[test]
    fn a_cancel_that_outwaits_its_deadline_learns_nothing() {
        // The deadline is the cancel deadline under test, not a synchronization of the test.
        let running = Entry::new();
        assert!(running.wait(Duration::from_millis(20)).is_none());
    }
}
