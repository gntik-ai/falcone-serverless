//! One execution, from admission to its result.
//!
//! Admission fixes the execution's single absolute deadline, `until`, and registers it for
//! cancel and update. Everything after that is bounded by `until` and by the cancel flag, and
//! nothing derives a new deadline from a duration:
//!
//! 1. the artifact is read, validated once, checked against the revision's declared imports,
//!    loaded from or compiled into the trusted cache, as one abandonable step;
//! 2. the platform is bound (the host-only credential), as another abandonable step;
//! 3. the guest is instantiated and run by `host::execute_until`, which checks `until` again
//!    after instantiation and before the handler, and bounds every host call by it;
//! 4. captured logs are redacted and the result is built.
//!
//! The execution slot is released before the execution reports itself finished, so a cancel
//! that is answered `acknowledged` is followed by free capacity.

use super::platform::Platform;
use super::redact::Redactor;
use super::{Shared, Slot, Stop, bare, http, refuse, unix_now_millis};
use crate::api::{
    self, Code, ExecuteRequest, ExecutionResult, HandlerResult, Reason, Refusal, Refused, Rejection,
};
use crate::artifact::Error;
use crate::artifact::supervisor::CompileLimits;
use crate::artifact::validate::{ALLOWED_IMPORTS, Validated};
use crate::cache::{CONTRACT_VERSION, Cache, Loaded};
use crate::host::{self, Bindings, outcome::InvocationOutcome};
use crate::limits::{Field, Limits};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// The answer to a refused request: the result envelope when the request could be read far
/// enough to attribute it, otherwise the bare refusal.
fn refused_reply(refused: Refused) -> (u16, Vec<u8>) {
    let status = refused.refusal.status();
    match refused.attribution {
        Some(attribution) => {
            match ExecutionResult::answer(&attribution, Err(refused.refusal), Vec::new()) {
                Some(result) => (status, serde_json::to_vec(&result).unwrap_or_default()),
                None => refuse(Code::ExecutionFailed, Reason::SetupFailed),
            }
        }
        None => (status, bare(&refused.refusal)),
    }
}

/// Over capacity. A request whose headers arrived and whose whole body is already buffered, or
/// already in the socket, still gets `runtime_mode_not_supported` when that is what it is; every
/// other request gets `busy` without its body being read.
pub(super) fn busy(stream: &mut TcpStream, body: &mut Vec<u8>, length: usize) -> (u16, Vec<u8>) {
    http::drain_available(stream, body, length);
    if body.len() >= length
        && let Err(Rejection::Refused(refused)) = api::parse_request(&body[..length])
        && refused.refusal.code() == Code::RuntimeModeNotSupported
    {
        return refused_reply(refused);
    }
    refuse(Code::Busy, Reason::AtCapacity)
}

fn answer(
    request: &ExecuteRequest,
    outcome: Result<HandlerResult, Refusal>,
    logs: Vec<String>,
) -> (u16, Vec<u8>) {
    let status = match &outcome {
        Ok(_) => 200,
        Err(refusal) => refusal.status(),
    };
    match ExecutionResult::answer(request, outcome, logs) {
        Some(result) => (status, serde_json::to_vec(&result).unwrap_or_default()),
        None => refuse(Code::ExecutionFailed, Reason::SetupFailed),
    }
}

struct Finished {
    result: Result<HandlerResult, Refusal>,
    logs: Vec<String>,
    /// A mutation may still commit: the cancel answer is `uncertain`.
    uncertain: bool,
}

fn failed(code: Code, reason: Reason) -> Finished {
    Finished {
        result: Err(Refusal::new(code, reason)),
        logs: Vec::new(),
        uncertain: false,
    }
}

fn stopped(stop: Stop) -> Finished {
    match stop {
        Stop::Timeout => failed(Code::Timeout, Reason::DeadlineExceeded),
        Stop::Cancelled => failed(Code::Cancelled, Reason::CancelRequested),
        Stop::Busy => failed(Code::Busy, Reason::AtCapacity),
        Stop::Failed => failed(Code::ExecutionFailed, Reason::SetupFailed),
    }
}

