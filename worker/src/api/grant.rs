//! Capability grants. The credential claims carry the canonical `@0.1.0` grants and the
//! request projection carries the `@0.2.0` grants of the revision runtime manifest; one type
//! serves both and [`Grant::check`] pins the version.

use super::{
    error::Malformed,
    types::{Presence, Text, present},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Storage,
    Http,
    Event,
    Workflow,
}

impl Family {
    fn interface(self, version: &str) -> String {
        let package = match self {
            Self::Storage => "storage",
            Self::Http => "http",
            Self::Event => "events",
            Self::Workflow => "workflows",
        };
        format!("falcone:host/{package}@{version}")
    }
}

/// A scope entry: non-empty and never a bare wildcard.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct ScopeEntry(String);

impl std::fmt::Debug for ScopeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl TryFrom<String> for ScopeEntry {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value == "*" {
            Err("scope entry")
        } else {
            Ok(Self(value))
        }
    }
}

impl From<ScopeEntry> for String {
    fn from(entry: ScopeEntry) -> String {
        entry.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageScope {
    pub prefixes: Vec<ScopeEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpScope {
    pub hosts: Vec<ScopeEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventScope {
    pub topics: Vec<ScopeEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowScope {
    pub targets: Vec<ScopeEntry>,
}

/// Exactly one family's field.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Scope {
    Storage(StorageScope),
    Http(HttpScope),
    Event(EventScope),
    Workflow(WorkflowScope),
}

impl Scope {
    fn family(&self) -> Family {
        match self {
            Self::Storage(_) => Family::Storage,
            Self::Http(_) => Family::Http,
            Self::Event(_) => Family::Event,
            Self::Workflow(_) => Family::Workflow,
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Storage(scope) => scope.prefixes.is_empty(),
            Self::Http(scope) => scope.hosts.is_empty(),
            Self::Event(scope) => scope.topics.is_empty(),
            Self::Workflow(scope) => scope.targets.is_empty(),
        }
    }
}

/// A reviewed narrow secret grant; the value never travels in the request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub name: Text,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub version: Presence<Text>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub family: Family,
    pub interface: Text,
    pub scope: Scope,
    #[serde(
        rename = "secretRef",
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub secret_ref: Presence<SecretRef>,
    #[serde(
        rename = "guestGrantId",
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub guest_grant_id: Presence<Text>,
}

impl Grant {
    /// The interface must be the family's own package at exactly `version`, and the scope
    /// must be that family's non-empty scope.
    pub(super) fn check(&self, version: &str) -> Result<(), Malformed> {
        if self.interface.as_str() != self.family.interface(version)
            || self.scope.family() != self.family
            || self.scope.is_empty()
        {
            return Err(Malformed::Rule("grant"));
        }
        if let Some(secret) = self.secret_ref.value()? {
            secret.version.value()?;
        }
        self.guest_grant_id.value()?;
        Ok(())
    }
}
