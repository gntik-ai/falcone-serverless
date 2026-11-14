//! The worker execution API service: two listeners, one execution pipeline.
//!
//! The execute listener serves only `POST /v1/execute`. The control listener serves only
//! `POST /v1/cancel` and `POST /v1/update`. Each refuses the other's operations with `404`,
//! has its own fixed connection limit and its own header, body and response deadlines, and
//! shares nothing with the other that traffic could exhaust: a flood on one cannot take
//! capacity from the other. See `docs/contracts/worker-execution-api/execution-api-v1.md`.

mod bounded;
pub mod http;
mod platform;
pub mod redact;
mod run;
#[cfg(any(test, feature = "qualification"))]
pub mod testing;

pub use http::ListenerLimits;
pub use platform::{LocalPlatform, MAX_ARTIFACT_BYTES, Platform, Unavailable};

use crate::api::{
    CancelMessage, Code, Reason, Refusal, Rejection, WorkerCeilings, parse_cancel, parse_request,
    parse_update,
};
use crate::cache::Cache;
use bounded::Helpers;
use http::{Fault, Head};
use run::Reply;
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long a cancel waits for the execution to end before it answers `uncertain`. An
/// execution that observes the cancel ends within the termination budget (500 ms); a guest
/// blocked inside a host call may not, which RUN-02's host-call deadlines close.
pub const CANCEL_WAIT: Duration = Duration::from_millis(1000);

/// What a server needs besides its platform.
#[derive(Clone)]
pub struct ServerConfig {
    pub execute_bind: SocketAddr,
    pub control_bind: SocketAddr,
    /// The most executions running at once.
    pub concurrency: usize,
    pub ceilings: WorkerCeilings,
    pub execute: ListenerLimits,
    pub control: ListenerLimits,
    /// The process-owned compile cache directory.
    pub cache_dir: PathBuf,
    /// The trusted `artifact-compile-child` executable.
    pub compiler: PathBuf,
}

/// A running execution, as the control listener sees it.
pub(crate) struct Active {
    pub cancel: Arc<AtomicBool>,
    pub update_sequence: AtomicU64,
    /// Set when the execution ended with a mutating host call that may still complete.
    pub uncertain: AtomicBool,
    finished: Mutex<bool>,
    done: Condvar,
}

impl Active {
    fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            update_sequence: AtomicU64::new(0),
            uncertain: AtomicBool::new(false),
            finished: Mutex::new(false),
            done: Condvar::new(),
        }
    }

    fn finish(&self) {
        *self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.done.notify_all();
    }

    /// Whether the execution has ended, waiting at most `within` for it to.
    fn wait_finished(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let mut finished = self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*finished {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            finished = self
                .done
                .wait_timeout(finished, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        true
    }
}

pub(crate) struct Services {
    pub platform: Arc<dyn Platform>,
    pub cache: Mutex<Cache>,
    pub helpers: Helpers,
    pub config: ServerConfig,
    /// The running executions, by activation and attempt. Its length is the number of
    /// execution slots in use; the slot count is `config.concurrency`.
    active: Mutex<HashMap<(String, String), Arc<Active>>>,
    /// Where connections wait on their peers; tests synchronise on it.
    #[cfg(any(test, feature = "qualification"))]
    observed: Arc<testing::Observed>,
}

/// An execution slot. Dropping it frees the slot and tells a waiting cancel.
struct Slot<'a> {
    services: &'a Services,
    key: (String, String),
    active: Arc<Active>,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.services
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
        self.active.finish();
    }
}

impl Services {
    fn running(&self) -> usize {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Takes a slot for the attempt, or None when every slot is busy or the same attempt is
    /// already running.
    fn admit(&self, key: (String, String)) -> Option<Slot<'_>> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active.len() >= self.config.concurrency || active.contains_key(&key) {
            return None;
        }
        let entry = Arc::new(Active::new());
        active.insert(key.clone(), entry.clone());
        Some(Slot {
            services: self,
            key,
            active: entry,
        })
    }

    #[cfg(any(test, feature = "qualification"))]
    fn waiting(&self, role: Role) -> &Arc<testing::Waiting> {
        match role {
            Role::Execute => &self.observed.execute,
            Role::Control => &self.observed.control,
        }
    }

    fn find(&self, activation: &str, attempt: &str) -> Option<Arc<Active>> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(activation.to_owned(), attempt.to_owned()))
            .cloned()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Execute,
    Control,
}

