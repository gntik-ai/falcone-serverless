//! The execute attempt: identity, deadline, execution credential and pinned revision projection.
use super::{
    error::{ParseError, Violation},
    text::{BaseId, BaseVersion, Digest, Grant, Name, Params, Positive, Seconds, present},
};
use serde::{Deserialize, Serialize};

/// The revision's runtime mode. It is required: absent, unknown or `null` is malformed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Wasm,
    Oci,
}

/// The legacy invocation identity. The wire names are the `falcone:host/invocation-context`
/// function names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationIdentity {
    #[serde(rename = "invocation-key")]
    pub invocation_key: Name,
    #[serde(rename = "attempt-id")]
    pub attempt_id: Name,
    #[serde(rename = "idempotency-key")]
    pub idempotency_key: Name,
}

/// `ctr-execution-credential` v1 claims, exactly as that contract's claims schema defines
/// them. The token signature and encoding stay with that contract (ADR 0006, Proposed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    pub iss: Name,
    pub aud: Name,
    pub tenant: Name,
    pub workspace: Name,
    pub pool_profile: Name,
    pub revision: Name,
    pub activation: Name,
    pub attempt: Name,
    pub iat: Seconds,
    pub exp: Seconds,
    pub allowed_capabilities: Vec<Grant>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub claims: Claims,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageProfile {
    pub name: Name,
    pub version: Name,
}

/// `{kind: catalog, id@version, digest}`; the version is exact, never an alias.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogBase {
    pub id: BaseId,
    pub version: BaseVersion,
    pub digest: Digest,
}

/// `{kind: byo, image digest, signer identity, attestation ref}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByoBase {
    pub image_digest: Digest,
    pub signer_identity: Name,
    pub attestation_ref: Name,
}

/// The pinned base. A revision that pins none omits the member; an unresolved default alias
/// has no representation and is malformed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Base {
    Catalog(CatalogBase),
    Byo(ByoBase),
}

impl Base {
    /// The digest the result identity must echo as `base_digest`.
    pub fn digest(&self) -> &Digest {
        match self {
            Self::Catalog(base) => &base.digest,
            Self::Byo(base) => &base.image_digest,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    pub fuel: Positive,
    pub memory_bytes: Positive,
    pub timeout_ms: Positive,
    pub max_host_calls: Positive,
}

/// The pinned revision projection v1. Falcone owns the revision record; this is what the
/// worker needs to run or refuse one attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionProjection {
    pub revision: Name,
    pub runtime_mode: RuntimeMode,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub base: Option<Base>,
    /// Digest of the raw validated artifact.
    pub artifact_digest: Digest,
    pub component_digest: Digest,
    pub language_profile: LanguageProfile,
    pub engine_version: Name,
    pub wasi_version: Name,
    pub interface_version: Name,
    pub imports: Vec<Name>,
    pub budgets: Budgets,
    pub capabilities: Vec<Grant>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {
    pub activation_id: Name,
    pub attempt_id: Name,
    /// One for the first attempt of the activation, then increasing.
    pub attempt_number: Positive,
    /// Absolute deadline, integer milliseconds in the dispatcher's clock domain.
    pub deadline_ms: u64,
    pub invocation: InvocationIdentity,
    pub credential: Credential,
    pub projection: RevisionProjection,
    pub params: Params,
}

impl ExecuteRequest {
    /// The attempt number as an integer, saturating at `u32::MAX`.
    pub(super) fn attempt(&self) -> u32 {
        u32::try_from(self.attempt_number.get()).unwrap_or(u32::MAX)
    }

    /// Cross-field rules: the invocation identity and the credential claims bind this attempt
    /// and revision, and the credential allows nothing the projection does not grant.
    fn check(&self) -> Result<(), Violation> {
        if self.invocation.attempt_id != self.attempt_id {
            return Err(Violation::IdentityMismatch);
        }
        let claims = &self.credential.claims;
        if claims.activation != self.activation_id
            || claims.attempt != self.attempt_id
            || claims.revision != self.projection.revision
            || claims.iat.value() > claims.exp.value()
        {
            return Err(Violation::CredentialMismatch);
        }
        if claims
            .allowed_capabilities
            .iter()
            .any(|grant| !self.projection.capabilities.contains(grant))
        {
            return Err(Violation::CapabilityNotGranted);
        }
        Ok(())
    }
}

/// Read an execute request. Errors carry no input text.
pub fn parse_request(bytes: &[u8]) -> Result<ExecuteRequest, ParseError> {
    let request: ExecuteRequest = serde_json::from_slice(bytes)?;
    request.check()?;
    Ok(request)
}
