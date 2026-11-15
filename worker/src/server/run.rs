//! One execution, from admission to its result envelope.
//!
//! The absolute deadline `until` is computed once, at admission, from the shortest of the
//! request's own deadline, the projection's `timeout_ms` and the worker's wall-clock limit.
//! Nothing after admission derives a deadline from a duration: the supervised steps, the guest's
//! epoch deadline and every host call take `until` itself.
//!
//! Every outcome is chosen with `until` and the cancel flag first. A step that ends after
//! `until` is a timeout, and one that ends after a cancel is a cancellation, whatever it
//! returned and whether or not capacity was also exhausted.

use super::platform::{Capturing, Secret};
use super::redact::{Logs, Redactor};
use super::supervise::{self, Left};
use super::{Permit, Shared};
use crate::api::{self, Code, ExecutionLimits, Reason, Refusal};
use crate::artifact::supervisor::CompileLimits;
use crate::cache::{CONTRACT_VERSION, Loaded};
use crate::host::{self, outcome::InvocationOutcome};
use sha2::{Digest, Sha256};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Why a supervised compile step did not produce a component.
enum Compiled {
    Digest,
    Cache(&'static str),
}

/// Runs one execute request body and returns the HTTP status and the response body. The
/// execution slot is released before the response is written.
pub(super) fn execute(shared: &Arc<Shared>, body: &[u8], slot: Permit) -> (u16, Vec<u8>) {
    let request = match api::parse_request(body) {
        Ok(request) => request,
        Err(rejection) => return rejection.response(),
    };
    // Admission: the one place `until` is computed.
    let admitted = Instant::now();
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let plan = request.plan();
    let limits = match request.execution_limits(shared.config.ceilings) {
        Ok(limits) => limits,
        Err(refusal) => return request.failure(refusal, Vec::new()),
    };
    let until = admitted
        + limits.timeout.min(Duration::from_millis(
            plan.request_deadline_millis.saturating_sub(now_millis),
        ));
    let Some(registration) = shared.register((plan.activation_id.clone(), plan.attempt_id.clone()))
    else {
        // The deadline outranks an admission refusal: a duplicate of a running attempt whose
        // own deadline has already passed is a timeout, not busy.
        let refusal = match Instant::now() >= until {
            true => left(Left::Timeout),
            false => Refusal::new(Code::Busy, Reason::AtCapacity),
        };
        return request.failure(refusal, Vec::new());
    };
    let cancel = registration.entry.cancel.clone();
    // Building the matcher holds the slot, so it ends at the same `until` and at a cancel. An
    // interrupted build fails closed, and the step that follows reports the interruption.
    #[cfg(any(test, feature = "qualification-hooks"))]
    let hooks = shared.hooks.clone();
    let claims = plan.secrets.iter().map(|claim| {
        #[cfg(any(test, feature = "qualification-hooks"))]
        hooks.build(&cancel);
        claim.as_str()
    });
    let logs = Arc::new(Mutex::new(Logs::new(Redactor::new(claims, until, &cancel))));
    let ended = run(shared, &plan, limits, until, &cancel, &logs);
    // Every exit path takes its log lines from the redacting collector, here and only here.
    let lines = logs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .finish();
    let response = match ended {
        Ok(handler) => request.success(&handler, lines),
        Err(refusal) => request.failure(refusal, lines),
    };
    drop(registration);
    drop(slot);
    response
}

fn left(left: Left) -> Refusal {
    match left {
        Left::Timeout => Refusal::new(Code::Timeout, Reason::DeadlineExceeded),
        Left::Cancelled => Refusal::new(Code::Cancelled, Reason::CancelRequested),
        Left::Busy => Refusal::new(Code::Busy, Reason::AtCapacity),
    }
}

fn failed(reason: Reason) -> Refusal {
    Refusal::new(Code::ExecutionFailed, reason)
}

fn run(
    shared: &Arc<Shared>,
    plan: &api::Plan,
    limits: ExecutionLimits,
    until: Instant,
    cancel: &Arc<AtomicBool>,
    logs: &Arc<Mutex<Logs>>,
) -> Result<Vec<u8>, Refusal> {
    // 1. The artifact, from the platform.
    let platform = shared.platform.clone();
    let digest = plan.artifact_digest.clone();
    let bytes = supervise::step(&shared.abandoned, until, cancel, move |_| {
        platform.artifact(&digest)
    })
    .map_err(left)?
    .map_err(|_| failed(Reason::ArtifactUnavailable))?;
    // 2. Digest, validation (once), declared imports and the cache operation, supervised.
    let mut cache = shared.cache();
    let expected = plan.artifact_digest.clone();
    let imports = plan.imports.clone();
    let compile = CompileLimits {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: until,
    };
    let loaded: Loaded = supervise::step(&shared.abandoned, until, cancel, move |abandoned| {
        if format!("sha256:{:x}", Sha256::digest(&bytes)) != expected {
            return Err(Compiled::Digest);
        }
        cache
            .compile_declared(
                CONTRACT_VERSION,
                &bytes,
                compile,
                &abandoned,
                Some(&imports),
            )
            .map_err(|error| Compiled::Cache(error.code))
    })
    .map_err(left)?
    .map_err(|error| match error {
        Compiled::Digest => failed(Reason::ArtifactRejected),
        Compiled::Cache("undeclared_import") => {
            Refusal::new(Code::UndeclaredImport, Reason::ImportNotDeclared)
        }
        Compiled::Cache("compile_deadline_exceeded") => left(Left::Timeout),
        Compiled::Cache("compile_cancelled") => left(Left::Cancelled),
        Compiled::Cache("trusted_cache_unavailable") => failed(Reason::InternalError),
        Compiled::Cache(_) => failed(Reason::ArtifactRejected),
    })?;
    // 3. Setup: binding the credential, under the same deadline and cancel flag.
    let platform = shared.platform.clone();
    let context = plan.context.clone();
    let credential = Secret::new(plan.credential.clone());
    #[cfg(any(test, feature = "qualification-hooks"))]
    let hooks = shared.hooks.clone();
    let bindings = supervise::step(&shared.abandoned, until, cancel, move |_| {
        #[cfg(any(test, feature = "qualification-hooks"))]
        hooks.setup();
        platform.bind(&context, credential)
    })
    .map_err(left)?
    .map_err(|_| failed(Reason::InternalError))?;
    // 4. The guest. Its host carries `until`, the cancel flag and the revision's budgets.
    let capturing = Arc::new(Capturing {
        inner: bindings,
        logs: logs.clone(),
    });
    let mut host = host::Host::with_limits(plan.context.clone(), capturing, limits.limits);
    host.set_cancellation(cancel.clone());
    host.set_until(until);
    host.set_fuel(limits.fuel);
    host.set_max_host_calls(limits.max_host_calls);
    // No guest starts at or after `until`, or after a cancel.
    if let Some(stop) = supervise::expired(until, cancel) {
        return Err(left(stop));
    }
    let prepared = match host::Prepared::new(&loaded, host) {
        Ok(prepared) => prepared,
        Err(execution) => return outcome(execution, logs, until, cancel),
    };
    // Instantiation is done and the handler is about to be invoked: the last point at which
    // `until` or a cancel keeps guest code from starting.
    if let Some(stop) = supervise::expired(until, cancel) {
        return Err(left(stop));
    }
    #[cfg(any(test, feature = "qualification-hooks"))]
    shared.hooks.guest_start();
    outcome(prepared.call(&plan.params), logs, until, cancel)
}

/// Maps what the guest did to the handler bytes or the failure.
///
/// A host call that was cut off by `until` or a cancel may have left a mutation that still
/// commits, which the engine reports as `Uncertain`. At this boundary the interruption wins:
/// the answer is `timeout` or `cancelled`, whose class is `uncertain` as well.
fn outcome(
    execution: host::Execution,
    logs: &Arc<Mutex<Logs>>,
    until: Instant,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Refusal> {
    match execution.outcome {
        InvocationOutcome::GuestInterrupted => return Err(left(Left::Timeout)),
        InvocationOutcome::Cancelled => return Err(left(Left::Cancelled)),
        InvocationOutcome::GuestMemoryLimit => return Err(failed(Reason::MemoryBudgetExceeded)),
        InvocationOutcome::FuelExhausted => return Err(failed(Reason::FuelExhausted)),
        InvocationOutcome::HostCallBudget => return Err(failed(Reason::HostCallBudgetExceeded)),
        InvocationOutcome::BufferLimit => return Err(failed(Reason::BufferLimitExceeded)),
        InvocationOutcome::Uncertain => {
            return Err(match supervise::expired(until, cancel) {
                Some(stop) => left(stop),
                None => failed(Reason::UncertainSideEffects),
            });
        }
        InvocationOutcome::Success | InvocationOutcome::Failed => {}
    }
    match execution.result {
        Ok(bytes) => Ok(bytes),
        Err(error) => {
            // The guest's own message is a log line, redacted like every other.
            if let Some(message) = &error.message {
                logs.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(&format!("handler error: {message}"));
            }
            Err(failed(match error.code {
                "handler_failed" => Reason::HandlerFailed,
                "invalid_input" => Reason::InvalidInput,
                "handler_trapped" => Reason::GuestTrapped,
                "instantiation_failed" | "missing_handler_export" | "invalid_handler_signature" => {
                    Reason::InstantiationFailed
                }
                _ => Reason::InternalError,
            }))
        }
    }
}