/// A running server. Dropping it stops accepting; connections in flight finish on their own
/// deadlines.
pub struct Server {
    execute: SocketAddr,
    control: SocketAddr,
    stop: Arc<AtomicBool>,
    #[cfg(any(test, feature = "qualification"))]
    observed: Arc<testing::Observed>,
}

impl Server {
    /// Binds both listeners and starts serving.
    pub fn start(config: ServerConfig, platform: Arc<dyn Platform>) -> Result<Self, String> {
        let cache = Cache::new(&config.cache_dir, &config.compiler)
            .map_err(|error| error.code.to_owned())?;
        let execute = TcpListener::bind(config.execute_bind)
            .map_err(|error| format!("execute listener: {}", error.kind()))?;
        let control = TcpListener::bind(config.control_bind)
            .map_err(|error| format!("control listener: {}", error.kind()))?;
        let addresses = (
            execute.local_addr().map_err(|error| error.to_string())?,
            control.local_addr().map_err(|error| error.to_string())?,
        );
        let services = Arc::new(Services {
            platform,
            cache: Mutex::new(cache),
            // Abandoned helpers (validation, binding) are bounded independently of the
            // execution slots.
            helpers: Helpers::new((2 * config.concurrency).max(16)),
            active: Mutex::new(HashMap::new()),
            #[cfg(any(test, feature = "qualification"))]
            observed: Arc::default(),
            config,
        });
        #[cfg(any(test, feature = "qualification"))]
        let observed = services.observed.clone();
        let stop = Arc::new(AtomicBool::new(false));
        for (listener, role, limits) in [
            (execute, Role::Execute, services.config.execute),
            (control, Role::Control, services.config.control),
        ] {
            let services = services.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name(match role {
                    Role::Execute => "execute-listener".into(),
                    Role::Control => "control-listener".into(),
                })
                .spawn(move || accept_loop(listener, role, limits, services, stop))
                .map_err(|error| error.to_string())?;
        }
        Ok(Self {
            execute: addresses.0,
            control: addresses.1,
            stop,
            #[cfg(any(test, feature = "qualification"))]
            observed,
        })
    }

    pub fn execute_addr(&self) -> SocketAddr {
        self.execute
    }

    pub fn control_addr(&self) -> SocketAddr {
        self.control
    }

    /// The connections waiting on their peers, by listener.
    #[cfg(any(test, feature = "qualification"))]
    pub fn observed(&self) -> &testing::Observed {
        &self.observed
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake each blocked accept so its loop sees the flag.
        for address in [self.execute, self.control] {
            let _ = TcpStream::connect_timeout(&address, Duration::from_millis(200));
        }
    }
}

struct Connection(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for Connection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn accept_loop(
    listener: TcpListener,
    role: Role,
    limits: ListenerLimits,
    services: Arc<Services>,
    stop: Arc<AtomicBool>,
) {
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut stream) = stream else { continue };
        if live.fetch_add(1, Ordering::AcqRel) >= limits.max_connections {
            live.fetch_sub(1, Ordering::AcqRel);
            // Over the listener's own connection limit: answer at once, read nothing.
            let busy = Reply::bare(&Refusal::new(Code::Busy, Reason::NoCapacity));
            http::respond(
                &mut stream,
                busy.status,
                &busy.body,
                Duration::from_millis(100),
            );
            let _ = stream.shutdown(std::net::Shutdown::Write);
            continue;
        }
        let connection = Connection(live.clone());
        let services = services.clone();
        let _ = std::thread::Builder::new()
            .name("execution-api-connection".into())
            .spawn(move || {
                let _connection = connection;
                serve_connection(&services, role, limits, stream);
            });
    }
}

