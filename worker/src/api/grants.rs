//! Capability grants. The request's projection carries the revision-runtime-manifest grants
//! (`@0.2.0`); the credential's `allowed_capabilities` carry capability-grants v1 (`@0.1.0`).
//! One closed shape serves both; the version selects the only interface string it may name.

use super::types::NonEmpty;
use super::wire::{present, redacted, unit_debug};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantVersion {
    V010,
    V020,
}
unit_debug!(GrantVersion: V010, V020);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Storage,
    Http,
    Event,
    Workflow,
}
unit_debug!(Family: Storage, Http, Event, Workflow);

/// A non-empty list of non-empty entries, none of them a bare `*`.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Names(Vec<String>);

impl<'de> Deserialize<'de> for Names {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let names = Vec::<String>::deserialize(input)?;
        if !names.is_empty() && names.iter().all(|n| !n.is_empty() && n != "*") {
            Ok(Self(names))
        } else {
            Err(D::Error::custom("invalid value"))
        }
    }
}
redacted!(Names);

/// A closed scope: exactly the one field of the grant's family.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    prefixes: Option<Names>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    hosts: Option<Names>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    topics: Option<Names>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    targets: Option<Names>,
}
redacted!(Scope);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    name: NonEmpty,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    version: Option<NonEmpty>,
}
redacted!(SecretRef);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Grant {
    family: Family,
    interface: String,
    scope: Scope,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    secret_ref: Option<SecretRef>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    guest_grant_id: Option<NonEmpty>,
}
redacted!(Grant);

impl Grant {
    /// The family selects both the interface string for `version` and the scope field.
    fn valid(&self, version: GrantVersion) -> bool {
        let (interface, scope_field) = match self.family {
            Family::Storage => ("storage", &self.scope.prefixes),
            Family::Http => ("http", &self.scope.hosts),
            Family::Event => ("events", &self.scope.topics),
            Family::Workflow => ("workflows", &self.scope.targets),
        };
        let version = match version {
            GrantVersion::V010 => "0.1.0",
            GrantVersion::V020 => "0.2.0",
        };
        let fields = [
            &self.scope.prefixes,
            &self.scope.hosts,
            &self.scope.topics,
            &self.scope.targets,
        ];
        self.interface == format!("falcone:host/{interface}@{version}")
            && scope_field.is_some()
            && fields.iter().filter(|f| f.is_some()).count() == 1
    }
}

/// Whether every grant names the interface of `version` and the scope of its family.
pub(crate) fn all_valid(grants: &[Grant], version: GrantVersion) -> bool {
    grants.iter().all(|grant| grant.valid(version))
}

/// The concrete scope entries a request may use, per family. Only exact entries both the
/// revision (projection) and the credential grant survive; wildcards confer no authority.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Scopes {
    pub prefixes: Vec<String>,
    pub hosts: Vec<String>,
    pub topics: Vec<String>,
    pub targets: Vec<String>,
}
redacted!(Scopes);

impl Grant {
    fn entries(&self) -> (Family, &[String]) {
        let names = match self.family {
            Family::Storage => &self.scope.prefixes,
            Family::Http => &self.scope.hosts,
            Family::Event => &self.scope.topics,
            Family::Workflow => &self.scope.targets,
        };
        (self.family, names.as_ref().map_or(&[][..], |n| &n.0))
    }
}

fn entries_of(grants: &[Grant], family: Family) -> Vec<&String> {
    grants
        .iter()
        .map(Grant::entries)
        .filter(|(candidate, _)| *candidate == family)
        .flat_map(|(_, entries)| entries)
        .collect()
}

/// The entries of `family` granted by both lists, in the revision's order, without repeats.
/// Membership uses hash sets, so the work is linear in the size of the lists.
pub(crate) fn intersect(projection: &[Grant], credential: &[Grant]) -> Scopes {
    let allowed = |family| {
        let granted: HashSet<&str> = entries_of(credential, family)
            .into_iter()
            .map(String::as_str)
            .collect();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut kept: Vec<String> = Vec::new();
        for entry in entries_of(projection, family) {
            if granted.contains(entry.as_str()) && seen.insert(entry.as_str()) {
                kept.push(entry.clone());
            }
        }
        kept
    };
    Scopes {
        prefixes: allowed(Family::Storage),
        hosts: allowed(Family::Http),
        topics: allowed(Family::Event),
        targets: allowed(Family::Workflow),
    }
}

/// Every string the grants carry that a log line could repeat: scope entries, secret
/// references and guest grant identifiers.
pub(crate) fn strings(grants: &[Grant], into: &mut Vec<String>) {
    for grant in grants {
        into.extend(grant.entries().1.iter().cloned());
        if let Some(secret) = &grant.secret_ref {
            into.push(secret.name.as_str().to_owned());
            into.extend(secret.version.iter().map(|v| v.as_str().to_owned()));
        }
        into.extend(grant.guest_grant_id.iter().map(|id| id.as_str().to_owned()));
    }
}
