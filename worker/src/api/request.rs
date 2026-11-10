//! The execute attempt: identifiers, deadline, execution credential claims, pinned revision
//! projection, legacy params and invocation identity.

use super::{
    CONTRACT_VERSION,
    error::{ApiError, MalformedReason, Mismatch},
    legacy::{Invocation, LegacyParams},
    redact::{Digest, Redacted, present},
};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Wasm,
    Oci,
}

/// A revision's pinned base, absent when the revision pins none. An unresolved default alias
/// is not a base: Falcone resolves it to `id@version` plus a digest before dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Base {
    Catalog {
        /// `<id>@<version>`, where the version starts with a digit.
        id: Redacted<String>,
        digest: Digest,
    },
    Byo {
        image_digest: Digest,
        signer_identity: Redacted<String>,
        attestation_ref: Redacted<String>,
    },
}

impl Base {
    /// The digest an activation identity records as `base_digest`.
    pub fn digest(&self) -> &Digest {
        match self {
            Self::Catalog { digest, .. } => digest,
            Self::Byo { image_digest, .. } => image_digest,
        }
    }

    fn check(&self) -> Result<(), ApiError> {
        match self {
            Self::Catalog { id, .. } => {
                let resolved = id.expose().split_once('@').is_some_and(|(name, version)| {
                    !name.is_empty() && version.starts_with(|c: char| c.is_ascii_digit())
                });
                if resolved {
                    Ok(())
                } else {
                    Err(ApiError::Malformed(MalformedReason::BaseAlias))
                }
            }
            Self::Byo {
                signer_identity,
                attestation_ref,
                ..
            } => non_empty([signer_identity.expose(), attestation_ref.expose()]),
        }
    }
}

/// Runtime budgets; the names follow `worker/src/limits`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    pub guest_memory_bytes: u64,
    pub guest_to_host_bytes: u64,
    pub host_to_guest_bytes: u64,
    pub guest_deadline_ms: u64,
    pub host_call_deadline_ms: u64,
    pub termination_budget_ms: u64,
}

/// The revision as pinned by Falcone. Later revisions of this contract only add optional
/// fields; OCI-only fields belong to OCI-04.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionProjection {
    pub revision: Redacted<String>,
    pub runtime_mode: RuntimeMode,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub base: Option<Base>,
    pub artifact_digest: Digest,
    pub component_digest: Digest,
    pub language_profile: Redacted<String>,
    /// The Wasmtime version the revision is pinned to.
    pub engine_version: Redacted<String>,
    pub wasi_version: Redacted<String>,
    pub interface_version: Redacted<String>,
    pub imports: Vec<Redacted<String>>,
    pub budgets: Budgets,
    pub granted_capabilities: Vec<Redacted<String>>,
}

impl RevisionProjection {
    fn check(&self) -> Result<(), ApiError> {
        non_empty([
            self.revision.expose(),
            self.language_profile.expose(),
            self.engine_version.expose(),
            self.wasi_version.expose(),
            self.interface_version.expose(),
        ])?;
        non_empty(
            self.imports
                .iter()
                .chain(&self.granted_capabilities)
                .map(Redacted::expose),
        )?;
        let budgets = &self.budgets;
        if [
            budgets.guest_memory_bytes,
            budgets.guest_to_host_bytes,
            budgets.host_to_guest_bytes,
            budgets.guest_deadline_ms,
            budgets.host_call_deadline_ms,
            budgets.termination_budget_ms,
        ]
        .contains(&0)
        {
            return Err(ApiError::Malformed(MalformedReason::Field));
        }
        self.base.as_ref().map_or(Ok(()), Base::check)
    }
}

/// The ctr-execution-credential v1 claims, carried structured and never as an opaque string
/// (claims schema sha256 9c98ba16...). A signed token is verified by the host capability
/// layer; this contract only binds the claims to the attempt.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCredential {
    pub iss: Redacted<String>,
    pub aud: Redacted<String>,
    pub tenant: Redacted<String>,
    pub workspace: Redacted<String>,
    pub pool_profile: Redacted<String>,
    pub revision: Redacted<String>,
    pub activation: Redacted<String>,
    pub attempt: Redacted<String>,
    pub iat: Number,
    pub exp: Number,
    /// capability-grants v1 grants, opaque to this contract.
    pub allowed_capabilities: Vec<Redacted<Value>>,
}

impl ExecutionCredential {
    fn check(&self) -> Result<(), ApiError> {
        non_empty([
            self.iss.expose(),
            self.aud.expose(),
            self.tenant.expose(),
            self.workspace.expose(),
            self.pool_profile.expose(),
            self.revision.expose(),
            self.activation.expose(),
            self.attempt.expose(),
        ])?;
        if [&self.iat, &self.exp]
            .iter()
            .any(|n| n.as_f64().is_none_or(|v| v < 0.0))
        {
            return Err(ApiError::Malformed(MalformedReason::Field));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteAttempt {
    pub contract_version: u32,
    pub activation_id: Redacted<String>,
    pub attempt_id: Redacted<String>,
    /// Epoch milliseconds, in the dispatcher's clock domain.
    pub deadline_unix_ms: u64,
    pub execution_credential: ExecutionCredential,
    pub projection: RevisionProjection,
    pub invocation: Invocation,
    pub params: LegacyParams,
}

impl ExecuteAttempt {
    /// Every v1 rule, compiled in all builds: field shapes, the credential's binding to this
    /// attempt, the invocation identity and the legacy envelope.
    pub fn check(&self) -> Result<(), ApiError> {
        if self.contract_version != CONTRACT_VERSION {
            return Err(ApiError::Malformed(MalformedReason::Version));
        }
        non_empty([self.activation_id.expose(), self.attempt_id.expose()])?;
        if self.deadline_unix_ms == 0 {
            return Err(ApiError::Malformed(MalformedReason::Field));
        }
        self.projection.check()?;
        let credential = &self.execution_credential;
        credential.check()?;
        for (claim, expected, field) in [
            (
                &credential.activation,
                &self.activation_id,
                Mismatch::CredentialActivation,
            ),
            (
                &credential.attempt,
                &self.attempt_id,
                Mismatch::CredentialAttempt,
            ),
            (
                &credential.revision,
                &self.projection.revision,
                Mismatch::CredentialRevision,
            ),
        ] {
            if claim != expected {
                return Err(ApiError::IdentityMismatch(field));
            }
        }
        self.invocation.check(self.attempt_id.expose())?;
        self.params.check()
    }
}

fn non_empty<'a>(values: impl IntoIterator<Item = &'a String>) -> Result<(), ApiError> {
    if values.into_iter().any(String::is_empty) {
        Err(ApiError::Malformed(MalformedReason::Field))
    } else {
        Ok(())
    }
}
