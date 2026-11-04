//! WAS-01 cluster evidence model (ac1 `plan-namespaced-only`, ac3 version and namespaced
//! permissions). It imports only `std`, keeps no credential and holds no cluster client.
//!
//! Every item starts unverified. An item can become `Passed` only with evidence in the exact
//! shape its class requires; the openshift cell, the four pins and the guest items cannot
//! change at all. Reasons are fixed codes, never text taken from a cluster. A report has no
//! "go" verdict: the story summary is always `not-go` and lists every item that has not passed.

use std::fmt;

/// First line of every report.
pub const HEADER: &str = "was01-cluster-evidence 1";
pub const VERSION_ITEM: &str = "kubernetes-version";
pub const PLAN_ITEM: &str = "plan-namespaced-only";
/// Verbs and resources of the ac3 namespaced permission items (AGENTS.md: the controller's write set).
pub const VERBS: [&str; 4] = ["get", "create", "update", "delete"];
pub const RESOURCES: [&str; 3] = ["deployments.apps", "services", "leases.coordination.k8s.io"];

/// Every item, in report order.
pub const ITEM_IDS: [&str; 22] = [
    "kubernetes-version",
    "can-i-get-deployments.apps",
    "can-i-create-deployments.apps",
    "can-i-update-deployments.apps",
    "can-i-delete-deployments.apps",
    "can-i-get-services",
    "can-i-create-services",
    "can-i-update-services",
    "can-i-delete-services",
    "can-i-get-leases.coordination.k8s.io",
    "can-i-create-leases.coordination.k8s.io",
    "can-i-update-leases.coordination.k8s.io",
    "can-i-delete-leases.coordination.k8s.io",
    "plan-namespaced-only",
    "openshift-4.20",
    "pin-engine",
    "pin-architecture",
    "pin-wasi",
    "pin-compiler",
    "guest-install",
    "guest-execute",
    "guest-interrupt",
];

/// The item id of one namespaced permission probe, or `None` outside the listed verbs and resources.
pub fn can_i_item(verb: &str, resource: &str) -> Option<&'static str> {
    ITEM_IDS
        .iter()
        .copied()
        .find(|id| id.strip_prefix("can-i-") == Some(&format!("{verb}-{resource}")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Failed,
    Unverified,
}

impl Verdict {
    pub fn code(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unverified => "unverified",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Passed, Self::Failed, Self::Unverified]
            .into_iter()
            .find(|verdict| verdict.code() == text)
    }
}

/// The fixed reason codes. Item reasons and cell reasons share one list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    NotProbed,
    NotCollected,
    NoPinSelected,
    OpenshiftNotAvailable,
    MissingVersion,
    MalformedVersion,
    UnsupportedVersion,
    Forbidden,
    Denied,
    ProbeError,
    CheckFailed,
    PlanViolation,
    ItemFailed,
    ItemUnverified,
    NoMatchingServer,
}

impl Reason {
    pub const ALL: [Reason; 15] = [
        Self::NotProbed,
        Self::NotCollected,
        Self::NoPinSelected,
        Self::OpenshiftNotAvailable,
        Self::MissingVersion,
        Self::MalformedVersion,
        Self::UnsupportedVersion,
        Self::Forbidden,
        Self::Denied,
        Self::ProbeError,
        Self::CheckFailed,
        Self::PlanViolation,
        Self::ItemFailed,
        Self::ItemUnverified,
        Self::NoMatchingServer,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::NotProbed => "not-probed",
            Self::NotCollected => "not-collected",
            Self::NoPinSelected => "no-pin-selected",
            Self::OpenshiftNotAvailable => "openshift-not-available",
            Self::MissingVersion => "missing-version",
            Self::MalformedVersion => "malformed-version",
            Self::UnsupportedVersion => "unsupported-version",
            Self::Forbidden => "forbidden",
            Self::Denied => "denied",
            Self::ProbeError => "probe-error",
            Self::CheckFailed => "check-failed",
            Self::PlanViolation => "plan-violation",
            Self::ItemFailed => "item-failed",
            Self::ItemUnverified => "item-unverified",
            Self::NoMatchingServer => "no-matching-server",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.code() == text)
    }
}