/// Runs one admitted execute request. `slot` is the execution slot; it is released before the
/// execution is reported finished to a waiting cancel.
pub(super) fn execute(shared: &Shared, body: &[u8], slot: Slot) -> (u16, Vec<u8>) {
    let request = match api::parse_request(body) {
        Ok(request) => request,
        Err(Rejection::Refused(refused)) => return refused_reply(refused),
        Err(Rejection::Malformed(_)) => {
            return refuse(Code::InvalidRequest, Reason::MalformedRequest);
        }
    };
    let Some(running) = shared.registry.register(
        request.activation_id().as_str(),
        request.attempt_id().as_str(),
    ) else {
        let refusal = Refusal::new(Code::InvalidRequest, Reason::DuplicateExecution);
        return answer(&request, Err(refusal), Vec::new());
    };
    let finished = admitted(shared, &request, &running.cancel_flag());
    drop(slot);
    #[cfg(any(test, feature = "qualification"))]
    super::Marks::set(&shared.marks.released, Instant::now());
    running.finish(finished.uncertain);
    let reply = answer(&request, finished.result, finished.logs);
    drop(running);
    reply
}

fn admitted(shared: &Shared, request: &ExecuteRequest, cancel: &Arc<AtomicBool>) -> Finished {
    let settings = &shared.settings;
    let limits = match request.execution_limits(&settings.worker, unix_now_millis()) {
        Ok(limits) => limits,
        Err(refusal) => {
            return Finished {
                result: Err(refusal),
                logs: Vec::new(),
                uncertain: false,
            };
        }
    };
    // The one absolute deadline of this execution, fixed at admission.
    let until = Instant::now() + limits.timeout;
    #[cfg(any(test, feature = "qualification"))]
    super::Marks::set(&shared.marks.until, until);
    let projection = request.projection();
    // Nothing starts for an execution that has no time left, or that is already cancelled.
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return stopped(Stop::Cancelled);
    }
    if limits.timeout.is_zero() {
        return stopped(Stop::Timeout);
    }

    let component = {
        let cache = shared.cache.clone();
        let directory = settings.artifact_dir.clone();
        let digest = projection.artifact_digest().to_owned();
        let declared: Vec<String> = projection.declared_imports().map(str::to_owned).collect();
        let cancel = cancel.clone();
        shared.jobs.run(until, &cancel.clone(), move || {
            load_component(&cache, &directory, &digest, &declared, until, &cancel)
        })
    };
    let loaded = match component {
        Ok(Ok(loaded)) => loaded,
        Ok(Err(error)) => return load_failure(&error),
        Err(stop) => return stopped(stop),
    };

    let platform = {
        let secrets = request.credential_secrets();
        #[cfg(any(test, feature = "qualification"))]
        let hooks = settings.hooks.clone();
        let flag = cancel.clone();
        shared.jobs.run(until, cancel, move || {
            #[cfg(any(test, feature = "qualification"))]
            if let Some(probe) = &hooks.bind {
                probe();
            }
            let platform = Platform::bind(secrets, until, flag);
            #[cfg(any(test, feature = "qualification"))]
            let platform = match hooks.call.clone() {
                Some(probe) => platform.with_call_probe(probe),
                None => platform,
            };
            platform
        })
    };
    let platform = match platform {
        Ok(platform) => Arc::new(platform),
        Err(stop) => return stopped(stop),
    };

    let contract =
        match Limits::compatibility().bounded(Field::GuestMemoryBytes, limits.memory_bytes) {
            Ok(contract) => contract,
            Err(_) => return failed(Code::InvalidRequest, Reason::BudgetAboveWorkerCeiling),
        };
    let scopes = projection.grant_scopes();
    let context = host::Context {
        grants: host::Grants {
            prefixes: scopes.prefixes,
            hosts: scopes.hosts,
            topics: scopes.topics,
            targets: scopes.targets,
        },
        tenant: request.tenant().to_owned(),
        workspace: request.workspace().to_owned(),
        invocation_key: request.invocation_key().to_owned(),
        attempt_id: request.attempt_id().as_str().to_owned(),
        idempotency_key: request.idempotency_key().unwrap_or_default().to_owned(),
    };
    let bindings: Arc<dyn Bindings> = platform.clone();
    let host = host::Host::with_limits(context, bindings, contract)
        .with_cancellation(cancel.clone())
        .with_budgets(limits.fuel, limits.max_host_calls);
    #[cfg(any(test, feature = "qualification"))]
    let host = match settings.hooks.entry.clone() {
        Some(hook) => host.with_entry_hook(hook),
        None => host,
    };
    #[cfg(any(test, feature = "qualification"))]
    let host = match settings.hooks.start.clone() {
        Some(hook) => host.with_start_hook(hook),
        None => host,
    };
    let execution = host::execute_until(&loaded, host, &request.params_json(), until);
    let (result, uncertain) = outcome(execution);
    let logs = Redactor::new(platform.secrets()).redact(platform.take_logs());
    Finished {
        result,
        logs,
        uncertain,
    }
}

