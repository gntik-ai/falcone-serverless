//! `POST /v1/execute`: admission, artifact checks, instantiation and the legacy envelope.
//!
//! Order of decisions: parse (400 for a malformed message); contract refusals, with a runtime
//! mode other than `wasm` first (409 `runtime_mode_not_supported`, before any capacity check);
//! engine pin (409); an execution slot (503 `busy`); the artifact and its declared imports (422
//! before anything is compiled or instantiated); then the guest runs.

use super::redact::redact_logs;
use super::{Entry, Finish, Running, State, error_body, locked, log};
use crate::api::{self, ExecuteRequest, ExecutionResult, Rejection};
use falcone_serverless_worker::artifact::cache_key::ENGINE_VERSION;
use falcone_serverless_worker::artifact::supervisor::CompileLimits;
use falcone_serverless_worker::artifact::validate::validate;
use falcone_serverless_worker::cache::CONTRACT_VERSION;
use falcone_serverless_worker::host::{
    self, Bindings, Context, Grants, HostError, Operation, Reply as HostReply,
};
use falcone_serverless_worker::limits::{CALL_CAP_MS, Field, GUEST_MEMORY_BYTES, Limits};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Reply = (u16, Vec<u8>);

/// Staged artifacts are bounded like the CLI's component input.
const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

fn fail(status: u16, code: &str) -> Reply {
    (status, error_body(code))
}

/// One of `concurrency` execution slots.
struct Slot<'a>(&'a AtomicUsize);

impl<'a> Slot<'a> {
    fn acquire(counter: &'a AtomicUsize, limit: usize) -> Option<Self> {
        if counter.fetch_add(1, Ordering::AcqRel) >= limit {
            counter.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(counter))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The registry entry of a running execution; dropping it, even by a panic, removes the entry
/// and releases any waiting cancel as `uncertain`.
struct Registered<'a> {
    state: &'a State,
    key: (String, String),
    running: Arc<Running>,
}

impl<'a> Registered<'a> {
    fn new(state: &'a State, request: &ExecuteRequest, running: Arc<Running>) -> Option<Self> {
        let key = (
            request.activation().to_owned(),
            request.attempt().to_owned(),
        );
        let mut registry = locked(&state.registry);
        if registry.contains_key(&key) {
            return None;
        }
        registry.insert(key.clone(), Arc::clone(&running));
        Some(Self {
            state,
            key,
            running,
        })
    }
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        locked(&self.state.registry).remove(&self.key);
        self.running.finish(Finish::Uncertain);
    }
}

/// The host binding layer of this service. Only guest logging is bound; every other capability
/// is denied until its binding exists. The execution credential is not reachable from here: it
/// is neither passed to the guest, nor kept in this struct, nor placed in the host `Context`.
struct Collector {
    lines: Mutex<Vec<String>>,
}

impl Bindings for Collector {
    fn call(&self, _: &Context, operation: Operation) -> Result<HostReply, HostError> {
        let Operation::Log {
            severity,
            message,
            fields,
        } = operation
        else {
            return Err(HostError::Denied);
        };
        let mut lines = locked(&self.lines);
        if lines.len() < api::MAX_LOG_LINES {
            let mut line = format!("{} {message}", format!("{severity:?}").to_lowercase());
            for field in &fields {
                line.push_str(&format!(" {}={}", field.name, field.value));
            }
            // Bound retained memory now; redaction truncates again before matching.
            if let Some((end, _)) = line.char_indices().nth(api::MAX_LOG_CHARS) {
                line.truncate(end);
            }
            lines.push(line);
        }
        Ok(HostReply::Empty)
    }
}

fn limits(state: &State, request: &ExecuteRequest) -> Limits {
    let compatibility = Limits::compatibility();
    let ceiling = state.settings.max_wall_clock_ms.clamp(1, CALL_CAP_MS);
    let deadline = request.projection().timeout_ms().clamp(1, ceiling);
    let memory = state
        .settings
        .max_memory_bytes
        .clamp(1, GUEST_MEMORY_BYTES as u64);
    let mut json = serde_json::json!({ "version": 1 });
    for field in Field::ALL {
        json[field.name()] = compatibility.value(field).into();
    }
    json["guest_deadline_ms"] = deadline.into();
    json["host_call_deadline_ms"] = deadline.into();
    json["guest_memory_bytes"] = memory.into();
    Limits::parse(&json.to_string()).unwrap_or(compatibility)
}

