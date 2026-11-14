//! One execution, from admission to result, under one absolute deadline.
//!
//! `until` is computed once, at admission, from the shortest of the request deadline, the
//! revision's `timeout_ms` and the worker's wall clock limit. The same instant bounds
//! artifact validation, compilation, binding, instantiation (the store's epoch deadline) and
//! every host call; nothing after admission derives a deadline from a duration. A stage that
//! crosses `until`, or sees the cancellation, ends the execution as `timeout` or `cancelled`
//! before the guest starts.

use super::bounded::{Helpers, Stop};
use super::platform::{Collecting, MAX_ARTIFACT_BYTES};
use super::redact::redact_logs;
use super::{Active, Services};
use crate::api::{Attribution, Code, ExecuteRequest, ExecutionResult, Plan, Reason, Refusal};
use crate::artifact::cache_key::ENGINE_VERSION;
use crate::artifact::supervisor::CompileLimits;
use crate::artifact::validate::{ALLOWED_IMPORTS, Checked, validate};
use crate::cache::CONTRACT_VERSION;
use crate::host::{self, outcome::InvocationOutcome};
use crate::limits::{Field, Limits};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// An HTTP status and the JSON body that carries one answer.
pub(super) struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub(super) fn bare(refusal: &Refusal) -> Self {
        Self {
            status: refusal.status(),
            body: serde_json::to_vec(refusal).unwrap_or_default(),
        }
    }

    fn of(result: &ExecutionResult) -> Self {
        Self {
            status: result.http_status(),
            body: result.to_vec(),
        }
    }
}

fn refusal(code: Code, reason: Reason) -> Refusal {
    Refusal::new(code, reason)
}

fn stopped(stop: Stop) -> Refusal {
    match stop {
        Stop::Timeout => refusal(Code::Timeout, Reason::DeadlineExceeded),
        Stop::Cancelled => refusal(Code::Cancelled, Reason::CancelRequested),
        Stop::Unavailable => refusal(Code::Busy, Reason::NoCapacity),
    }
}

/// A refusal of an execution that has not started: no guest ran and no logs exist.
fn answer(attribution: &Attribution, refusal: Refusal) -> Reply {
    Reply::of(&ExecutionResult::failure(attribution, refusal, Vec::new()))
}

