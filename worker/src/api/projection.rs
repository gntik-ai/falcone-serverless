//! The pinned revision projection: everything the worker needs to run one revision.

use super::grants::{Grant, GrantVersion, all_valid};
use super::types::{
    CatalogId, CatalogVersion, Digest, Id, InterfaceVersion, Positive, RuntimeMode, Text,
};
use super::wire::{present, redacted};
use super::{Malformed, constraint};
use serde::{Deserialize, Serialize};

/// The base a revision pins: a catalog base resolved to `id@version` and its digest, or a
/// bring-your-own image with its signer identity and attestation reference.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Base {
    Catalog {
        id: CatalogId,
        version: CatalogVersion,
        digest: Digest,
    },
    Byo {
        image_digest: Digest,
        signer_identity: Text,
        attestation_ref: Text,
    },
}
redacted!(Base);

impl Base {
    fn digest(&self) -> &Digest {
        match self {
            Self::Catalog { digest, .. } => digest,
            Self::Byo { image_digest, .. } => image_digest,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageProfile {
    name: Text,
    version: Text,
}
redacted!(LanguageProfile);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    fuel: Positive,
    memory_bytes: Positive,
    timeout_ms: Positive,
    max_host_calls: Positive,
}
redacted!(Budgets);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub(super) revision: Id,
    pub(super) runtime_mode: RuntimeMode,
    /// Absent when the revision pins no base; `null` is malformed.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    base: Option<Base>,
    pub(super) artifact_digest: Digest,
    component_digest: Digest,
    pub(super) language_profile: LanguageProfile,
    pub(super) engine_version: Text,
    wasi_version: Text,
    pub(super) interface_version: InterfaceVersion,
    imports: Vec<Text>,
    budgets: Budgets,
    grants: Vec<Grant>,
}
redacted!(Projection);

impl Projection {
    /// The rules a schema cannot attach to one field.
    pub(crate) fn check(&self) -> Result<(), Malformed> {
        if all_valid(&self.grants, GrantVersion::V020) {
            Ok(())
        } else {
            Err(constraint("projection.grants"))
        }
    }

    pub(crate) fn base_digest(&self) -> Option<&Digest> {
        self.base.as_ref().map(Base::digest)
    }
}

impl Projection {
    pub(crate) fn component_digest(&self) -> &str {
        self.component_digest.as_str()
    }
    /// Check validated component import names against this revision's declarations.
    /// Global import/type validation must precede this per-projection admission check.
    pub fn check_imports(&self, imports: &[String]) -> Result<(), Malformed> {
        if imports
            .iter()
            .all(|name| self.imports.iter().any(|import| import.as_str() == name))
        {
            Ok(())
        } else {
            Err(constraint("projection.imports"))
        }
    }
}

impl Projection {
    pub(crate) fn engine_version(&self) -> &str {
        self.engine_version.as_str()
    }
}
