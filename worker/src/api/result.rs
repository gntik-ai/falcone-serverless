//! The execution result: the legacy `status`, `result` and `logs` envelope, a failure with
//! its class and code, and the activation runtime identity block every result carries.

use super::{
    MAX_RESULT_BYTES,
    error::{ErrorBody, Malformed, Reason},
    from_bytes_within,
    legacy::LegacyResponse,
    request::ExecuteRequest,
    types::{
        Digest, LanguageProfile, Name, Presence, Projection, Redacted, RuntimeMode, V1, present,
    },
};
use serde::{Deserialize, Serialize};

const MAX_LOG_LINES: usize = 1000;
const MAX_LOG_LINE_CHARS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Success,
    Failure,
}

/// What ran: the function revision, runtime mode, raw artifact digest, base digest when the
/// revision pins a base (absent otherwise, never `null`), Wasmtime version, language
/// profile and interface version.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub function_revision: Name,
    pub runtime_mode: RuntimeMode,
    pub artifact_digest: Digest,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub base_digest: Presence<Digest>,
    pub wasmtime_version: Name,
    pub language_profile: LanguageProfile,
    pub interface_version: Name,
}

impl Identity {
    /// The identity a revision's projection pins; the engine version is the Wasmtime build
    /// the revision requires.
    pub fn from_projection(projection: &Projection) -> Self {
        let base_digest = match projection.pinned_base() {
            Ok(Some(base)) => Presence::Value(base.digest().clone()),
            _ => Presence::Absent,
        };
        Self {
            function_revision: projection.revision.clone(),
            runtime_mode: projection.runtime_mode,
            artifact_digest: projection.artifact_digest.clone(),
            base_digest,
            wasmtime_version: projection.engine_version.clone(),
            language_profile: projection.language_profile.clone(),
            interface_version: projection.interface_version.clone(),
        }
    }

    /// The identity must repeat the projection, and `base_digest` is present exactly when the
    /// projection pins a base: `null` is never the same as absent.
    fn check(&self, projection: &Projection) -> Result<(), Malformed> {
        let base_digest = self.base_digest.value()?;
        match (projection.pinned_base()?, base_digest) {
            (Some(base), Some(digest)) if base.digest() == digest => {}
            (Some(_), Some(_)) => return Err(Malformed::Rule("identity_base_digest_mismatch")),
            (Some(_), None) => return Err(Malformed::Rule("base_digest_missing")),
            (None, Some(_)) => return Err(Malformed::Rule("base_digest_without_base")),
            (None, None) => {}
        }
        if self.function_revision == projection.revision
            && self.runtime_mode == projection.runtime_mode
            && self.artifact_digest == projection.artifact_digest
            && self.wasmtime_version == projection.engine_version
            && self.language_profile == projection.language_profile
            && self.interface_version == projection.interface_version
        {
            Ok(())
        } else {
            Err(Malformed::Rule("identity_mismatch"))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResult {
    pub contract_version: V1,
    pub activation_id: Name,
    pub attempt_id: Name,
    pub status: Status,
    /// The legacy result envelope; present exactly when `status` is `success`.
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub result: Presence<LegacyResponse>,
    /// Present exactly when `status` is `failure`.
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub error: Presence<ErrorBody>,
    pub logs: Redacted<Vec<String>>,
    pub identity: Identity,
}

impl ExecutionResult {
    pub fn success(request: &ExecuteRequest, response: LegacyResponse, logs: Vec<String>) -> Self {
        Self {
            contract_version: V1,
            activation_id: request.activation_id.clone(),
            attempt_id: request.attempt_id.clone(),
            status: Status::Success,
            result: Presence::Value(response),
            error: Presence::Absent,
            logs: Redacted::new(logs),
            identity: Identity::from_projection(&request.projection),
        }
    }

    pub fn failure(request: &ExecuteRequest, reason: Reason) -> Self {
        Self::failure_with(
            request.activation_id.clone(),
            request.attempt_id.clone(),
            Identity::from_projection(&request.projection),
            reason,
        )
    }

    pub(super) fn failure_with(
        activation_id: Name,
        attempt_id: Name,
        identity: Identity,
        reason: Reason,
    ) -> Self {
        Self {
            contract_version: V1,
            activation_id,
            attempt_id,
            status: Status::Failure,
            result: Presence::Absent,
            error: Presence::Value(ErrorBody::new(reason)),
            logs: Redacted::new(Vec::new()),
            identity,
        }
    }

    /// Check this result against the projection it answers; enforced in every build.
    pub fn check_identity(&self, projection: &Projection) -> Result<(), Malformed> {
        self.identity.check(projection)
    }

    fn check<A: Answered + ?Sized>(&self, answered: &A) -> Result<(), Malformed> {
        if let Some((activation, attempt)) = answered.attempt()
            && (&self.activation_id != activation || &self.attempt_id != attempt)
        {
            return Err(Malformed::Rule("result_binding"));
        }
        self.check_identity(answered.projection())?;
        let result = self.result.value()?;
        let error = self.error.value()?;
        match (self.status, result, error) {
            (Status::Success, Some(response), None) => response.check()?,
            (Status::Failure, None, Some(error)) => error.check()?,
            _ => return Err(Malformed::Rule("status")),
        }
        let logs = self.logs.get();
        if logs.len() > MAX_LOG_LINES
            || logs
                .iter()
                .any(|line| line.chars().count() > MAX_LOG_LINE_CHARS)
        {
            return Err(Malformed::Rule("logs"));
        }
        Ok(())
    }
}

/// What a result answers: the dispatched request, or just its projection. The dispatcher
/// builds requests for every runtime mode, so a projection alone must also be accepted.
pub trait Answered {
    fn projection(&self) -> &Projection;

    /// The activation and attempt the result must echo, when the context knows them.
    fn attempt(&self) -> Option<(&Name, &Name)>;
}

impl Answered for ExecuteRequest {
    fn projection(&self) -> &Projection {
        &self.projection
    }

    fn attempt(&self) -> Option<(&Name, &Name)> {
        Some((&self.activation_id, &self.attempt_id))
    }
}

impl Answered for Projection {
    fn projection(&self) -> &Projection {
        self
    }

    fn attempt(&self) -> Option<(&Name, &Name)> {
        None
    }
}

/// Parse a worker result for `answered` (a request or its projection). The identity block
/// must repeat the projection, including `base_digest` exactly when a base is pinned, and a
/// request's activation and attempt must be echoed.
pub fn parse_result<A: Answered + ?Sized>(
    bytes: &[u8],
    answered: &A,
) -> Result<ExecutionResult, Malformed> {
    let result: ExecutionResult = from_bytes_within(bytes, MAX_RESULT_BYTES)?;
    result.check(answered)?;
    Ok(result)
}
