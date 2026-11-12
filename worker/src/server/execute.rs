//! The execute, cancel and update operations: what the HTTP layer does with a complete message.
//!
//! An execute request is answered, in this order, by: parsing and the contract's admission
//! (runtime mode first, then grants, credential binding and the legacy inputs); the pinned
//! engine; the deadline; the artifact and its digest; the declared-import check; and only then
//! compilation and instantiation. Nothing before the import check compiles or instantiates a
//! guest. No failure text carries a request value or a credential string.

use super::Shared;
use super::artifacts::MAX_ARTIFACT_BYTES;
use super::binding::Binding;
use super::registry::Registered;
use crate::api::{
    Attribution, Code, ExecuteRequest, ExecutionResult, Family, HandlerResult, Reason, Refusal,
    Rejection, parse_cancel, parse_request, parse_update,
};
use crate::artifact::cache_key::ENGINE_VERSION;
use crate::artifact::supervisor::CompileLimits;
use crate::artifact::validate::validate;
use crate::cache::{CONTRACT_VERSION, Cache, Loaded};
use crate::host::{self, Context, Grants, Host};
use crate::limits::{CALL_CAP_MS, Field, Limits};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A complete reply: the HTTP status and the JSON body (empty for a status-only reply).
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn status(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
        }
    }

    fn json(status: u16, value: &impl serde::Serialize) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => Self { status, body },
            Err(_) => Self::status(500),
        }
    }

    /// The 413 of a request over a size ceiling: the contract's `request_too_large` envelope.
    pub fn too_large() -> Self {
        let refusal = Refusal::new(Code::RequestTooLarge, Reason::BodyTooLarge);
        Self::json(refusal.http_status(), &refusal)
    }

    /// The 503 of a request over capacity, before it is read: the bare `busy` envelope, because
    /// no activation has been read to attribute a result to.
    pub fn busy() -> Self {
        let refusal = Refusal::new(Code::Busy, Reason::Busy);
        Self::json(refusal.http_status(), &refusal)
    }
}

/// A failure reply: a full result with the identity of the projection when the activation is
/// known, otherwise the bare failure envelope.
fn failure_of(attribution: Option<&Attribution>, refusal: Refusal, logs: Vec<String>) -> Reply {
    match attribution {
        Some(attribution) => {
            let result = ExecutionResult::failure(attribution, refusal, logs);
            Reply::json(result.http_status(), &result)
        }
        None => Reply::json(refusal.http_status(), &refusal),
    }
}

fn failure(
    attribution: Option<&Attribution>,
    code: Code,
    reason: Reason,
    logs: Vec<String>,
) -> Reply {
    failure_of(attribution, Refusal::new(code, reason), logs)
}

fn execution_failed(attribution: &Attribution, reason: Reason, logs: Vec<String>) -> Reply {
    failure(Some(attribution), Code::ExecutionFailed, reason, logs)
}

pub fn execute(shared: &Shared, body: &[u8]) -> Reply {
    match parse_request(body) {
        Ok(request) => run(shared, &request),
        Err(Rejection::Malformed(_)) => Reply::status(400),
        Err(Rejection::Refused(refused)) => {
            failure_of(refused.attribution.as_ref(), refused.refusal, Vec::new())
        }
    }
}

