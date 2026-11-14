//! The execute message and the checks that admit it to the Wasm worker.

use super::grants::{Grant, GrantVersion, all_valid};
use super::projection::{Projection, RevisionBudgets};
use super::refusal::{Code, Reason, Refusal};
use super::types::{Deadline, Id, NonEmpty, Positive, RuntimeMode, Seconds, Version};
use super::wire::{constant, present, redacted};
use super::{Attribution, Refused, Rejection, constraint};
use serde::de::{Error as _, IgnoredAny};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

constant!(ExecuteType, "execute");

/// The size budget of a request body: `max-host-to-guest-bytes` in `worker/wit/host.wit`.
const MAX_BODY_BYTES: usize = 1_048_576;

/// The ctr-execution-credential v1 claims. The signed encoding belongs to that contract.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    iss: NonEmpty,
    aud: NonEmpty,
    tenant: NonEmpty,
    workspace: NonEmpty,
    pool_profile: NonEmpty,
    revision: NonEmpty,
    activation: NonEmpty,
    attempt: NonEmpty,
    iat: Seconds,
    exp: Seconds,
    allowed_capabilities: Vec<Grant>,
}
redacted!(Credential);

/// The invocation identity (`falcone:host/invocation-context`); `attempt-id` is `attempt_id`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    invocation_key: Id,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    idempotency_key: Option<Id>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    correlation_id: Option<Id>,
}
redacted!(Invocation);

/// The legacy `main(params)` object: `__ow_*` request fields beside the action parameters.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Params(Map<String, Value>);
redacted!(Params);

impl<'de> Deserialize<'de> for Params {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let map = Map::<String, Value>::deserialize(input)?;
        let strings = |key: &str| map.get(key).is_none_or(Value::is_string);
        let objects = |key: &str| map.get(key).is_none_or(Value::is_object);
        let headers = map.get("__ow_headers").is_none_or(|value| {
            value
                .as_object()
                .is_some_and(|headers| headers.values().all(Value::is_string))
        });
        if strings("__ow_method")
            && strings("__ow_path")
            && strings("__ow_body")
            && objects("__ow_query")
            && headers
        {
            Ok(Self(map))
        } else {
            Err(D::Error::custom("invalid value"))
        }
    }
}

impl Params {
    /// The legacy inputs rejected before invocation: nothing runs for any of them.
    fn check(&self) -> Result<(), Refusal> {
        let invalid = |reason| Refusal::new(Code::InvalidRequest, reason);
        if !self.0.contains_key("__ow_path") {
            return Err(invalid(Reason::MissingPath));
        }
        let Some(body) = self.0.get("__ow_body").and_then(Value::as_str) else {
            return Ok(());
        };
        if body.len() > MAX_BODY_BYTES {
            return Err(Refusal::new(Code::RequestTooLarge, Reason::BodyTooLarge));
        }
        if self.json_content_type() && serde_json::from_str::<IgnoredAny>(body).is_err() {
            return Err(invalid(Reason::NonJsonBody));
        }
        Ok(())
    }

    fn json_content_type(&self) -> bool {
        self.0
            .get("__ow_headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .filter_map(|(_, value)| value.as_str())
            .any(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
            })
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {
    version: Version,
    #[serde(rename = "type")]
    message: ExecuteType,
    activation_id: Id,
    attempt_id: Id,
    attempt_number: Positive,
    deadline: Deadline,
    invocation: Invocation,
    credential: Credential,
    pub(super) projection: Projection,
    params: Params,
}
redacted!(ExecuteRequest);

/// The worker's own ceilings, which no revision budget may exceed.
#[derive(Clone, Copy)]
pub struct WorkerCeilings {
    pub max_memory_bytes: u64,
    pub max_wall_clock_ms: u64,
}

/// The limits one execution runs under. Every field is enforced, none is advisory.
pub struct ExecutionLimits {
    /// Guest memory is the least of the revision budget, the worker's `max_memory_bytes`
    /// and the compatibility ceiling. Fixed copy sizes are the compatibility profile's.
    pub limits: crate::limits::Limits,
    pub fuel: u64,
    pub max_host_calls: u64,
}

/// What the worker takes from an admitted request. The credential stays in it: only the
/// host binding layer (`Platform::bind`) reads it, and no guest-visible value is derived
/// from it except the verified tenant and workspace of the host context.
pub struct Plan {
    pub activation_id: String,
    pub attempt_id: String,
    pub revision: String,
    pub artifact_digest: [u8; 32],
    /// The imports the revision declares; empty means none.
    pub declared_imports: Vec<String>,
    pub budgets: RevisionBudgets,
    /// The absolute deadline the dispatcher set.
    pub request_deadline: SystemTime,
    pub context: crate::host::Context,
    /// The legacy `main(params)` object as JSON.
    pub params: Vec<u8>,
    /// Every string of the credential claims, for log redaction.
    pub secrets: Vec<String>,
    credential: Credential,
}

impl Plan {
    pub fn credential(&self) -> &Credential {
        &self.credential
    }

    /// The shortest of the request deadline, the revision's `timeout_ms` and the worker's
    /// wall clock limit, measured from `now`. The caller turns it into one absolute
    /// instant at admission and passes that instant everywhere; it is never recomputed.
    pub fn effective_timeout(&self, now: SystemTime, ceilings: &WorkerCeilings) -> Duration {
        let requested = self
            .request_deadline
            .duration_since(now)
            .unwrap_or(Duration::ZERO);
        requested
            .min(Duration::from_millis(self.budgets.timeout_ms))
            .min(Duration::from_millis(ceilings.max_wall_clock_ms))
    }

    /// The budgets this worker can honour, or the refusal that says which it cannot. A
    /// budget is never replaced by a default.
    pub fn execution_limits(&self, ceilings: &WorkerCeilings) -> Result<ExecutionLimits, Refusal> {
        use crate::limits::{Field, Limits};
        let memory_ceiling = ceilings
            .max_memory_bytes
            .min(Limits::ceiling_of(Field::GuestMemoryBytes));
        let refuse = || Refusal::new(Code::InvalidRequest, Reason::BudgetAboveWorkerCeiling);
        if self.budgets.memory_bytes > memory_ceiling {
            return Err(refuse());
        }
        let memory = self
            .budgets
            .memory_bytes
            .min(ceilings.max_memory_bytes)
            .min(Limits::ceiling_of(Field::GuestMemoryBytes));
        let limits = Limits::compatibility()
            .with(Field::GuestMemoryBytes, memory)
            .map_err(|_| refuse())?;
        Ok(ExecutionLimits {
            limits,
            fuel: self.budgets.fuel,
            max_host_calls: self.budgets.max_host_calls,
        })
    }
}

fn collect_strings(value: &Value, into: &mut BTreeSet<String>) {
    match value {
        Value::String(text) if !text.is_empty() => {
            into.insert(text.clone());
        }
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, into)),
        Value::Object(fields) => fields.values().for_each(|item| collect_strings(item, into)),
        _ => {}
    }
}