/// The two Kubernetes lines this task records. OpenShift 4.20 is an item that never passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    K8s137,
    K8s136,
}

impl Target {
    pub const ALL: [Target; 2] = [Self::K8s137, Self::K8s136];

    pub fn code(self) -> &'static str {
        match self {
            Self::K8s137 => "k8s-1.37",
            Self::K8s136 => "k8s-1.36",
        }
    }
}

/// A server `gitVersion` split into the parts that choose a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitVersion {
    major: u32,
    minor: u32,
    prerelease: bool,
}

impl GitVersion {
    /// `v<major>.<minor>.<patch>[-<prerelease>][+<build>]`, at most 64 characters of
    /// `[0-9A-Za-z.+-]`. Anything else is malformed, so no cluster text is recorded unchecked.
    pub fn parse(text: &str) -> Result<Self, Reason> {
        let malformed = Reason::MalformedVersion;
        if text.len() > 64 || !text.bytes().all(is_label_byte) {
            return Err(malformed);
        }
        let rest = text.strip_prefix('v').ok_or(malformed)?;
        let (core_and_pre, build) = match rest.split_once('+') {
            Some((head, build)) => (head, Some(build)),
            None => (rest, None),
        };
        let (core, prerelease) = match core_and_pre.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (core_and_pre, None),
        };
        let numbers: Vec<u32> = core
            .split('.')
            .map(|part| {
                let plain = !part.is_empty()
                    && part.len() <= 6
                    && part.bytes().all(|b| b.is_ascii_digit())
                    && (part == "0" || !part.starts_with('0'));
                if plain {
                    part.parse().map_err(|_| malformed)
                } else {
                    Err(malformed)
                }
            })
            .collect::<Result<_, _>>()?;
        let [major, minor, _patch] = numbers[..] else {
            return Err(malformed);
        };
        for tail in [prerelease, build].into_iter().flatten() {
            let identifiers_ok = tail.split('.').all(|identifier| {
                !identifier.is_empty()
                    && identifier
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
            if !identifiers_ok {
                return Err(malformed);
            }
        }
        Ok(Self {
            major,
            minor,
            prerelease: prerelease.is_some(),
        })
    }

    /// The cell this version fills: 1.37.x or 1.36.x releases only. Pre-releases, other lines
    /// and other majors fill none.
    pub fn target(self) -> Option<Target> {
        match (self.major, self.minor, self.prerelease) {
            (1, 37, false) => Some(Target::K8s137),
            (1, 36, false) => Some(Target::K8s136),
            _ => None,
        }
    }
}

fn is_label_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Version,
    CanI,
    Plan,
    /// Never changes; the reason is fixed.
    Fixed(Reason),
}

fn class_of(id: &str) -> Option<Class> {
    ITEM_IDS.contains(&id).then(|| match id {
        VERSION_ITEM => Class::Version,
        PLAN_ITEM => Class::Plan,
        "openshift-4.20" => Class::Fixed(Reason::OpenshiftNotAvailable),
        id if id.starts_with("pin-") => Class::Fixed(Reason::NoPinSelected),
        id if id.starts_with("guest-") => Class::Fixed(Reason::NotCollected),
        _ => Class::CanI,
    })
}

fn unverified_reasons(class: Class) -> &'static [Reason] {
    match class {
        Class::Version => &[
            Reason::NotProbed,
            Reason::MissingVersion,
            Reason::MalformedVersion,
            Reason::UnsupportedVersion,
            Reason::Forbidden,
            Reason::ProbeError,
        ],
        Class::CanI => &[
            Reason::NotProbed,
            Reason::Denied,
            Reason::Forbidden,
            Reason::ProbeError,
        ],
        Class::Plan => &[Reason::NotProbed],
        Class::Fixed(_) => &[],
    }
}

fn failed_reasons(class: Class) -> &'static [Reason] {
    match class {
        Class::Version | Class::CanI => &[Reason::CheckFailed],
        Class::Plan => &[Reason::CheckFailed, Reason::PlanViolation],
        Class::Fixed(_) => &[],
    }
}