/// The artifact, validated once, with its imports checked against the revision's declaration
/// before anything is compiled, loaded or run.
fn load_component(
    cache: &Cache,
    directory: &Path,
    digest: &str,
    declared: &[String],
    until: Instant,
    cancel: &AtomicBool,
) -> Result<Loaded, Error> {
    let bytes = read_artifact(directory, digest)?;
    let limits = CompileLimits {
        input_bytes: MAX_ARTIFACT_BYTES,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: until,
    };
    cache.compile_shared(CONTRACT_VERSION, &bytes, limits, cancel, &|validated| {
        declared_imports(validated, declared)
    })
}

const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

fn artifact_error(code: &'static str) -> Error {
    Error { code, import: None }
}

fn read_artifact(directory: &Path, digest: &str) -> Result<Vec<u8>, Error> {
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| artifact_error("artifact_digest_mismatch"))?;
    let file = std::fs::File::open(directory.join(format!("{hex}.wasm")))
        .map_err(|_| artifact_error("artifact_unavailable"))?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(artifact_error("artifact_unavailable"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| artifact_error("artifact_unavailable"))?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(artifact_error("compile_size_limit_exceeded"));
    }
    if format!("sha256:{:x}", Sha256::digest(&bytes)) != digest {
        return Err(artifact_error("artifact_digest_mismatch"));
    }
    Ok(bytes)
}

/// Every import of the component must be declared by the revision and be an allowed interface.
fn declared_imports(validated: &Validated<'_>, declared: &[String]) -> Result<(), Error> {
    for import in &validated.imports {
        if !(declared.contains(import) && ALLOWED_IMPORTS.contains(&import.as_str())) {
            return Err(Error {
                code: "undeclared_import",
                import: Some(import.chars().take(256).collect()),
            });
        }
    }
    Ok(())
}

fn load_failure(error: &Error) -> Finished {
    match error.code {
        "undeclared_import" => failed(Code::UndeclaredImport, Reason::ImportNotDeclared),
        "compile_cancelled" => failed(Code::Cancelled, Reason::CancelRequested),
        "compile_deadline_exceeded" => failed(Code::Timeout, Reason::DeadlineExceeded),
        "artifact_unavailable" => failed(Code::ExecutionFailed, Reason::ArtifactUnavailable),
        "trusted_cache_unavailable" => failed(Code::ExecutionFailed, Reason::SetupFailed),
        _ => failed(Code::ExecutionFailed, Reason::ArtifactRejected),
    }
}

/// Maps what the host reports to the contract's outcome. The deadline outranks every budget,
/// and the order is that of `host::outcome::classify`.
fn outcome(execution: host::Execution) -> (Result<HandlerResult, Refusal>, bool) {
    let fail = |code, reason| Err(Refusal::new(code, reason));
    let failed_execution = |reason| fail(Code::ExecutionFailed, reason);
    match (execution.outcome, execution.result) {
        (InvocationOutcome::Success, Ok(bytes)) => match HandlerResult::from_guest(&bytes) {
            Some(handler) => (Ok(handler), false),
            None => (failed_execution(Reason::InvalidHandlerResult), false),
        },
        (InvocationOutcome::Success, Err(_)) => (failed_execution(Reason::HandlerFailed), false),
        (InvocationOutcome::Failed, Err(error)) => (
            failed_execution(match error.code {
                "handler_trapped" => Reason::GuestTrapped,
                "instantiation_failed"
                | "epoch_interruption_required"
                | "fuel_required"
                | "missing_handler_export"
                | "invalid_handler_signature" => Reason::SetupFailed,
                _ => Reason::HandlerFailed,
            }),
            false,
        ),
        (InvocationOutcome::Failed, Ok(_)) => (failed_execution(Reason::HandlerFailed), false),
        (InvocationOutcome::GuestInterrupted, _) => {
            (fail(Code::Timeout, Reason::DeadlineExceeded), false)
        }
        (InvocationOutcome::GuestMemoryLimit, _) => {
            (failed_execution(Reason::MemoryBudgetExceeded), false)
        }
        (InvocationOutcome::FuelExhausted, _) => (failed_execution(Reason::FuelExhausted), false),
        (InvocationOutcome::HostCallBudget, _) => {
            (failed_execution(Reason::HostCallBudgetExceeded), false)
        }
        (InvocationOutcome::BufferLimit, _) => {
            (failed_execution(Reason::BufferLimitExceeded), false)
        }
        (InvocationOutcome::Cancelled, _) => {
            (fail(Code::Cancelled, Reason::CancelRequested), false)
        }
        (InvocationOutcome::Uncertain, _) => (failed_execution(Reason::OutcomeUncertain), true),
    }
}