fn digest_bytes(digest: &str) -> [u8; 32] {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest).as_bytes();
    let mut bytes = [0u8; 32];
    for (byte, pair) in bytes.iter_mut().zip(hex.chunks(2)) {
        let nibble = |c: u8| match c {
            b'0'..=b'9' => c - b'0',
            _ => c - b'a' + 10,
        };
        *byte = nibble(pair[0]) << 4 | nibble(pair[1]);
    }
    bytes
}

impl ExecuteRequest {
    /// The activation and attempt, which identify a running execution.
    pub fn key(&self) -> (String, String) {
        (
            self.activation_id.as_str().to_owned(),
            self.attempt_id.as_str().to_owned(),
        )
    }

    /// The execution plan of an admitted request.
    pub fn plan(&self) -> Plan {
        let mut secrets = BTreeSet::new();
        if let Ok(value) = serde_json::to_value(&self.credential) {
            collect_strings(&value, &mut secrets);
        }
        let mut context = crate::host::Context {
            grants: self.projection.host_grants(),
            tenant: self.credential.tenant.as_str().to_owned(),
            workspace: self.credential.workspace.as_str().to_owned(),
            invocation_key: self.invocation.invocation_key.as_str().to_owned(),
            attempt_id: self.attempt_id.as_str().to_owned(),
            idempotency_key: String::new(),
        };
        if let Some(key) = &self.invocation.idempotency_key {
            context.idempotency_key = key.as_str().to_owned();
        }
        Plan {
            activation_id: self.activation_id.as_str().to_owned(),
            attempt_id: self.attempt_id.as_str().to_owned(),
            revision: self.projection.revision.as_str().to_owned(),
            artifact_digest: digest_bytes(self.projection.artifact_digest.as_str()),
            declared_imports: self.projection.declared_imports(),
            budgets: self.projection.budgets(),
            request_deadline: self.deadline.to_system_time(),
            context,
            params: serde_json::to_vec(&self.params).unwrap_or_default(),
            secrets: secrets.into_iter().collect(),
            credential: self.credential.clone(),
        }
    }

    /// The worker's pinned engine must be the one the revision names: the result reports
    /// it as the runtime identity, and a mismatch would make that report untrue.
    pub fn check_engine(&self, pinned: &str) -> Result<(), Refusal> {
        if self.projection.engine_version() == pinned {
            Ok(())
        } else {
            Err(Refusal::new(
                Code::InvalidRequest,
                Reason::EngineVersionMismatch,
            ))
        }
    }

    pub(super) fn activation_id(&self) -> &Id {
        &self.activation_id
    }

    pub(super) fn attempt_id(&self) -> &Id {
        &self.attempt_id
    }

    pub(super) fn attempt_number(&self) -> Positive {
        self.attempt_number
    }

    pub fn attribution(&self) -> Attribution {
        Attribution {
            activation_id: self.activation_id.clone(),
            attempt_id: self.attempt_id.clone(),
            attempt_number: self.attempt_number,
            projection: Box::new(self.projection.clone()),
        }
    }

    /// Admission on the Wasm worker, in this order: a runtime mode other than `wasm` is
    /// refused; the grants must match their schemas' versions; the credential is bound to the
    /// request; the legacy inputs are checked. Nothing is invoked on any error.
    pub(super) fn admit(self) -> Result<Self, Rejection> {
        let refuse = |request: &Self, refusal| {
            Rejection::Refused(Refused::new(refusal, Some(request.attribution())))
        };
        if self.projection.runtime_mode != RuntimeMode::Wasm {
            let refusal = Refusal::new(Code::RuntimeModeNotSupported, Reason::RuntimeModeNotWasm);
            return Err(refuse(&self, refusal));
        }
        self.projection.check()?;
        if !all_valid(&self.credential.allowed_capabilities, GrantVersion::V010) {
            return Err(constraint("credential.allowed_capabilities").into());
        }
        if !(self.activation_id.same_as(&self.credential.activation)
            && self.attempt_id.same_as(&self.credential.attempt)
            && self.projection.revision.same_as(&self.credential.revision))
        {
            return Err(constraint("credential.binding").into());
        }
        self.params
            .check()
            .map_err(|refusal| refuse(&self, refusal))?;
        Ok(self)
    }
}