fn evidence_valid(class: Class, evidence: &str) -> bool {
    match class {
        Class::Version => GitVersion::parse(evidence)
            .ok()
            .and_then(GitVersion::target)
            .is_some(),
        Class::CanI => evidence == "yes",
        Class::Plan => evidence
            .strip_prefix("calls=")
            .is_some_and(|n| (1..=3).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())),
        Class::Fixed(_) => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelError {
    UnknownItem,
    /// The item cannot change: openshift, the pins and the guest items stay unverified.
    FixedItem,
    /// A passed item needs evidence in its class's exact shape.
    InvalidEvidence,
    InvalidReason,
    Malformed,
    /// A derived line (cell, summary) does not match the items.
    Inconsistent,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnknownItem => "unknown WAS-01 evidence item",
            Self::FixedItem => "WAS-01 evidence item cannot change",
            Self::InvalidEvidence => "passed requires valid evidence",
            Self::InvalidReason => "reason code is not allowed for this item",
            Self::Malformed => "malformed WAS-01 evidence report",
            Self::Inconsistent => "derived report lines do not match the items",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub verdict: Verdict,
    pub reason: Option<Reason>,
    pub evidence: Option<String>,
}

fn initial(class: Class) -> Item {
    Item {
        verdict: Verdict::Unverified,
        reason: Some(match class {
            Class::Fixed(reason) => reason,
            _ => Reason::NotProbed,
        }),
        evidence: None,
    }
}

fn item_valid(class: Class, item: &Item) -> bool {
    if let Class::Fixed(_) = class {
        return *item == initial(class);
    }
    match (item.verdict, item.reason, item.evidence.as_deref()) {
        (Verdict::Passed, None, Some(evidence)) => evidence_valid(class, evidence),
        (Verdict::Unverified, Some(reason), None) => unverified_reasons(class).contains(&reason),
        (Verdict::Failed, Some(reason), None) => failed_reasons(class).contains(&reason),
        _ => false,
    }
}

/// The derived status of one target cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub verdict: Verdict,
    pub reason: Option<Reason>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    items: Vec<Item>,
}

impl Default for Report {
    fn default() -> Self {
        Self::new()
    }
}

impl Report {
    /// Every item unverified: ac3 and the plan `not-probed`, the rest with their fixed codes.
    pub fn new() -> Self {
        Self {
            items: ITEM_IDS
                .iter()
                .filter_map(|id| class_of(id))
                .map(initial)
                .collect(),
        }
    }

    fn index(id: &str) -> Result<usize, ModelError> {
        ITEM_IDS
            .iter()
            .position(|known| *known == id)
            .ok_or(ModelError::UnknownItem)
    }

    fn set(&mut self, id: &str, item: Item, reject: ModelError) -> Result<(), ModelError> {
        let index = Self::index(id)?;
        let class = class_of(id).ok_or(ModelError::UnknownItem)?;
        if let Class::Fixed(_) = class {
            return Err(ModelError::FixedItem);
        }
        if !item_valid(class, &item) {
            return Err(reject);
        }
        self.items[index] = item;
        Ok(())
    }

    /// Passed needs evidence: the verbatim gitVersion of a 1.37 or 1.36 server, `yes`, or `calls=<n>`.
    pub fn set_passed(&mut self, id: &str, evidence: &str) -> Result<(), ModelError> {
        let item = Item {
            verdict: Verdict::Passed,
            reason: None,
            evidence: Some(evidence.to_owned()),
        };
        self.set(id, item, ModelError::InvalidEvidence)
    }

    pub fn set_unverified(&mut self, id: &str, reason: Reason) -> Result<(), ModelError> {
        let item = Item {
            verdict: Verdict::Unverified,
            reason: Some(reason),
            evidence: None,
        };
        self.set(id, item, ModelError::InvalidReason)
    }

    pub fn set_failed(&mut self, id: &str, reason: Reason) -> Result<(), ModelError> {
        let item = Item {
            verdict: Verdict::Failed,
            reason: Some(reason),
            evidence: None,
        };
        self.set(id, item, ModelError::InvalidReason)
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        Self::index(id).ok().map(|index| &self.items[index])
    }