/// The staged component for a digest: `<cache_path>/artifacts/<hex>.wasm`, a pod-local file the
/// trusted platform places there. Its bytes must hash to the digest the revision pins.
fn staged(state: &State, hex: &str) -> Result<Vec<u8>, Reply> {
    let path = state
        .settings
        .cache_path
        .join("artifacts")
        .join(format!("{hex}.wasm"));
    let unavailable = || fail(424, "artifact_unavailable");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|_| unavailable())?;
    let metadata = file.metadata().map_err(|_| unavailable())?;
    if !metadata.is_file() || metadata.len() > MAX_ARTIFACT_BYTES as u64 {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if format!("{:x}", Sha256::digest(&bytes)) != hex {
        return Err(fail(424, "artifact_digest_mismatch"));
    }
    Ok(bytes)
}

/// Every import of the component must be declared by the revision's projection. (The artifact
/// gate has already required each to be in `ALLOWED_IMPORTS`.)
fn declared(imports: &[String], request: &ExecuteRequest) -> Result<(), Reply> {
    let declared: Vec<&str> = request.projection().imports().collect();
    match imports
        .iter()
        .find(|import| !declared.contains(&import.as_str()))
    {
        None => Ok(()),
        Some(import) => Err(undeclared(import)),
    }
}

fn undeclared(import: &str) -> Reply {
    (
        422,
        serde_json::json!({ "error": "undeclared_import", "import": import })
            .to_string()
            .into_bytes(),
    )
}

/// The validated and compiled component of the request's revision, from memory when known.
/// Declared imports are checked before anything is compiled, every time.
fn component(
    state: &State,
    request: &ExecuteRequest,
    cancelled: &AtomicBool,
) -> Result<Arc<Entry>, Reply> {
    let hex = request
        .projection()
        .component_digest()
        .strip_prefix("sha256:")
        .unwrap_or_default();
    let known = locked(&state.entries).get(hex).cloned();
    if let Some(entry) = known {
        declared(&entry.imports, request)?;
        return Ok(entry);
    }
    let bytes = staged(state, hex)?;
    let validated = validate(&bytes, MAX_ARTIFACT_BYTES).map_err(|error| match error {
        error if error.code == "undeclared_import" => {
            undeclared(error.import.as_deref().unwrap_or_default())
        }
        error => fail(422, error.code),
    })?;
    declared(&validated.imports, request)?;
    let limits = CompileLimits {
        input_bytes: MAX_ARTIFACT_BYTES,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    };
    let loaded = locked(&state.compiler)
        .compile(CONTRACT_VERSION, &bytes, limits, cancelled)
        .map_err(|error| match error.code {
            "compile_cancelled" => fail(409, "cancelled"),
            code => fail(500, code),
        })?;
    let entry = Arc::new(Entry {
        loaded: Arc::new(loaded),
        imports: validated.imports,
    });
    let mut entries = locked(&state.entries);
    if entries.len() >= super::MAX_ENTRIES {
        entries.clear();
    }
    entries.insert(hex.to_owned(), Arc::clone(&entry));
    Ok(entry)
}

fn refusal(refused: api::Refused) -> Reply {
    let status = refused.refusal.http_status();
    let body = match &refused.attribution {
        Some(attribution) => {
            serde_json::to_vec(&ExecutionResult::refused(attribution, refused.refusal))
        }
        None => serde_json::to_vec(&refused.refusal),
    };
    match body {
        Ok(body) => (status, body),
        Err(_) => fail(500, "internal"),
    }
}

