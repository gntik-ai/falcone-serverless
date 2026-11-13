//! One execution, from the request body to the response: admission checks, the trusted artifact
//! store, declared-import enforcement, trusted compilation, the host with its budgets, and the
//! result envelope with the activation runtime identity.
//!
//! The execution credential lives only in this layer. It is never placed in the host context,
//! never reaches a guest, and is redacted from every log line before the line leaves here.

use super::redact::Redactor;
use super::{Ended, Shared, acquire, bare, log};
use crate::api::{
    self, Attribution, Code, ExecuteRequest, ExecutionResult, HandlerResult, Reason, Refusal,
    Refused, Rejection,
};
use falcone_serverless_worker::artifact::cache_key::ENGINE_VERSION;
use falcone_serverless_worker::artifact::supervisor::CompileLimits;
use falcone_serverless_worker::artifact::validate::validate;
use falcone_serverless_worker::cache::{CONTRACT_VERSION, Loaded};
use falcone_serverless_worker::host::outcome::InvocationOutcome;
use falcone_serverless_worker::host::{
    self, Bindings, Context, Grants, HostError, Operation, Reply,
};
use falcone_serverless_worker::limits::{Field, Limits};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The largest raw artifact the worker reads from its store.
const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
/// Guest log lines kept per execution.
const MAX_LOG_LINES: usize = 1000;
/// Redacted guest log lines copied to the structured worker log per execution.
const EMITTED_LOG_LINES: usize = 100;
/// Longest guest log line copied to the structured worker log.
const EMITTED_LOG_CHARS: usize = 1024;

/// Handles one execute request body: the HTTP status and the response body.
pub(super) fn handle(shared: &Arc<Shared>, body: &[u8]) -> (u16, Vec<u8>) {
    let request = match api::parse_request(body) {
        Ok(request) => request,
        Err(Rejection::Refused(refused)) => return refuse(refused),
        Err(Rejection::Malformed(_)) => {
            return bare(Code::InvalidRequest, Reason::MalformedMessage);
        }
    };
    let at = request.attribution();
    let Some(_running) = acquire(&shared.running, shared.config.concurrency.max(1)) else {
        return bare(Code::Busy, Reason::AtCapacity);
    };
    let key = {
        let (activation, attempt) = request.key();
        (activation.to_owned(), attempt.to_owned())
    };
    let Some(registration) = shared.register(key) else {
        return bare(Code::InvalidRequest, Reason::DuplicateAttempt);
    };
    log::event(
        "execution_admitted",
        &[
            ("activation_id", json!(request.key().0)),
            ("attempt_id", json!(request.key().1)),
        ],
    );
    let started = Instant::now();
    let (result, ended, host_calls) =
        execute(shared, &request, &at, &registration.entry.cancel, started);
    registration.entry.end(ended);
    log::event(
        "execution_finished",
        &[
            ("activation_id", json!(request.key().0)),
            ("attempt_id", json!(request.key().1)),
            ("http_status", json!(result.http_status())),
            ("host_calls", json!(host_calls)),
            ("elapsed_ms", json!(started.elapsed().as_millis() as u64)),
        ],
    );
    (result.http_status(), result.to_json())
}

/// A refusal of the request as parsed: an execution result when the activation is known, the
/// bare failure envelope when it is not.
fn refuse(refused: Refused) -> (u16, Vec<u8>) {
    match refused.attribution {
        Some(at) => {
            let result = ExecutionResult::failure(&at, refused.refusal, Vec::new());
            (result.http_status(), result.to_json())
        }
        None => (refused.refusal.http_status(), refused.refusal.to_json()),
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// The guest's host bindings for one execution: log operations are captured as result logs,
/// everything else goes to the platform bindings. The credential claims are held by the caller
/// and are not reachable from here.
struct ExecutionBindings {
    inner: Arc<dyn Bindings>,
    logs: Mutex<Vec<String>>,
    calls: AtomicU64,
}

impl Bindings for ExecutionBindings {
    fn call(&self, context: &Context, operation: Operation) -> Result<Reply, HostError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        let Operation::Log {
            severity,
            message,
            fields,
        } = operation
        else {
            return self.inner.call(context, operation);
        };
        let level = format!("{severity:?}").to_lowercase();
        let mut line = format!("[{level}] {message}");
        for field in fields {
            line.push_str(&format!(" {}={}", field.name, field.value));
        }
        let mut logs = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        if logs.len() < MAX_LOG_LINES {
            // Bound memory now; the redactor clips again.
            logs.push(clip(line));
        }
        Ok(Reply::Empty)
    }

    fn resolve(&self, host: &str) -> Result<Vec<std::net::IpAddr>, HostError> {
        self.inner.resolve(host)
    }

    fn http(
        &self,
        context: &Context,
        request: &host::net::PinnedRequest,
    ) -> Result<host::net::Response, HostError> {
        self.inner.http(context, request)
    }
}

fn clip(line: String) -> String {
    match line.char_indices().nth(super::redact::MAX_LINE_CHARS) {
        Some((end, _)) => line[..end].to_owned(),
        None => line,
    }
}

fn refusal(code: Code, reason: Reason) -> Refusal {
    Refusal::new(code, reason)
}

fn timeout() -> Refusal {
    refusal(Code::ExecutionTimeout, Reason::DeadlineExceeded)
}

fn read_artifact(shared: &Shared, request: &ExecuteRequest) -> Result<Vec<u8>, Refusal> {
    let unavailable = || refusal(Code::ExecutionFailed, Reason::ArtifactUnavailable);
    // The name is 64 lowercase hex digits by construction of the digest type.
    let path = shared
        .config
        .artifact_dir
        .join(format!("{}.wasm", request.artifact_hex()));
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_ARTIFACT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| unavailable())?;
    // The identity reports the pinned digest: the bytes must be exactly that artifact.
    if bytes.len() > MAX_ARTIFACT_BYTES || Sha256::digest(&bytes)[..] != request.artifact_digest() {
        return Err(unavailable());
    }
    Ok(bytes)
}

