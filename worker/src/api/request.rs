//! The execute request: activation and attempt identity, the pinned revision projection,
//! the execution credential claims, the deadline and the legacy `main(params)` input.

use super::{
    error::{ApiError, Malformed, Reason, Refusal},
    from_bytes,
    grant::Grant,
    legacy::admit_legacy,
    result::Identity,
    types::{AttemptNumber, Deadline, Name, Projection, Redacted, RuntimeMode, Text, V1},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// The ctr-execution-credential v1 claims. The worker binds the attempt to them and hands
/// the grants to the host; the claims are never forwarded to a guest.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialClaims {
    pub iss: Text,
    pub aud: Text,
    pub tenant: Text,
    pub workspace: Text,
    pub pool_profile: Text,
    pub revision: Text,
    pub activation: Text,
    pub attempt: Text,
    pub iat: Number,
    pub exp: Number,
    pub allowed_capabilities: Vec<Grant>,
}

impl CredentialClaims {
    fn check(&self) -> Result<(), Malformed> {
        let non_negative = |n: &Number| n.as_f64().is_some_and(|v| v >= 0.0);
        if !(non_negative(&self.iat) && non_negative(&self.exp)) {
            return Err(Malformed::Rule("credential_time"));
        }
        self.allowed_capabilities
            .iter()
            .try_for_each(|grant| grant.check(super::CREDENTIAL_GRANT_VERSION))
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {
    pub contract_version: V1,
    pub activation_id: Name,
    pub attempt_id: Name,
    pub attempt_number: AttemptNumber,
    pub deadline: Deadline,
    pub credential: CredentialClaims,
    pub projection: Projection,
    pub params: Redacted<Map<String, Value>>,
}

impl ExecuteRequest {
    /// Rules the types do not state: grant versions, concrete bases, absent-not-null, and
    /// the binding of the credential to this attempt and revision.
    fn check(&self) -> Result<(), Malformed> {
        self.projection.check()?;
        self.credential.check()?;
        if self.credential.activation.as_str() != self.activation_id.as_str()
            || self.credential.attempt.as_str() != self.attempt_id.as_str()
            || self.credential.revision.as_str() != self.projection.revision.as_str()
        {
            return Err(Malformed::Rule("credential_binding"));
        }
        Ok(())
    }
}

/// Parse and validate an execute request. A well-formed request for any runtime mode other
/// than `wasm` is refused with a non-retryable result, never executed. For `wasm`, the legacy
/// admission rules apply to `params`: an input the contract rejects before invocation is
/// refused with its documented code and is never handed to the guest.
pub fn parse_request(bytes: &[u8]) -> Result<ExecuteRequest, ApiError> {
    let request: ExecuteRequest = from_bytes(bytes)?;
    request.check()?;
    let refuse = |reason| {
        ApiError::Refused(Box::new(Refusal::new(
            request.activation_id.clone(),
            request.attempt_id.clone(),
            request.attempt_number.get(),
            Identity::from_projection(&request.projection),
            reason,
        )))
    };
    if request.projection.runtime_mode != RuntimeMode::Wasm {
        return Err(refuse(Reason::RuntimeModeUnsupported));
    }
    admit_legacy(request.params.get(), |_| ()).map_err(|rejection| refuse(rejection.reason()))?;
    Ok(request)
}
