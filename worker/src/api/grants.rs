//! Capability grants. The request's projection carries the revision-runtime-manifest grants
//! (`@0.2.0`); the credential's `allowed_capabilities` carry capability-grants v1 (`@0.1.0`).
//! One closed shape serves both; the version selects the only interface string it may name.

use super::types::NonEmpty;
use super::wire::{present, redacted, unit_debug};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

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

/// The concrete scopes a revision's grants give the capability host.
pub(crate) fn to_host(grants: &[Grant]) -> crate::host::Grants {
    let mut host = crate::host::Grants::default();
    let names = |names: &Option<Names>| names.as_ref().map(|n| n.0.clone()).unwrap_or_default();
    for grant in grants {
        match grant.family {
            Family::Storage => host.prefixes.extend(names(&grant.scope.prefixes)),
            Family::Http => host.hosts.extend(names(&grant.scope.hosts)),
            Family::Event => host.topics.extend(names(&grant.scope.topics)),
            Family::Workflow => host.targets.extend(names(&grant.scope.targets)),
        }
    }
    host
}

impl Grant {
    /// Every string of the grant that identifies a protected resource.
    pub(crate) fn push_strings(&self, out: &mut Vec<String>) {
        for scope in [
            &self.scope.prefixes,
            &self.scope.hosts,
            &self.scope.topics,
            &self.scope.targets,
        ]
        .into_iter()
        .flatten()
        {
            out.extend(scope.0.iter().cloned());
        }
        if let Some(reference) = &self.secret_ref {
            out.push(reference.name.as_str().to_owned());
            if let Some(version) = &reference.version {
                out.push(version.as_str().to_owned());
            }
        }
        if let Some(id) = &self.guest_grant_id {
            out.push(id.as_str().to_owned());
        }
    }
}

/// The scopes both lists grant: a host capability needs the revision's grant and the
/// credential's. The credential never widens the revision, nor the revision the credential.
pub(crate) fn intersect(
    revision: &crate::host::Grants,
    credential: &crate::host::Grants,
) -> crate::host::Grants {
    // Linear in both lists: a request can carry hundreds of thousands of names.
    let both = |a: &[String], b: &[String]| -> Vec<String> {
        let allowed: std::collections::HashSet<&String> = b.iter().collect();
        a.iter()
            .filter(|name| allowed.contains(name))
            .cloned()
            .collect()
    };
    crate::host::Grants {
        prefixes: both(&revision.prefixes, &credential.prefixes),
        hosts: both(&revision.hosts, &credential.hosts),
        topics: both(&revision.topics, &credential.topics),
        targets: both(&revision.targets, &credential.targets),
    }
}

/// Whether every grant names the interface of `version` and the scope of its family.
pub(crate) fn all_valid(grants: &[Grant], version: GrantVersion) -> bool {
    grants.iter().all(|grant| grant.valid(version))
}
