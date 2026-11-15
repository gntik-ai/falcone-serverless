//! What the execution service needs from an admitted request: the execution plan, the limits
//! the revision's budgets impose, and the result envelope with its runtime identity.
//!
//! Nothing here runs a guest. Every value that reaches a result is either request-controlled
//! data the caller has redacted or a fixed code from [`Refusal`].

use super::projection::Projection;
use super::refusal::{Class, Code, Reason, Refusal};
use super::request::ExecuteRequest;
use super::types::{Id, Positive};
use super::{Rejection, parse_result};
use crate::artifact::cache_key::ENGINE_VERSION;
use crate::limits::{Field, GUEST_MEMORY_BYTES, Limits};
use serde_json::{Map, Value, json};
use std::time::Duration;

/// The worker's own ceilings, from its runtime configuration.
#[derive(Clone, Copy)]
pub struct Ceilings {
    pub max_memory_bytes: u64,
    pub max_wall_clock_ms: u64,
}

impl std::fmt::Debug for Ceilings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Ceilings({} bytes, {} ms)",
            self.max_memory_bytes, self.max_wall_clock_ms
        )
    }
}

/// The limits one execution runs under: the revision's budgets, never more than the worker
/// allows. A budget the worker cannot honour is refused, never replaced by a default.
#[derive(Clone, Copy)]
pub struct ExecutionLimits {
    pub limits: Limits,
    pub fuel: u64,
    pub max_host_calls: u64,
    /// The shorter of the projection's `timeout_ms` and the worker's wall-clock limit.
    pub timeout: Duration,
}

impl std::fmt::Debug for ExecutionLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ExecutionLimits(fuel {}, host calls {}, timeout {:?})",
            self.fuel, self.max_host_calls, self.timeout
        )
    }
}

/// Everything the service reads from an execute request, as plain values.
pub struct Plan {
    pub activation_id: String,
    pub attempt_id: String,
    pub revision: String,
    pub artifact_digest: String,
    pub imports: Vec<String>,
    pub context: crate::host::Context,
    /// Claim strings of the credential. They stay in the host binding layer.
    pub secrets: Vec<String>,
    pub params: Vec<u8>,
    /// The credential claims as JSON. They go to the host binding layer and nowhere else.
    pub credential: String,
    /// The request's absolute `deadline` in milliseconds since the Unix epoch.
    pub request_deadline_millis: u64,
}

impl ExecuteRequest {
    pub fn plan(&self) -> Plan {
        Plan {
            activation_id: self.activation_id().as_str().to_owned(),
            attempt_id: self.attempt_id().as_str().to_owned(),
            revision: self.projection.revision.as_str().to_owned(),
            artifact_digest: self.projection.artifact_digest.as_str().to_owned(),
            imports: self.projection.declared_imports(),
            context: self.host_context(),
            secrets: self.credential_secrets(),
            params: self.params_json(),
            credential: self.credential_json(),
            request_deadline_millis: self.request_deadline_millis(),
        }
    }

    /// The limits of this execution, or the refusal naming why the worker cannot honour them.
    ///
    /// - memory: the minimum of `budgets.memory_bytes`, the worker's `max_memory_bytes` and
    ///   the compatibility ceiling; a request above the minimum of the last two is refused
    ///   with `budget_above_worker_ceiling`;
    /// - fuel and `max_host_calls` are the revision's budgets as given;
    /// - the timeout is the shorter of `timeout_ms` and the worker's wall-clock limit.
    ///
    /// A projection whose engine is not the worker's pinned Wasmtime is refused too, because
    /// the result's runtime identity must name the engine that ran.
    pub fn execution_limits(&self, worker: Ceilings) -> Result<ExecutionLimits, Refusal> {
        let invalid = |reason| Refusal::new(Code::InvalidRequest, reason);
        if self.projection.engine_version.as_str() != ENGINE_VERSION {
            return Err(invalid(Reason::EngineVersionMismatch));
        }
        let budgets = self.projection.budgets();
        let ceiling = worker.max_memory_bytes.min(GUEST_MEMORY_BYTES as u64);
        if budgets.memory_bytes() > ceiling {
            return Err(invalid(Reason::BudgetAboveWorkerCeiling));
        }
        let limits = Limits::compatibility()
            .narrowed(Field::GuestMemoryBytes, budgets.memory_bytes())
            .narrowed(Field::GuestMemoryBytes, worker.max_memory_bytes);
        Ok(ExecutionLimits {
            limits,
            fuel: budgets.fuel(),
            max_host_calls: budgets.max_host_calls(),
            timeout: Duration::from_millis(budgets.timeout_ms().min(worker.max_wall_clock_ms)),
        })
    }

    /// The success envelope. `handler` is the guest's JSON result; an object with a
    /// `statusCode` is the legacy `{statusCode, headers?, body?}` result, and any other JSON
    /// value is the body of a 200. A result that is not JSON or breaks the envelope is the
    /// failure `invalid_handler_result`.
    pub fn success(&self, handler: &[u8], logs: Vec<String>) -> (u16, Vec<u8>) {
        match legacy_result(handler) {
            Some(result) => (200, self.envelope("success", result, None, logs)),
            None => self.failure(
                Refusal::new(Code::ExecutionFailed, Reason::InvalidHandlerResult),
                logs,
            ),
        }
    }

