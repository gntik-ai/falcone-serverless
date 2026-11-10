//! Execute attempt, execution updates and cancel.

use super::credential::ExecutionCredential;
use super::legacy::{LegacyHeaders, LegacyIdentity, LegacyInvocation};
use super::projection::RevisionProjection;
use super::result::{ExecutionResult, FailureClass};
use super::types::{ContractError, Deadline, Id, parse};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The invocation identity: the `invocation-key`, `attempt-id` and `idempotency-key`
/// of `falcone:host/invocation-context` plus the correlation id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationIdentity {
    pub invocation_key: Id,
    pub attempt_id: Id,
    pub idempotency_key: Id,
    pub correlation_id: Id,
}

impl InvocationIdentity {
    /// The legacy fixture form of the same identity.
    pub fn to_legacy(&self) -> LegacyInvocation {
        LegacyInvocation {
            headers: LegacyHeaders {
                correlation_id: self.correlation_id.clone(),
                idempotency_key: self.idempotency_key.clone(),
            },
            identity: LegacyIdentity {
                invocation_key: self.invocation_key.clone(),
                attempt_id: self.attempt_id.clone(),
                idempotency_key: self.idempotency_key.clone(),
            },
        }
    }
}

/// One execution attempt. Parse it with `from_value`/`from_slice`: they also enforce
/// the cross-field rules that plain deserialization cannot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteAttempt {
    pub activation_id: Id,
    pub attempt_id: Id,
    pub invocation: InvocationIdentity,
    pub projection: RevisionProjection,
    pub credential: ExecutionCredential,
    pub deadline: Deadline,
    pub params: Map<String, Value>,
}

impl ExecuteAttempt {
    pub fn from_value(value: &Value) -> Result<Self, ContractError> {
        let attempt: Self = parse(value)?;
        attempt.validate()?;
        Ok(attempt)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, ContractError> {
        let value = serde_json::from_slice(bytes).map_err(|e| ContractError::from_serde(&e))?;
        Self::from_value(&value)
    }

    /// The request and credential must describe the same attempt of the same revision,
    /// and the credential may narrow the revision's grants but never widen them.
    fn validate(&self) -> Result<(), ContractError> {
        let claims = &self.credential.claims;
        if self.invocation.attempt_id != self.attempt_id {
            return Err(ContractError::new(
                "v1: invocation.attempt_id differs from attempt_id",
            ));
        }
        if claims.activation != self.activation_id || claims.attempt != self.attempt_id {
            return Err(ContractError::new(
                "v1: credential is bound to another attempt",
            ));
        }
        if claims.revision != self.projection.revision {
            return Err(ContractError::new(
                "v1: credential is bound to another revision",
            ));
        }
        if !claims
            .allowed_capabilities
            .iter()
            .all(|grant| self.projection.granted_capabilities.contains(grant))
        {
            return Err(ContractError::new(
                "v1: credential widens the revision grants",
            ));
        }
        Ok(())
    }

    /// Checks a result against the attempt that produced it: same attempt, and an
    /// activation runtime identity that describes the pinned revision. `base_digest`
    /// is present exactly when the revision pins a base.
    pub fn check_result(&self, result: &ExecutionResult) -> Result<(), ContractError> {
        let identity = &result.identity;
        let projection = &self.projection;
        if result.activation_id != self.activation_id || result.attempt_id != self.attempt_id {
            return Err(ContractError::new("v1: result belongs to another attempt"));
        }
        if identity.function_revision != projection.revision {
            return Err(ContractError::new("v1: identity names another revision"));
        }
        if result.is_refusal() {
            // A refusal ran nothing: only the mode it saw has to agree.
            return match identity.runtime_mode {
                Some(mode) if mode != projection.runtime_mode => Err(ContractError::new(
                    "v1: identity runtime_mode differs from the projection",
                )),
                _ => Ok(()),
            };
        }
        if identity.runtime_mode != Some(projection.runtime_mode) {
            return Err(ContractError::new(
                "v1: identity runtime_mode differs from the projection",
            ));
        }
        if identity.base_digest.as_ref() != projection.base.as_ref().map(|base| base.digest()) {
            return Err(ContractError::new(
                "v1: base_digest must be present exactly when the revision pins a base, and equal it",
            ));
        }
        if identity
            .artifact_digest
            .as_ref()
            .is_some_and(|d| *d != projection.artifact_digest)
            || identity
                .wasmtime_version
                .as_ref()
                .is_some_and(|v| *v != projection.engine_version)
            || identity
                .interface_version
                .as_ref()
                .is_some_and(|v| *v != projection.interface_version)
            || identity
                .language_profile
                .as_ref()
                .is_some_and(|p| *p != projection.language_profile)
        {
            return Err(ContractError::new(
                "v1: identity differs from the pinned projection",
            ));
        }
        Ok(())
    }
}

/// A non-negative percentage, 0 through 100.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Percent(u8);

impl TryFrom<u8> for Percent {
    type Error = ContractError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err(ContractError::new("v1: percent must be 0-100"))
        }
    }
}

impl From<Percent> for u8 {
    fn from(value: Percent) -> Self {
        value.0
    }
}

/// An update sent by the worker while an attempt runs. A checkpoint is a reference to
/// the checkpoint object (see checkpoint-object.md), never inline state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateBody {
    Heartbeat {},
    Progress {
        percent: Percent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<Id>,
    },
    Checkpoint {
        checkpoint_ref: Id,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionUpdate {
    pub activation_id: Id,
    pub attempt_id: Id,
    /// Strictly increasing per attempt, starting at 1.
    pub sequence: u64,
    pub update: UpdateBody,
}

impl ExecutionUpdate {
    pub fn from_value(value: &Value) -> Result<Self, ContractError> {
        parse(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelState {
    Requested,
    Acknowledged,
    Uncertain,
}

impl CancelState {
    /// `uncertain` is the only state that is also a result class: the guest may or may
    /// not have stopped, and its side effects are unknown.
    pub fn outcome_class(self) -> Option<FailureClass> {
        match self {
            Self::Uncertain => Some(FailureClass::Uncertain),
            Self::Requested | Self::Acknowledged => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelMessage {
    pub activation_id: Id,
    pub attempt_id: Id,
    pub state: CancelState,
}