fn plain(status: u16, error: &str) -> Reply {
    Reply {
        status,
        body: format!("{{\"error\":\"{error}\"}}").into_bytes(),
    }
}

fn malformed() -> Reply {
    Reply::bare(&Refusal::new(
        Code::InvalidRequest,
        Reason::MalformedRequest,
    ))
}

fn serve_connection(
    services: &Services,
    role: Role,
    limits: ListenerLimits,
    mut stream: TcpStream,
) {
    let _ = stream.set_nodelay(true);
    let header_deadline = Instant::now() + limits.header_deadline;
    #[cfg(any(test, feature = "qualification"))]
    let in_headers = testing::Enter::headers(services.waiting(role));
    let read = http::read_head(&mut stream, limits.max_header_bytes, header_deadline);
    #[cfg(any(test, feature = "qualification"))]
    drop(in_headers);
    let mut head = match read {
        Ok(head) => head,
        Err(Fault::Closed) => return,
        Err(Fault::HeadersTooLarge) => {
            let reply = plain(431, "headers_too_large");
            http::respond(
                &mut stream,
                reply.status,
                &reply.body,
                limits.response_deadline,
            );
            return http::finish(&mut stream);
        }
        Err(Fault::Malformed) => {
            let reply = plain(400, "malformed_request");
            http::respond(
                &mut stream,
                reply.status,
                &reply.body,
                limits.response_deadline,
            );
            return http::finish(&mut stream);
        }
    };
    let reply = match (role, head.method.as_str(), head.path.as_str()) {
        (Role::Execute, "POST", "/v1/execute") => execute(services, &mut stream, &mut head, limits),
        (Role::Control, "POST", "/v1/cancel") => {
            control(services, &mut stream, &mut head, limits, cancel)
        }
        (Role::Control, "POST", "/v1/update") => {
            control(services, &mut stream, &mut head, limits, update)
        }
        _ => Some(plain(404, "not_found")),
    };
    // None: the body never arrived within its deadline. Close without a reply.
    let Some(reply) = reply else { return };
    if http::respond(
        &mut stream,
        reply.status,
        &reply.body,
        limits.response_deadline,
    ) {
        http::finish(&mut stream);
    }
}

/// Reads the body the head announces. `Err` carries the reply for a head that cannot be
/// served; `Ok(None)` means the body stalled or ended early.
fn read_announced(
    services: &Services,
    role: Role,
    stream: &mut TcpStream,
    head: &mut Head,
    limits: ListenerLimits,
) -> Result<Option<Vec<u8>>, Reply> {
    let Some(length) = head.content_length else {
        return Err(plain(411, "length_required"));
    };
    if length > limits.max_body_bytes {
        return Err(Reply::bare(&Refusal::new(
            Code::RequestTooLarge,
            Reason::BodyTooLarge,
        )));
    }
    if head.expects_continue && !http::respond_continue(stream, limits.response_deadline) {
        return Ok(None);
    }
    let deadline = Instant::now() + limits.body_deadline;
    #[cfg(any(test, feature = "qualification"))]
    let _in_body = testing::Enter::body(services.waiting(role));
    // Only the observation hooks use them.
    #[cfg(not(any(test, feature = "qualification")))]
    let _ = (services, role);
    Ok(http::read_body(stream, head, length, deadline).ok())
}