    /// The cell the recorded server version fills, if the version item passed.
    pub fn target(&self) -> Option<Target> {
        let item = self.item(VERSION_ITEM)?;
        match (item.verdict, item.evidence.as_deref()) {
            (Verdict::Passed, Some(version)) => GitVersion::parse(version).ok()?.target(),
            _ => None,
        }
    }

    /// Failed if any ac3 item failed, passed only if all passed, otherwise unverified. Only
    /// the cell the server version filled is derived from items; the other cell has no server.
    pub fn cell(&self, target: Target) -> Cell {
        if self.target() != Some(target) {
            return Cell {
                verdict: Verdict::Unverified,
                reason: Some(Reason::NoMatchingServer),
            };
        }
        let ac3 = || {
            ITEM_IDS
                .iter()
                .zip(&self.items)
                .filter(|(id, _)| matches!(class_of(id), Some(Class::Version | Class::CanI)))
                .map(|(_, item)| item.verdict)
        };
        if ac3().any(|verdict| verdict == Verdict::Failed) {
            Cell {
                verdict: Verdict::Failed,
                reason: Some(Reason::ItemFailed),
            }
        } else if ac3().all(|verdict| verdict == Verdict::Passed) {
            Cell {
                verdict: Verdict::Passed,
                reason: None,
            }
        } else {
            Cell {
                verdict: Verdict::Unverified,
                reason: Some(Reason::ItemUnverified),
            }
        }
    }

    /// Every item that has not passed, in report order. The story summary lists exactly these.
    pub fn non_passed(&self) -> Vec<&'static str> {
        ITEM_IDS
            .iter()
            .zip(&self.items)
            .filter(|(_, item)| item.verdict != Verdict::Passed)
            .map(|(id, _)| *id)
            .collect()
    }

    fn derived_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = Target::ALL
            .iter()
            .map(|target| {
                let cell = self.cell(*target);
                format!(
                    "cell {} {} {}",
                    target.code(),
                    cell.verdict.code(),
                    cell.reason.map_or("-", Reason::code)
                )
            })
            .collect();
        let listed = self.non_passed().join(",");
        lines.push(format!(
            "summary not-go non-passed={}",
            if listed.is_empty() { "-" } else { &listed }
        ));
        lines
    }

    pub fn to_text(&self) -> String {
        let mut lines = vec![HEADER.to_owned()];
        for (id, item) in ITEM_IDS.iter().zip(&self.items) {
            lines.push(format!(
                "item {id} {} {} {}",
                item.verdict.code(),
                item.reason.map_or("-", Reason::code),
                item.evidence.as_deref().unwrap_or("-")
            ));
        }
        lines.extend(self.derived_lines());
        lines.join("\n") + "\n"
    }

    /// Reads a report back. The layout is exact: header, every item once in order, then the
    /// derived lines, which must equal what the items imply. Anything else is rejected.
    pub fn parse(text: &str) -> Result<Self, ModelError> {
        let body = text.strip_suffix('\n').ok_or(ModelError::Malformed)?;
        let mut lines = body.split('\n');
        if lines.next() != Some(HEADER) {
            return Err(ModelError::Malformed);
        }
        let mut report = Self::new();
        for (position, expected) in ITEM_IDS.iter().enumerate() {
            let line = lines.next().ok_or(ModelError::Malformed)?;
            let fields: Vec<&str> = line.split(' ').collect();
            let [tag, id, verdict, reason, evidence] = fields[..] else {
                return Err(ModelError::Malformed);
            };
            if tag != "item" {
                return Err(ModelError::Malformed);
            }
            let class = class_of(id).ok_or(ModelError::UnknownItem)?;
            if id != *expected {
                return Err(ModelError::Malformed);
            }
            let item = Item {
                verdict: Verdict::parse(verdict).ok_or(ModelError::Malformed)?,
                reason: match reason {
                    "-" => None,
                    code => Some(Reason::parse(code).ok_or(ModelError::Malformed)?),
                },
                evidence: (evidence != "-").then(|| evidence.to_owned()),
            };
            if !item_valid(class, &item) {
                return Err(ModelError::Malformed);
            }
            report.items[position] = item;
        }
        let rest: Vec<&str> = lines.collect();
        if rest != report.derived_lines() {
            return Err(ModelError::Inconsistent);
        }
        Ok(report)
    }
}
