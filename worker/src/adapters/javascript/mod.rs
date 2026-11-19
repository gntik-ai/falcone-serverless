//! JSON main(params) outcomes, using the frozen execution API's envelope mapping.

use super::{
    AdapterError,
    compat::{Source, check},
};
use crate::{
    api::{self, Code, ExecuteRequest, Reason, Refusal},
    host::Context,
    server::{MARKER, MAX_LINES, Redactor, SCAN_BUDGET_CHARS},
};
use serde_json::Value;
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub struct Input {
    pub params: Vec<u8>,
    pub context: Context,
}

impl Input {
    /// Private adapter packet carried by the existing WIT handler byte-list interface.
    /// The trusted wrapper calls main with the original params and a separate immutable
    /// identity. No credential, binding, scope or secret enters this packet.
    pub fn wire(&self) -> Result<Vec<u8>, Refusal> {
        let invalid = |_| Refusal::new(Code::InvalidRequest, Reason::MalformedMessage);
        let params: Value = serde_json::from_slice(&self.params).map_err(invalid)?;
        serde_json::to_vec(&serde_json::json!({"params":params,"context":{
            "tenant":self.context.tenant,"workspace":self.context.workspace,
            "invocation_key":self.context.invocation_key,"attempt_id":self.context.attempt_id,
            "idempotency_key":self.context.idempotency_key,
        }}))
        .map_err(invalid)
    }
}

pub struct Outcome {
    pub result: Result<Vec<u8>, Refusal>,
    pub logs: Vec<String>,
}

impl Outcome {
    /// Only a successful engine outcome and a returned handler value can be success.
    /// Guest exception text never becomes an envelope field or an implicit log line.
    pub fn from_execution(execution: crate::host::Execution, logs: Vec<String>) -> Self {
        use crate::host::outcome::InvocationOutcome;
        let failed = |reason| Refusal::new(Code::ExecutionFailed, reason);
        let result = match execution.outcome {
            InvocationOutcome::GuestInterrupted => {
                Err(Refusal::new(Code::Timeout, Reason::DeadlineExceeded))
            }
            InvocationOutcome::Cancelled => {
                Err(Refusal::new(Code::Cancelled, Reason::CancelRequested))
            }
            InvocationOutcome::GuestMemoryLimit => Err(failed(Reason::MemoryBudgetExceeded)),
            InvocationOutcome::BufferLimit => Err(failed(Reason::BufferLimitExceeded)),
            InvocationOutcome::FuelExhausted => Err(failed(Reason::FuelExhausted)),
            InvocationOutcome::HostCallBudget => Err(failed(Reason::HostCallBudgetExceeded)),
            InvocationOutcome::Uncertain => Err(failed(Reason::UncertainSideEffects)),
            InvocationOutcome::Success | InvocationOutcome::Failed => match execution.result {
                Ok(bytes) if execution.outcome == InvocationOutcome::Success => Ok(bytes),
                Ok(_) => Err(failed(Reason::HandlerFailed)),
                Err(error) => Err(failed(match error.code {
                    "invalid_input" => Reason::InvalidInput,
                    "handler_trapped" => Reason::GuestTrapped,
                    "instantiation_failed"
                    | "missing_handler_export"
                    | "invalid_handler_signature" => Reason::InstantiationFailed,
                    _ => Reason::HandlerFailed,
                })),
            },
        };
        Self { result, logs }
    }
}

pub(super) fn prepare(
    request: &ExecuteRequest,
    context: Option<&Context>,
    source: &str,
    manifest: &str,
) -> Result<Input, AdapterError> {
    let invalid = |message| AdapterError {
        refusal: Refusal::new(Code::InvalidRequest, Reason::MalformedMessage),
        message,
    };
    let context = context.ok_or_else(|| invalid("execution context missing"))?;
    let plan = request.plan();
    // Read only verified claims in the host binding layer. Never inspect params or headers
    // for identity. Recheck revision binding here even for requests admitted upstream.
    let credential: Value = serde_json::from_str(&plan.credential)
        .map_err(|_| invalid("execution context does not match the pinned revision"))?;
    let trusted = &plan.context;
    if credential["revision"].as_str() != Some(plan.revision.as_str())
        || context.tenant != trusted.tenant
        || context.workspace != trusted.workspace
        || context.invocation_key != trusted.invocation_key
        || context.attempt_id != trusted.attempt_id
        || context.idempotency_key != trusted.idempotency_key
    {
        return Err(invalid(
            "execution context does not match the pinned revision",
        ));
    }
    let bytes = serde_json::to_vec(request).map_err(|_| invalid("javascript request rejected"))?;
    api::parse_request(&bytes).map_err(|error| AdapterError {
        refusal: error.refusal(),
        message: "javascript request rejected",
    })?;
    if !check(&[Source { text: source }], manifest).supported() {
        return Err(AdapterError::blocked("javascript source blocked"));
    }
    // The supplied legacy context is checked but never used as authority (including grants).
    Ok(Input {
        params: plan.params,
        context: plan.context,
    })
}

pub(super) fn finish(request: &ExecuteRequest, outcome: Outcome) -> (u16, Vec<u8>) {
    let secrets = request.plan().secrets;
    let redactor = Redactor::new(
        secrets.iter().map(String::as_str),
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    );
    let mut budget = SCAN_BUDGET_CHARS;
    let mut logs = Vec::new();
    let mut overflowed = redactor.overflowed();
    for line in outcome.logs.iter().take(MAX_LINES) {
        match redactor.line(line, &mut budget) {
            Some(line) => logs.push(line),
            None => {
                overflowed = true;
                break;
            }
        }
    }
    if overflowed {
        logs = vec![MARKER.to_owned()];
    }
    match outcome.result {
        Ok(bytes) => request.success(&bytes, logs),
        Err(refusal) => request.failure(refusal, logs),
    }
}

/// The offline/operator qualification seam. It never exists in a default release and does
/// not change the registry or the matrix. The callback is the trusted Wasm executor, not JS.
#[cfg(any(test, feature = "qualification-hooks"))]
pub fn qualify(
    request: &ExecuteRequest,
    context: Option<&Context>,
    source: &str,
    manifest: &str,
    guest: impl FnOnce(Input) -> Outcome,
) -> Result<(u16, Vec<u8>), AdapterError> {
    let input = prepare(request, context, source, manifest)?;
    Ok(finish(request, guest(input)))
}