/// The answer to an execute request over capacity. The contract's admission comes first: a
/// `runtime_mode` other than `wasm` is refused `runtime_mode_not_supported` (non-retryable) even
/// when every execute slot is busy. Everything else is `busy`; no guest, engine or cache is
/// touched, and a request that does not parse is answered `busy` too.
pub fn over_capacity(body: &[u8]) -> Reply {
    let mode = Refusal::new(Code::RuntimeModeNotSupported, Reason::RuntimeModeNotWasm);
    match parse_request(body) {
        Err(Rejection::Refused(refused)) if refused.refusal == mode => {
            failure_of(refused.attribution.as_ref(), refused.refusal, Vec::new())
        }
        _ => Reply::busy(),
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// The context the host hands to capability calls: concrete scopes from the projection's grants
/// and the identity of the invocation. Nothing in it comes from the guest, and none of it is
/// the credential.
fn context(request: &ExecuteRequest) -> Context {
    let mut grants = Grants::default();
    for grant in request.projection().grants() {
        let names = grant.names().iter().cloned();
        match grant.family() {
            Family::Storage => grants.prefixes.extend(names),
            Family::Http => grants.hosts.extend(names),
            Family::Event => grants.topics.extend(names),
            Family::Workflow => grants.targets.extend(names),
        }
    }
    let invocation = request.invocation();
    Context {
        grants,
        tenant: request.credential().tenant().to_owned(),
        workspace: request.credential().workspace().to_owned(),
        invocation_key: invocation.invocation_key().to_owned(),
        attempt_id: request.attempt_id().as_str().to_owned(),
        idempotency_key: invocation.idempotency_key().unwrap_or_default().to_owned(),
    }
}

/// The guest budgets: the compatibility ceilings, with the guest deadline bounded by the
/// revision's wall-clock budget and by the time left until the request's deadline. Memory,
/// fuel and host-call limits from the projection are enforced by RUN-02.
fn limits(guest_ms: u64) -> Limits {
    let mut json = serde_json::json!({"version": 1});
    for field in Field::ALL {
        json[field.name()] = Limits::compatibility().value(field).into();
    }
    json[Field::GuestDeadlineMs.name()] = guest_ms.into();
    Limits::parse(&json.to_string()).unwrap_or(Limits::compatibility())
}

/// The reason an artifact the worker cannot run is refused.
fn artifact_reply(attribution: &Attribution, code: &str) -> Reply {
    match code {
        "undeclared_import" => failure(
            Some(attribution),
            Code::UndeclaredImport,
            Reason::UndeclaredImport,
            Vec::new(),
        ),
        "compile_cancelled" => execution_failed(attribution, Reason::Cancelled, Vec::new()),
        "compile_deadline_exceeded" => {
            execution_failed(attribution, Reason::DeadlineExceeded, Vec::new())
        }
        "compile_size_limit_exceeded"
        | "precompiled_rejected"
        | "invalid_module"
        | "unsupported_string_encoding"
        | "missing_handler_export"
        | "invalid_handler_signature"
        | "unsupported_feature" => {
            execution_failed(attribution, Reason::ArtifactRejected, Vec::new())
        }
        _ => execution_failed(attribution, Reason::WorkerError, Vec::new()),
    }
}

/// Compiles through the trusted compile child, waiting for the cache without ignoring a cancel
/// or the deadline.
fn compile(
    shared: &Shared,
    registered: &Registered,
    bytes: &[u8],
    cancelled: &std::sync::atomic::AtomicBool,
    until: Instant,
) -> Result<Loaded, &'static str> {
    let mut cache = loop {
        match shared.cache.try_lock() {
            Ok(cache) => break cache,
            Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if registered.entry.cancel_requested() {
                    return Err("compile_cancelled");
                }
                if Instant::now() >= until {
                    return Err("compile_deadline_exceeded");
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    };
    let cache: &mut Cache = &mut cache;
    let limits = CompileLimits {
        input_bytes: MAX_ARTIFACT_BYTES,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    };
    cache
        .compile(CONTRACT_VERSION, bytes, limits, cancelled)
        .map_err(|error| error.code)
}

fn run(shared: &Shared, request: &ExecuteRequest) -> Reply {
    let attribution = request.attribution();
    let projection = request.projection();
    let Some(registered) = shared.registry.register(
        request.activation_id().as_str(),
        request.attempt_id().as_str(),
    ) else {
        // The same attempt is already running here.
        return failure(Some(&attribution), Code::Busy, Reason::Busy, Vec::new());
    };
    let cancelled = || registered.entry.cancel_requested();
    if projection.engine_version() != ENGINE_VERSION {
        return execution_failed(&attribution, Reason::EngineMismatch, Vec::new());
    }
    let remaining = request
        .deadline()
        .unix_millis()
        .map_or(0, |deadline| deadline.saturating_sub(now_millis()));
    if remaining == 0 {
        return execution_failed(&attribution, Reason::DeadlineExceeded, Vec::new());
    }
    if cancelled() {
        return execution_failed(&attribution, Reason::Cancelled, Vec::new());
    }
    let digest = projection.artifact_digest().as_str();
    let Some(bytes) = shared.artifacts.component(digest) else {
        return execution_failed(&attribution, Reason::ArtifactUnavailable, Vec::new());
    };
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return execution_failed(&attribution, Reason::ArtifactRejected, Vec::new());
    }
    if format!("sha256:{:x}", Sha256::digest(&bytes)) != digest {
        return execution_failed(&attribution, Reason::ArtifactDigestMismatch, Vec::new());
    }
    // The declared-import check: before any compile or instantiation. Every import of the
    // component must be declared by the projection and allow-listed; `validate` refuses an
    // import that is not allow-listed, whether or not it is declared.
    let validated = match validate(&bytes, MAX_ARTIFACT_BYTES) {
        Ok(validated) => validated,
        Err(error) => return artifact_reply(&attribution, error.code),
    };
    let declared: HashSet<&str> = projection.imports().collect();
    if validated
        .imports
        .iter()
        .any(|import| !declared.contains(import.as_str()))
    {
        return artifact_reply(&attribution, "undeclared_import");
    }
    let binding = Arc::new(Binding::new(request.credential().clone()));
    // `timeout_ms` is at least 1 by the contract, so the range is never empty.
    let guest_ms = remaining.clamp(1, projection.timeout_ms().clamp(1, CALL_CAP_MS));
    let host = Host::with_limits(context(request), binding.clone(), limits(guest_ms));
    registered
        .entry
        .attach(host.cancellation(), binding.clone());
    let until = Instant::now() + Duration::from_millis(remaining);
    let loaded = match compile(shared, &registered, &bytes, &host.cancellation(), until) {
        Ok(loaded) => loaded,
        Err(code) => return artifact_reply(&attribution, code),
    };
    if cancelled() {
        return execution_failed(&attribution, Reason::Cancelled, Vec::new());
    }
    shared.instances.fetch_add(1, Ordering::SeqCst);
    let execution = host::execute(&loaded, host, &request.params_json());
    registered
        .entry
        .finish(execution.outcome == host::outcome::InvocationOutcome::Uncertain);
    let mut lines = binding.take_lines();
    let outcome = match execution.result {
        Ok(output) => match HandlerResult::from_output(&output) {
            Some(handler) => Ok(handler),
            None => Err((Reason::InvalidHandlerResult, None)),
        },
        Err(error) => Err((error_reason(error.code), error.message)),
    };
    match outcome {
        Ok(handler) => {
            let logs = binding.redactor().redact(lines);
            let result = ExecutionResult::success(&attribution, handler, logs);
            Reply::json(result.http_status(), &result)
        }
        Err((reason, message)) => {
            // The guest's own error text is a log line like any other: redacted with the rest.
            lines.extend(message.map(|message| format!("error {message}")));
            let logs = binding.redactor().redact(lines);
            execution_failed(&attribution, reason, logs)
        }
    }
}

/// The contract reason of a failed host execution.
fn error_reason(code: &str) -> Reason {
    match code {
        "invalid_input" => Reason::InvalidInput,
        "handler_failed" => Reason::HandlerFailed,
        "handler_trapped" => Reason::HandlerTrapped,
        "deadline_exceeded" => Reason::DeadlineExceeded,
        "guest_memory_limit_exceeded" => Reason::GuestMemoryLimitExceeded,
        "buffer_limit_exceeded" => Reason::BufferLimitExceeded,
        "cancelled" => Reason::Cancelled,
        "uncertain" => Reason::Uncertain,
        "instantiation_failed" | "missing_handler_export" | "invalid_handler_signature" => {
            Reason::ArtifactRejected
        }
        _ => Reason::WorkerError,
    }
}

/// Cancel: interrupts the guest and answers `acknowledged` once it has stopped, or `uncertain`
/// when it has not stopped within the cancel deadline or a detached host call may still commit.
/// An attempt that is not running has nothing left to stop. A cancel cannot interrupt a blocked
/// host call before RUN-02 adds host-call deadlines, which is the `uncertain` case.
pub fn cancel(shared: &Shared, body: &[u8]) -> Reply {
    let Ok(message) = parse_cancel(body) else {
        return Reply::status(400);
    };
    if !message.is_requested() {
        return Reply::status(400);
    }
    let acknowledged = match shared.registry.get(
        message.activation_id().as_str(),
        message.attempt_id().as_str(),
    ) {
        None => true,
        Some(entry) => {
            entry.request_cancel();
            entry.wait(shared.cancel_deadline) == Some(false)
        }
    };
    Reply::json(200, &message.reply(acknowledged))
}

/// Update: the dispatcher polls a running attempt with a `heartbeat` update and gets the latest
/// progress or checkpoint reference the guest reported, or a heartbeat when it reported none.
/// An attempt that is not running is a 404.
pub fn update(shared: &Shared, body: &[u8]) -> Reply {
    let Ok(message) = parse_update(body) else {
        return Reply::status(400);
    };
    if !message.is_heartbeat() {
        return Reply::status(400);
    }
    let entry = shared.registry.get(
        message.activation_id().as_str(),
        message.attempt_id().as_str(),
    );
    let Some(binding) = entry.and_then(|entry| entry.binding()) else {
        return Reply::status(404);
    };
    let (latest, reports) = binding.progress();
    match message.reply(latest, reports) {
        Some(reply) => Reply::json(200, &reply),
        None => Reply::status(400),
    }
}

impl Shared {
    /// The compile count of the trusted cache, for the service's own accounting.
    pub fn compiles(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .spawn_count()
    }
}