/// Declared-import enforcement, before any compilation or instantiation: every import of the
/// component must be allowed by the worker and declared by the revision.
fn check_imports(request: &ExecuteRequest, bytes: &[u8]) -> Result<(), Refusal> {
    match validate(bytes, MAX_ARTIFACT_BYTES) {
        Ok(validated) => request.check_imports(&validated.imports),
        Err(error) if error.code == "undeclared_import" => {
            Err(refusal(Code::UndeclaredImport, Reason::ImportNotAllowed))
        }
        Err(_) => Err(refusal(Code::ExecutionFailed, Reason::ArtifactRejected)),
    }
}

enum Compiled {
    Ready(Loaded),
    Cancelled,
    Timeout,
    Rejected,
}

/// Trusted compilation under the cache lock. Waiting for the lock honours cancellation and the
/// execution's time limit.
fn compile(shared: &Shared, bytes: &[u8], cancel: &Arc<AtomicBool>, until: Instant) -> Compiled {
    let mut cache = loop {
        match shared.cache.try_lock() {
            Ok(cache) => break cache,
            Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if cancel.load(Ordering::Acquire) {
                    return Compiled::Cancelled;
                }
                if Instant::now() >= until {
                    return Compiled::Timeout;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    };
    let limits = CompileLimits {
        input_bytes: MAX_ARTIFACT_BYTES,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: until,
    };
    match cache.compile(CONTRACT_VERSION, bytes, limits, cancel) {
        Ok(loaded) => Compiled::Ready(loaded),
        Err(error) => match error.code {
            "compile_cancelled" => Compiled::Cancelled,
            "compile_deadline_exceeded" => Compiled::Timeout,
            _ => Compiled::Rejected,
        },
    }
}

type Done = (ExecutionResult, Ended, u64);

fn execute(
    shared: &Shared,
    request: &ExecuteRequest,
    at: &Attribution,
    cancel: &Arc<AtomicBool>,
    started: Instant,
) -> Done {
    let failed = |refusal: Refusal| -> Done {
        (
            ExecutionResult::failure(at, refusal, Vec::new()),
            Ended::Stopped,
            0,
        )
    };
    if let Err(refusal) = request.check_engine(ENGINE_VERSION) {
        return failed(refusal);
    }
    let limits = match request.execution_limits(shared.config.ceilings, now_unix_ms()) {
        Ok(limits) => limits,
        Err(refusal) => return failed(refusal),
    };
    let timeout_ms = limits.value(Field::GuestDeadlineMs);
    if timeout_ms == 0 {
        return failed(timeout());
    }
    let until = started + Duration::from_millis(timeout_ms);
    let bytes = match read_artifact(shared, request) {
        Ok(bytes) => bytes,
        Err(refusal) => return failed(refusal),
    };
    if let Err(refusal) = check_imports(request, &bytes) {
        return failed(refusal);
    }
    let loaded = match compile(shared, &bytes, cancel, until) {
        Compiled::Ready(loaded) => loaded,
        Compiled::Cancelled => {
            return failed(refusal(Code::ExecutionFailed, Reason::Cancelled));
        }
        Compiled::Timeout => return failed(timeout()),
        Compiled::Rejected => {
            return failed(refusal(Code::ExecutionFailed, Reason::ArtifactRejected));
        }
    };
    // Compilation counts against the execution's time: the guest gets what is left.
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return failed(timeout());
    }
    let left_ms = (left.as_millis() as u64).max(1);
    let limits = limits.with(Field::GuestDeadlineMs, left_ms).with(
        Field::HostCallDeadlineMs,
        left_ms.min(Field::HostCallDeadlineMs.ceiling()),
    );
    run_guest(shared, request, at, cancel, &loaded, limits)
}

fn run_guest(
    shared: &Shared,
    request: &ExecuteRequest,
    at: &Attribution,
    cancel: &Arc<AtomicBool>,
    loaded: &Loaded,
    limits: Limits,
) -> Done {
    let scopes = request.scopes();
    let context = Context {
        grants: Grants {
            prefixes: scopes.prefixes,
            hosts: scopes.hosts,
            topics: scopes.topics,
            targets: scopes.targets,
        },
        tenant: request.tenant().to_owned(),
        workspace: request.workspace().to_owned(),
        invocation_key: request.invocation_key().to_owned(),
        attempt_id: request.attempt().to_owned(),
        idempotency_key: request.idempotency_key().to_owned(),
    };
    let bindings = Arc::new(ExecutionBindings {
        inner: shared.bindings.clone(),
        logs: Mutex::new(Vec::new()),
        calls: AtomicU64::new(0),
    });
    let host = host::Host::with_limits(context, bindings.clone(), limits)
        .with_cancellation(cancel.clone());
    let execution = match host::Prepared::new(loaded, host) {
        Ok(prepared) => {
            log::event(
                "guest_started",
                &[
                    ("activation_id", json!(request.key().0)),
                    ("attempt_id", json!(request.key().1)),
                ],
            );
            prepared.call(&request.params_json())
        }
        Err(execution) => execution,
    };
    let calls = bindings.calls.load(Ordering::Acquire);
    let mut lines = std::mem::take(&mut *bindings.logs.lock().unwrap_or_else(|e| e.into_inner()));
    // The handler's own error text is a log line, redacted like every other.
    if let Err(error) = &execution.result
        && let Some(message) = &error.message
    {
        lines.push(clip(format!("handler error: {message}")));
    }
    let logs = Redactor::new(request.credential_strings()).lines(lines);
    emit_guest_logs(request, &logs);
    let ended = if execution.outcome == InvocationOutcome::Uncertain {
        Ended::Uncertain
    } else {
        Ended::Stopped
    };
    let result = match (&execution.outcome, &execution.result) {
        (InvocationOutcome::Success, Ok(output)) => match HandlerResult::from_output(output) {
            Some(handler) => ExecutionResult::success(at, handler, logs),
            None => ExecutionResult::failure(
                at,
                refusal(Code::ExecutionFailed, Reason::InvalidHandlerOutput),
                logs,
            ),
        },
        (_, Err(error)) => ExecutionResult::failure(at, failure_of(error.code), logs),
        (_, Ok(_)) => ExecutionResult::failure(
            at,
            refusal(Code::ExecutionFailed, Reason::HandlerFailed),
            logs,
        ),
    };
    (result, ended, calls)
}

/// The refusal for what the host reports. The mapping is closed: an unknown code is a failed
/// instantiation, never a success.
fn failure_of(code: &str) -> Refusal {
    use Reason::*;
    let (code, reason) = match code {
        "deadline_exceeded" => (Code::ExecutionTimeout, DeadlineExceeded),
        "uncertain" => (Code::ExecutionUncertain, HostCallOutcomeUnknown),
        "cancelled" => (Code::ExecutionFailed, Cancelled),
        "guest_memory_limit_exceeded" => (Code::ExecutionFailed, MemoryBudgetExceeded),
        "fuel_exhausted" => (Code::ExecutionFailed, FuelExhausted),
        "host_call_budget_exceeded" => (Code::ExecutionFailed, HostCallBudgetExceeded),
        "buffer_limit_exceeded" => (Code::ExecutionFailed, BufferLimitExceeded),
        "invalid_input" | "handler_failed" => (Code::ExecutionFailed, HandlerFailed),
        "handler_trapped" => (Code::ExecutionFailed, HandlerTrapped),
        "missing_handler_export" | "invalid_handler_signature" => {
            (Code::ExecutionFailed, ArtifactRejected)
        }
        _ => (Code::ExecutionFailed, InstantiationFailed),
    };
    Refusal::new(code, reason)
}

/// Copies the already redacted guest log lines to the structured worker log, bounded.
fn emit_guest_logs(request: &ExecuteRequest, redacted: &[String]) {
    for line in redacted.iter().take(EMITTED_LOG_LINES) {
        let line: String = line.chars().take(EMITTED_LOG_CHARS).collect();
        log::event(
            "guest_log",
            &[
                ("activation_id", json!(request.key().0)),
                ("attempt_id", json!(request.key().1)),
                ("line", json!(line)),
            ],
        );
    }
}