/// What validating an artifact on a helper produced.
enum Validation {
    Valid(Arc<[u8]>, Checked, Vec<String>),
    Unavailable,
    Invalid(&'static str),
}

fn validate_artifact(
    services: &Services,
    plan: &Arc<Plan>,
    until: Instant,
    cancel: &Arc<AtomicBool>,
) -> Result<Validation, Stop> {
    let platform = services.platform.clone();
    let plan = plan.clone();
    let flag = cancel.clone();
    services.helpers.call(until, cancel, move || {
        let Ok(bytes) = platform.artifact(&plan, until, &flag) else {
            return Validation::Unavailable;
        };
        #[cfg(any(test, feature = "qualification"))]
        super::testing::validation_started(&plan.artifact_digest);
        let outcome = validate(&bytes, MAX_ARTIFACT_BYTES)
            .map(|validated| (validated.checked(), validated.imports.clone()));
        #[cfg(any(test, feature = "qualification"))]
        super::testing::validation_finished(&plan.artifact_digest);
        match outcome {
            Ok((checked, imports)) => Validation::Valid(bytes, checked, imports),
            Err(error) => Validation::Invalid(error.code),
        }
    })
}

/// Every import of the component must be declared by the revision and be one the worker
/// provides. An empty declaration means none.
fn imports_declared(plan: &Plan, imports: &[String]) -> bool {
    imports.iter().all(|import| {
        plan.declared_imports.contains(import) && ALLOWED_IMPORTS.contains(&import.as_str())
    })
}

fn compile_stop(code: &str) -> Option<Stop> {
    match code {
        "compile_cancelled" => Some(Stop::Cancelled),
        "compile_deadline_exceeded" => Some(Stop::Timeout),
        _ => None,
    }
}

/// Waits for the compile cache without outliving the execution.
fn lock_cache<'a>(
    services: &'a Services,
    until: Instant,
    cancel: &AtomicBool,
) -> Result<std::sync::MutexGuard<'a, crate::cache::Cache>, Stop> {
    loop {
        match services.cache.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if let Some(stop) = Helpers::stopped(until, cancel) {
                    return Err(stop);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

fn failed(outcome: InvocationOutcome, code: &str) -> Refusal {
    match outcome {
        InvocationOutcome::GuestInterrupted => refusal(Code::Timeout, Reason::DeadlineExceeded),
        InvocationOutcome::Cancelled => refusal(Code::Cancelled, Reason::CancelRequested),
        InvocationOutcome::Uncertain => {
            refusal(Code::ExecutionFailed, Reason::UncertainSideEffects)
        }
        InvocationOutcome::GuestMemoryLimit => {
            refusal(Code::ExecutionFailed, Reason::MemoryBudgetExceeded)
        }
        InvocationOutcome::BufferLimit => {
            refusal(Code::ExecutionFailed, Reason::BufferLimitExceeded)
        }
        InvocationOutcome::FuelExhausted => refusal(Code::ExecutionFailed, Reason::FuelExhausted),
        InvocationOutcome::HostCallBudget => {
            refusal(Code::ExecutionFailed, Reason::HostCallBudgetExceeded)
        }
        InvocationOutcome::Success | InvocationOutcome::Failed => {
            let reason = match code {
                "handler_trapped" => Reason::HandlerTrapped,
                "instantiation_failed" | "epoch_interruption_required" | "fuel_required" => {
                    Reason::InstantiationFailed
                }
                "missing_handler_export" | "invalid_handler_signature" => Reason::ArtifactRejected,
                "buffer_limit_exceeded" => Reason::BufferLimitExceeded,
                _ => Reason::HandlerFailed,
            };
            refusal(Code::ExecutionFailed, reason)
        }
    }
}

/// Runs an admitted request to its result.
pub(super) fn run(
    services: &Services,
    request: &ExecuteRequest,
    active: &Active,
    admitted: Instant,
) -> Reply {
    let attribution = request.attribution();
    let plan = Arc::new(request.plan());
    let cancel = active.cancel.clone();
    let ceilings = services.config.ceilings;
    // The one absolute deadline of this execution.
    let until = admitted + plan.effective_timeout(SystemTime::now(), &ceilings);
    let refuse = |refusal: Refusal| answer(&attribution, refusal);

    if let Err(error) = request.check_engine(ENGINE_VERSION) {
        return refuse(error);
    }
    let limits = match plan.execution_limits(&ceilings) {
        Ok(limits) => limits,
        Err(error) => return refuse(error),
    };
    if let Some(stop) = Helpers::stopped(until, &cancel) {
        return refuse(stopped(stop));
    }

    // Validate the artifact once, where the execution can leave it.
    let (bytes, checked, imports) = match validate_artifact(services, &plan, until, &cancel) {
        Err(stop) => return refuse(stopped(stop)),
        Ok(Validation::Unavailable) => {
            return refuse(refusal(Code::ExecutionFailed, Reason::ArtifactUnavailable));
        }
        Ok(Validation::Invalid("undeclared_import")) => {
            return refuse(refusal(Code::UndeclaredImport, Reason::UndeclaredImport));
        }
        Ok(Validation::Invalid(_)) => {
            return refuse(refusal(Code::ExecutionFailed, Reason::ArtifactRejected));
        }
        Ok(Validation::Valid(bytes, checked, imports)) => (bytes, checked, imports),
    };
    if let Some(stop) = Helpers::stopped(until, &cancel) {
        return refuse(stopped(stop));
    }
    if checked.artifact_digest != plan.artifact_digest {
        return refuse(refusal(
            Code::InvalidRequest,
            Reason::ArtifactDigestMismatch,
        ));
    }
    if !imports_declared(&plan, &imports) {
        return refuse(refusal(Code::UndeclaredImport, Reason::UndeclaredImport));
    }

    // Compile under the supervised trusted child, bounded by the same instant.
    let compile_limits = Limits::compatibility();
    let loaded = {
        let mut cache = match lock_cache(services, until, &cancel) {
            Ok(cache) => cache,
            Err(stop) => return refuse(stopped(stop)),
        };
        cache.compile_checked(
            CONTRACT_VERSION,
            &bytes,
            &checked,
            CompileLimits {
                input_bytes: MAX_ARTIFACT_BYTES,
                cpu_seconds: compile_limits.value(Field::CompilerCpuSeconds),
                memory_bytes: compile_limits.value(Field::CompilerMemoryBytes),
                wall_clock_deadline: until,
            },
            &cancel,
        )
    };
    drop(bytes);
    let loaded = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            // A cancelled or expired execution is `cancelled` or `timeout`, whatever the
            // compile reported.
            let stop = Helpers::stopped(until, &cancel).or_else(|| compile_stop(error.code));
            return refuse(match stop {
                Some(stop) => stopped(stop),
                None => refusal(Code::ExecutionFailed, Reason::CompileFailed),
            });
        }
    };

    // Bind the capabilities, again where the execution can leave it.
    let bindings = {
        let platform = services.platform.clone();
        let plan = plan.clone();
        let flag = cancel.clone();
        services
            .helpers
            .call(until, &cancel, move || platform.bind(&plan, until, &flag))
    };
    let bindings = match bindings {
        Err(stop) => return refuse(stopped(stop)),
        Ok(Err(_)) => return refuse(refusal(Code::ExecutionFailed, Reason::SetupFailed)),
        Ok(Ok(bindings)) => bindings,
    };
    // The guest never starts at or after `until`; `Prepared` checks again after
    // instantiation, immediately before the handler.
    if let Some(stop) = Helpers::stopped(until, &cancel) {
        return refuse(stopped(stop));
    }

    let lines = Arc::new(Mutex::new(Vec::new()));
    let mut host = host::Host::with_limits(
        plan.context.clone(),
        Arc::new(Collecting {
            inner: bindings,
            lines: lines.clone(),
        }),
        limits.limits,
    );
    host.set_cancellation(cancel.clone());
    host.set_budgets(limits.fuel, limits.max_host_calls);
    let execution = match host::Prepared::with_until(&loaded, host, until) {
        Ok(prepared) => prepared.call(&plan.params),
        Err(execution) => execution,
    };
    #[cfg(any(test, feature = "qualification"))]
    if execution.late_entry {
        super::testing::late_entry(&plan.context.attempt_id);
    }
    if execution.outcome == InvocationOutcome::Uncertain {
        active
            .uncertain
            .store(true, std::sync::atomic::Ordering::Release);
    }
    let logs = redact_logs(
        std::mem::take(
            &mut *lines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ),
        &plan.secrets,
    );
    match execution.result {
        Ok(bytes) if execution.outcome == InvocationOutcome::Success => {
            match ExecutionResult::success(&attribution, &bytes, logs.clone()) {
                Some(result) => Reply::of(&result),
                None => Reply::of(&ExecutionResult::failure(
                    &attribution,
                    refusal(Code::ExecutionFailed, Reason::InvalidHandlerResult),
                    logs,
                )),
            }
        }
        Ok(_) => Reply::of(&ExecutionResult::failure(
            &attribution,
            failed(execution.outcome, ""),
            logs,
        )),
        Err(error) => Reply::of(&ExecutionResult::failure(
            &attribution,
            failed(execution.outcome, error.code),
            logs,
        )),
    }
}