    /// A failure envelope carrying the activation identity. `logs` must already be redacted.
    pub fn failure(&self, refusal: Refusal, logs: Vec<String>) -> (u16, Vec<u8>) {
        let class = refusal.class();
        let result = serde_json::to_value(&refusal).unwrap_or(Value::Null);
        (
            refusal.status(),
            self.envelope("failure", result, Some(class), logs),
        )
    }

    fn envelope(
        &self,
        status: &str,
        result: Value,
        class: Option<Class>,
        logs: Vec<String>,
    ) -> Vec<u8> {
        let subject = Subject {
            activation_id: self.activation_id(),
            attempt_id: self.attempt_id(),
            attempt_number: self.attempt_number(),
            projection: &self.projection,
        };
        // The envelope the worker sends always validates against the request it answers.
        let bytes = subject.envelope(status, result, class, logs);
        if parse_result(&bytes, self).is_ok() {
            bytes
        } else {
            subject.fallback()
        }
    }
}

/// Whom a result answers: the activation, the attempt and the pinned revision.
struct Subject<'a> {
    activation_id: &'a Id,
    attempt_id: &'a Id,
    attempt_number: Positive,
    projection: &'a Projection,
}

impl Subject<'_> {
    fn identity(&self) -> Value {
        let mut identity = json!({
            "function_revision": self.projection.revision,
            "runtime_mode": self.projection.runtime_mode,
            "artifact_digest": self.projection.artifact_digest,
            "wasmtime_version": self.projection.engine_version,
            "language_profile": self.projection.language_profile,
            "interface_version": "0.2.0",
        });
        if let Some(base) = self.projection.base_digest() {
            identity["base_digest"] = json!(base);
        }
        identity
    }

    fn envelope(
        &self,
        status: &str,
        result: Value,
        class: Option<Class>,
        logs: Vec<String>,
    ) -> Vec<u8> {
        let mut message = json!({
            "version": "1",
            "type": "result",
            "activation_id": self.activation_id,
            "attempt_id": self.attempt_id,
            "attempt_number": self.attempt_number,
            "status": status,
            "result": result,
            "logs": logs,
            "identity": self.identity(),
        });
        if let Some(class) = class {
            message["class"] = serde_json::to_value(class).unwrap_or(Value::Null);
        }
        serde_json::to_vec(&message).unwrap_or_default()
    }

    /// A last resort that cannot fail validation: no handler data, no logs.
    fn fallback(&self) -> Vec<u8> {
        let refusal = Refusal::new(Code::ExecutionFailed, Reason::InternalError);
        let result = serde_json::to_value(&refusal).unwrap_or(Value::Null);
        self.envelope("failure", result, Some(refusal.class()), Vec::new())
    }
}

/// The legacy handler result of the guest's bytes, or `None` when they are not one.
fn legacy_result(bytes: &[u8]) -> Option<Value> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let result = match value {
        Value::Object(object) if object.contains_key("statusCode") => {
            let mut result = Map::new();
            for (key, value) in object {
                if !matches!(key.as_str(), "statusCode" | "headers" | "body") || value.is_null() {
                    // A key outside the legacy shape, or an explicit null, breaks it.
                    if value.is_null() && matches!(key.as_str(), "headers" | "body") {
                        continue;
                    }
                    return None;
                }
                result.insert(key, value);
            }
            Value::Object(result)
        }
        Value::Null => json!({"statusCode": 200}),
        other => json!({"statusCode": 200, "body": other}),
    };
    Some(result)
}

impl Rejection {
    /// The refusal that answers a request which did not become invocable.
    pub fn refusal(&self) -> Refusal {
        match self {
            Self::Malformed(_) => Refusal::new(Code::InvalidRequest, Reason::MalformedMessage),
            Self::Refused(refused) => refused.refusal.clone(),
        }
    }

    /// The HTTP status and body of that refusal. A request that was read in full is answered
    /// with a result envelope carrying its activation identity (the pinned projection's, so a
    /// non-`wasm` refusal names its runtime mode); anything else gets the bare failure.
    pub fn response(&self) -> (u16, Vec<u8>) {
        let refusal = self.refusal();
        let status = refusal.status();
        if let Self::Refused(refused) = self
            && let Some(attribution) = &refused.attribution
        {
            let subject = Subject {
                activation_id: &attribution.activation_id,
                attempt_id: &attribution.attempt_id,
                attempt_number: attribution.attempt_number,
                projection: &attribution.projection,
            };
            let class = refusal.class();
            let result = serde_json::to_value(&refusal).unwrap_or(Value::Null);
            let bytes = subject.envelope("failure", result, Some(class), Vec::new());
            if parse_result(&bytes, &*attribution.projection).is_ok() {
                return (status, bytes);
            }
        }
        (status, serde_json::to_vec(&refusal).unwrap_or_default())
    }
}
