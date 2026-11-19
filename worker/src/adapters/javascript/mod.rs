//! Legacy JavaScript admission and host execution. No identity is read from guest inputs.
use super::compat::Verdict;
use crate::{api, cache::Loaded, host};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use std::time::Instant;

/// Fixed rule names only: formatting an error cannot disclose an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    ContextMissing,
    ContextMismatch,
    Registry(super::RegistryError),
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ContextMissing => "execution context missing",
            Self::ContextMismatch => "execution context does not match the pinned revision",
            Self::Registry(error) => return std::fmt::Display::fmt(error, f),
        })
    }
}

impl AdmissionError {
    pub fn response(self, request: &api::ExecuteRequest) -> (u16, Vec<u8>) {
        request.failure(
            api::Refusal::new(api::Code::InvalidRequest, api::Reason::MalformedMessage),
            Vec::new(),
        )
    }
}

fn context(plan: Option<&api::Plan>) -> Result<&api::Plan, AdmissionError> {
    let plan = plan.ok_or(AdmissionError::ContextMissing)?;
    if plan.context.tenant.is_empty()
        || plan.context.workspace.is_empty()
        || plan.context.invocation_key.is_empty()
        || plan.context.attempt_id.is_empty()
    {
        return Err(AdmissionError::ContextMissing);
    }
    if !plan.context_matches_credential() {
        return Err(AdmissionError::ContextMismatch);
    }
    Ok(plan)
}

/// Trusted runtime inputs, constructed only after admission. The component must have passed
/// the existing validation/trusted-compiler cache boundary; no native guest input is accepted.
pub struct Runtime<'a> {
    pub loaded: &'a Loaded,
    pub bindings: Arc<dyn host::Bindings>,
    pub limits: api::ExecutionLimits,
    pub until: Instant,
    pub cancelled: Arc<AtomicBool>,
}

/// The factory is the instantiation boundary. Every context refusal happens before it.
pub fn invoke<'a>(
    request: &api::ExecuteRequest,
    plan: Option<&api::Plan>,
    verdict: &Verdict,
    start: impl FnOnce() -> Runtime<'a>,
) -> Result<(u16, Vec<u8>), AdmissionError> {
    let plan = context(plan)?;
    super::admit_profile(request.language_profile_name(), verdict)
        .map_err(AdmissionError::Registry)?;
    Ok(run(request, plan, start()))
}

/// Qualification-only execution of a candidate component. This symbol does not exist in
/// production builds. It never changes the matrix, and still enforces context and source gates.
#[cfg(feature = "qualification-hooks")]
pub fn qualify<'a>(
    request: &api::ExecuteRequest,
    plan: Option<&api::Plan>,
    verdict: &Verdict,
    start: impl FnOnce() -> Runtime<'a>,
) -> Result<(u16, Vec<u8>), AdmissionError> {
    let plan = context(plan)?;
    if !verdict.is_supported() {
        return Err(AdmissionError::Registry(super::RegistryError::Blocked));
    }
    Ok(run(request, plan, start()))
}

struct Lines {
    redactor: crate::server::Redactor,
    budget: usize,
    overflow: bool,
    lines: Vec<String>,
}
impl Lines {
    fn push(&mut self, text: &str) {
        if self.overflow || self.lines.len() >= crate::server::MAX_LINES {
            return;
        }
        if let Some(line) = self.redactor.line(text, &mut self.budget) {
            self.lines.push(line);
        } else {
            self.overflow = true;
            self.lines.clear();
        }
    }
    fn finish(&mut self) -> Vec<String> {
        if self.overflow {
            vec![crate::server::MARKER.into()]
        } else {
            std::mem::take(&mut self.lines)
        }
    }
}
struct Capturing {
    inner: Arc<dyn host::Bindings>,
    logs: Mutex<Lines>,
}
impl host::Bindings for Capturing {
    fn call(
        &self,
        context: &host::Context,
        operation: host::Operation,
    ) -> Result<host::Reply, host::HostError> {
        match operation {
            host::Operation::Log {
                mut message,
                fields,
                ..
            } => {
                for field in fields {
                    message.push_str(&format!(" {}={}", field.name, field.value));
                }
                self.logs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(&message);
                Ok(host::Reply::Empty)
            }
            other => self.inner.call(context, other),
        }
    }
    fn resolve(&self, host: &str) -> Result<Vec<std::net::IpAddr>, host::HostError> {
        self.inner.resolve(host)
    }
    fn http(
        &self,
        context: &host::Context,
        request: &host::net::PinnedRequest,
    ) -> Result<host::net::Response, host::HostError> {
        self.inner.http(context, request)
    }
}

fn run(request: &api::ExecuteRequest, plan: &api::Plan, runtime: Runtime<'_>) -> (u16, Vec<u8>) {
    let claims = request.plan().secrets;
    let redactor = crate::server::Redactor::new(
        claims.iter().map(String::as_str),
        runtime.until,
        &runtime.cancelled,
    );
    let overflow = redactor.overflowed();
    let capturing = Arc::new(Capturing {
        inner: runtime.bindings,
        logs: Mutex::new(Lines {
            redactor,
            budget: crate::server::SCAN_BUDGET_CHARS,
            overflow,
            lines: Vec::new(),
        }),
    });
    let mut host = host::Host::with_limits(
        plan.context.clone(),
        capturing.clone(),
        runtime.limits.limits,
    );
    host.set_cancellation(runtime.cancelled);
    host.set_until(runtime.until);
    host.set_fuel(runtime.limits.fuel);
    host.set_max_host_calls(runtime.limits.max_host_calls);
    let execution = host::execute(runtime.loaded, host, &plan.params);
    let logs = capturing
        .logs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .finish();
    match outcome(execution) {
        Ok(bytes) => request.success(&bytes, logs),
        Err(refusal) => request.failure(refusal, logs),
    }
}

fn outcome(execution: host::Execution) -> Result<Vec<u8>, api::Refusal> {
    use api::{Code, Reason, Refusal};
    use host::outcome::InvocationOutcome as O;
    let failed = |reason| Refusal::new(Code::ExecutionFailed, reason);
    match execution.outcome {
        O::GuestInterrupted => return Err(Refusal::new(Code::Timeout, Reason::DeadlineExceeded)),
        O::Cancelled => return Err(Refusal::new(Code::Cancelled, Reason::CancelRequested)),
        O::GuestMemoryLimit => return Err(failed(Reason::MemoryBudgetExceeded)),
        O::FuelExhausted => return Err(failed(Reason::FuelExhausted)),
        O::HostCallBudget => return Err(failed(Reason::HostCallBudgetExceeded)),
        O::BufferLimit => return Err(failed(Reason::BufferLimitExceeded)),
        O::Uncertain => return Err(failed(Reason::UncertainSideEffects)),
        O::Failed | O::Success => {}
    }
    execution.result.map_err(|error| {
        failed(match error.code {
            "handler_failed" => Reason::HandlerFailed,
            "invalid_input" => Reason::InvalidInput,
            "handler_trapped" => Reason::GuestTrapped,
            "instantiation_failed" | "missing_handler_export" | "invalid_handler_signature" => {
                Reason::InstantiationFailed
            }
            _ => Reason::InternalError,
        })
    })
}