fn failure_status(code: &str) -> u16 {
    match code {
        "invalid_input" => 400,
        "cancelled" => 409,
        "deadline_exceeded" => 504,
        "instantiation_failed"
        | "missing_handler_export"
        | "invalid_handler_signature"
        | "epoch_interruption_required"
        | "fuel_required" => 422,
        _ => 502,
    }
}

/// Handles one execute request body.
pub fn execute(state: &State, body: &[u8]) -> Reply {
    let request = match api::parse_request(body) {
        Ok(request) => request,
        Err(Rejection::Refused(refused)) => return refusal(refused),
        Err(Rejection::Malformed(error)) => {
            return (
                400,
                serde_json::json!({ "error": "malformed_request", "message": error.to_string() })
                    .to_string()
                    .into_bytes(),
            );
        }
    };
    if request.projection().engine_version() != ENGINE_VERSION {
        return fail(409, "engine_version_mismatch");
    }
    let Some(_slot) = Slot::acquire(&state.running, state.settings.concurrency) else {
        return fail(503, "busy");
    };
    // Cancellable from here on: the flag is registered before any admission work.
    let cancelled = Arc::new(AtomicBool::new(false));
    let running = Arc::new(Running::new(Arc::clone(&cancelled)));
    let Some(registered) = Registered::new(state, &request, Arc::clone(&running)) else {
        return fail(409, "duplicate_attempt");
    };
    let scopes = request.scopes();
    if cancelled.load(Ordering::Acquire) {
        return finished_cancelled(&registered);
    }
    let collector = Arc::new(Collector {
        lines: Mutex::new(Vec::new()),
    });
    let host = host::Host::with_limits(
        Context {
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
        },
        collector.clone(),
        limits(state, &request),
    )
    .with_cancellation(Arc::clone(&cancelled));
    let entry = match component(state, &request, &cancelled) {
        Ok(entry) => entry,
        Err(reply) => return reply,
    };
    let prepared = match host::Prepared::new(&entry.loaded, host) {
        Ok(prepared) => prepared,
        Err(execution) => return finished(&request, &registered, collector, execution),
    };
    let number = state.executions.fetch_add(1, Ordering::Relaxed) + 1;
    log::event(
        "execution_started",
        &[
            ("execution", log::Value::Number(number)),
            (
                "running",
                log::Value::Number(state.running.load(Ordering::Acquire) as u64),
            ),
        ],
    );
    let execution = prepared.call(&request.guest_input());
    log::event(
        "execution_finished",
        &[
            ("execution", log::Value::Number(number)),
            (
                "outcome",
                log::Value::Text(match &execution.result {
                    Ok(_) => "ok",
                    Err(error) => error.code,
                }),
            ),
        ],
    );
    finished(&request, &registered, collector, execution)
}

/// A cancel arrived while the request was still being admitted: nothing ran, so the cancel is
/// acknowledged and the request answers 409 `cancelled`.
fn finished_cancelled(registered: &Registered<'_>) -> Reply {
    registered.running.finish(Finish::Acknowledged);
    fail(409, "cancelled")
}

/// Tells a waiting cancel how the execution ended, then builds the response.
fn finished(
    request: &ExecuteRequest,
    registered: &Registered<'_>,
    collector: Arc<Collector>,
    execution: host::Execution,
) -> Reply {
    registered.running.finish(match execution.outcome {
        host::outcome::InvocationOutcome::Uncertain => Finish::Uncertain,
        _ => Finish::Acknowledged,
    });
    let lines = std::mem::take(&mut *locked(&collector.lines));
    // The credential set is only collected when there is text to redact.
    let logs = if lines.is_empty() {
        lines
    } else {
        redact_logs(&request.secrets(), lines)
    };
    match execution.result {
        Ok(output) => match ExecutionResult::success(request, &output, logs, ENGINE_VERSION) {
            Some(result) => match serde_json::to_vec(&result) {
                Ok(body) => (200, body),
                Err(_) => fail(500, "internal"),
            },
            None => fail(502, "invalid_handler_result"),
        },
        Err(error) => (
            failure_status(error.code),
            serde_json::json!({ "error": error.code, "logs": logs })
                .to_string()
                .into_bytes(),
        ),
    }
}
