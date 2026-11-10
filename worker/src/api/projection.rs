//! Pinned revision projection v1: what the worker is told about the revision it runs.

use super::types::{ContractError, Digest, Id};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::num::NonZeroU64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeMode {
    Wasm,
    Oci,
}

/// A catalog base reference `name@version`. Unresolved default aliases are malformed:
/// the name may not be `default` and the version must start with a digit, so a
/// version-less id, `latest` and similar aliases are rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CatalogBaseId(String);

impl TryFrom<String> for CatalogBaseId {
    type Error = ContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = value.split_once('@').is_some_and(|(name, version)| {
            let mut name_chars = name.chars();
            name_chars
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && name_chars.all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
                })
                && name != "default"
                && version.starts_with(|c: char| c.is_ascii_digit())
                && version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        });
        if valid {
            Ok(Self(value))
        } else {
            Err(ContractError::new(
                "v1: catalog base id must be name@version, not an unresolved alias",
            ))
        }
    }
}

impl From<CatalogBaseId> for String {
    fn from(value: CatalogBaseId) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogBase {
    pub id: CatalogBaseId,
    pub digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByoBase {
    pub image_digest: Digest,
    pub signer_identity: Id,
    pub attestation_ref: Id,
}

/// The base the revision is pinned to. A revision that pins none omits the field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Base {
    Catalog(CatalogBase),
    Byo(ByoBase),
}

impl Base {
    /// The digest the activation runtime identity reports as `base_digest`.
    pub fn digest(&self) -> &Digest {
        match self {
            Self::Catalog(base) => &base.digest,
            Self::Byo(base) => &base.image_digest,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageProfile {
    pub name: Id,
    pub version: Id,
}

/// Limits, not observations; all are positive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    pub fuel: NonZeroU64,
    pub memory_bytes: NonZeroU64,
    pub timeout_ms: NonZeroU64,
    pub max_host_calls: NonZeroU64,
}

/// A capability grant. `ctr-capability-grants` v1 is not pinned here, so a grant is an
/// opaque JSON object until that contract is.
pub type Grant = Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionProjection {
    pub revision: Id,
    pub runtime_mode: RuntimeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Base>,
    pub artifact_digest: Digest,
    pub component_digest: Digest,
    pub language_profile: LanguageProfile,
    pub engine_version: Id,
    pub wasi_version: Id,
    pub interface_version: Id,
    pub declared_imports: Vec<Id>,
    pub budgets: Budgets,
    pub granted_capabilities: Vec<Grant>,
}