fn execute(
    services: &Services,
    stream: &mut TcpStream,
    head: &mut Head,
    limits: ListenerLimits,
) -> Option<Reply> {
    let Some(length) = head.content_length else {
        return Some(plain(411, "length_required"));
    };
    if length > limits.max_body_bytes {
        return Some(Reply::bare(&Refusal::new(
            Code::RequestTooLarge,
            Reason::BodyTooLarge,
        )));
    }
    // With every slot busy the body is not read: not waited for, not collected. The one
    // thing still looked at is a body that already arrived with the headers (it is in the
    // head's buffer, nothing is read from the socket for it), because a refusal that does
    // not depend on capacity (the runtime mode) takes precedence over `busy`.
    if services.running() >= services.config.concurrency {
        if head.buffered.len() >= length
            && let Err(Rejection::Refused(refused)) = parse_request(&head.buffered[..length])
            && refused.refusal.code() == Code::RuntimeModeNotSupported
        {
            return Some(refusal_reply(refused));
        }
        return Some(Reply::bare(&Refusal::new(Code::Busy, Reason::NoCapacity)));
    }
    let body = match read_announced(services, Role::Execute, stream, head, limits) {
        Err(reply) => return Some(reply),
        Ok(None) => return None,
        Ok(Some(body)) => body,
    };
    let request = match parse_request(&body) {
        Ok(request) => request,
        Err(Rejection::Malformed(_)) => return Some(malformed()),
        Err(Rejection::Refused(refused)) => return Some(refusal_reply(refused)),
    };
    drop(body);
    let Some(slot) = services.admit(request.key()) else {
        return Some(Reply::bare(&Refusal::new(Code::Busy, Reason::NoCapacity)));
    };
    // Admission is this instant: the execution's one absolute deadline counts from here.
    let admitted = Instant::now();
    let reply = run::run(services, &request, &slot.active, admitted);
    // The slot is free before the response is written: a slow reader holds a connection,
    // never an execution slot.
    drop(slot);
    Some(reply)
}

fn refusal_reply(refused: crate::api::Refused) -> Reply {
    match refused.attribution {
        Some(attribution) => {
            let result =
                crate::api::ExecutionResult::failure(&attribution, refused.refusal, Vec::new());
            Reply {
                status: result.http_status(),
                body: result.to_vec(),
            }
        }
        None => Reply::bare(&refused.refusal),
    }
}

type ControlHandler = fn(&Services, &[u8]) -> Reply;

fn control(
    services: &Services,
    stream: &mut TcpStream,
    head: &mut Head,
    limits: ListenerLimits,
    handler: ControlHandler,
) -> Option<Reply> {
    let body = match read_announced(services, Role::Control, stream, head, limits) {
        Err(reply) => return Some(reply),
        Ok(None) => return None,
        Ok(Some(body)) => body,
    };
    Some(handler(services, &body))
}

/// Cancels a running execution and reports whether it ended. `acknowledged` means the
/// execution observed the cancellation and ended within `CANCEL_WAIT`, with no mutating host
/// call left in flight; `uncertain` means it did not, or ended with such a call (a guest
/// blocked in a host call), or is not running (it may have ended on its own, or never
/// started): the outcome is not known.
fn cancel(services: &Services, body: &[u8]) -> Reply {
    let message: CancelMessage = match parse_cancel(body) {
        Ok(message) if message.is_requested() => message,
        _ => return malformed(),
    };
    let (activation, attempt) = message.key();
    let answer = match services.find(activation, attempt) {
        Some(active) => {
            active.cancel.store(true, Ordering::Release);
            // An execution that ended with a mutating host call still in flight is not a
            // cancellation that is known to have taken effect.
            if active.wait_finished(CANCEL_WAIT) && !active.uncertain.load(Ordering::Acquire) {
                message.acknowledged()
            } else {
                message.uncertain()
            }
        }
        None => message.uncertain(),
    };
    Reply {
        status: 200,
        body: answer.to_vec(),
    }
}

/// Records an execution update against the running execution: the highest sequence wins.
/// `204` when the execution is running, `404` when it is not.
fn update(services: &Services, body: &[u8]) -> Reply {
    let Ok(message) = parse_update(body) else {
        return malformed();
    };
    let (activation, attempt) = message.key();
    match services.find(activation, attempt) {
        Some(active) => {
            active
                .update_sequence
                .fetch_max(message.sequence(), Ordering::AcqRel);
            Reply {
                status: 204,
                body: Vec::new(),
            }
        }
        None => plain(404, "not_running"),
    }
}
