//! Offline evidence only; this module is not a worker runtime target.
use std::collections::{BTreeMap, BTreeSet};

pub const RESOURCES: [&str; 3] = ["deployments.apps", "services", "leases.coordination.k8s.io"];
pub const VERBS: [&str; 4] = ["get", "create", "update", "delete"];
pub const TARGETS: [&str; 3] = ["k8s-1.37", "k8s-1.36", "openshift-4.20"];
pub const FIXED: [(&str, Reason); 8] = [
    ("openshift-4.20", Reason::OpenshiftNotAvailable),
    ("pin-engine", Reason::NoPinSelected),
    ("pin-architecture", Reason::NoPinSelected),
    ("pin-wasi", Reason::NoPinSelected),
    ("pin-compiler", Reason::NoPinSelected),
    ("guest-install", Reason::NotCollected),
    ("guest-execute", Reason::NotCollected),
    ("guest-interrupt", Reason::NotCollected),
];

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    NotProbed,
    Recorded,
    Allowed,
    Denied,
    ProbeError,
    Forbidden,
    InvalidVersion,
    OpenshiftNotAvailable,
    NoPinSelected,
    NotCollected,
    PlanSafe,
    RejectedPlan,
}
impl Reason {
    pub const ALL: [Self; 12] = [
        Self::NotProbed,
        Self::Recorded,
        Self::Allowed,
        Self::Denied,
        Self::ProbeError,
        Self::Forbidden,
        Self::InvalidVersion,
        Self::OpenshiftNotAvailable,
        Self::NoPinSelected,
        Self::NotCollected,
        Self::PlanSafe,
        Self::RejectedPlan,
    ];
    pub fn code(self) -> &'static str {
        match self {
            Self::NotProbed => "not-probed",
            Self::Recorded => "recorded",
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::ProbeError => "probe-error",
            Self::Forbidden => "forbidden",
            Self::InvalidVersion => "invalid-version",
            Self::OpenshiftNotAvailable => "openshift-not-available",
            Self::NoPinSelected => "no-pin-selected",
            Self::NotCollected => "not-collected",
            Self::PlanSafe => "plan-safe",
            Self::RejectedPlan => "plan-rejected",
        }
    }
}

/// Validate the observed value without normalizing it. A target match is separate from validity.
pub fn version_target(version: &str) -> Result<Option<&'static str>, &'static str> {
    let invalid = || "invalid-version";
    if version.len() > 128 || !version.is_ascii() {
        return Err(invalid());
    }
    let version = version.strip_prefix('v').ok_or_else(invalid)?;
    let (release, build) = version
        .split_once('+')
        .map_or((version, None), |(a, b)| (a, Some(b)));
    let (core, prerelease) = release
        .split_once('-')
        .map_or((release, None), |(a, b)| (a, Some(b)));
    for suffix in [build, prerelease].into_iter().flatten() {
        if suffix.split('.').any(|part| {
            part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }) {
            return Err(invalid());
        }
    }
    let parts: Vec<_> = core.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|b| b.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return Err(invalid());
    }
    Ok(match (parts[0], parts[1]) {
        ("1", "37") => Some("k8s-1.37"),
        ("1", "36") => Some("k8s-1.36"),
        _ => None,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub verdict: Verdict,
    pub reason: Reason,
    pub evidence: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    items: BTreeMap<String, Item>,
}
impl Report {
    pub fn ac3_ids() -> Vec<String> {
        let mut ids = vec!["kubernetes-version".into()];
        for resource in RESOURCES {
            for verb in VERBS {
                ids.push(format!("can-{verb}-{resource}"));
            }
        }
        ids
    }
    pub fn new() -> Self {
        let mut items: BTreeMap<_, _> = Self::ac3_ids()
            .into_iter()
            .chain(["plan-namespaced-only".into()])
            .map(|id| {
                (
                    id,
                    Item {
                        verdict: Verdict::Unverified,
                        reason: Reason::NotProbed,
                        evidence: None,
                    },
                )
            })
            .collect();
        for (id, reason) in FIXED {
            items.insert(
                id.into(),
                Item {
                    verdict: Verdict::Unverified,
                    reason,
                    evidence: None,
                },
            );
        }
        Self { items }
    }
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.items.keys().map(String::as_str)
    }
    pub fn item(&self, id: &str) -> Result<&Item, &'static str> {
        self.items.get(id).ok_or("unknown-item")
    }
    pub fn record(
        &mut self,
        id: &str,
        verdict: Verdict,
        reason: Reason,
        evidence: Option<&str>,
    ) -> Result<(), &'static str> {
        self.item(id)?;
        if verdict == Verdict::Passed && evidence.is_none_or(str::is_empty) {
            return Err("evidence-required");
        }
        if let Some((_, fixed)) = FIXED.iter().find(|(name, _)| *name == id) {
            if verdict != Verdict::Unverified || reason != *fixed || evidence.is_some() {
                return Err("fixed-item");
            }
        } else if verdict == Verdict::Passed {
            let valid = match id {
                "kubernetes-version" => {
                    version_target(evidence.unwrap_or_default())?;
                    reason == Reason::Recorded
                }
                "plan-namespaced-only" => {
                    reason == Reason::PlanSafe && evidence == Some("allow-listed-plan")
                }
                _ => reason == Reason::Allowed && evidence == Some("yes"),
            };
            if !valid {
                return Err("invalid-evidence");
            }
        } else {
            let valid = match (verdict, reason) {
                (Verdict::Unverified, Reason::NotProbed) => true,
                (Verdict::Failed, Reason::RejectedPlan) => id == "plan-namespaced-only",
                (Verdict::Unverified, Reason::Denied) => id.starts_with("can-"),
                (Verdict::Unverified | Verdict::Failed, Reason::ProbeError | Reason::Forbidden) => {
                    id != "plan-namespaced-only"
                }
                (Verdict::Unverified | Verdict::Failed, Reason::InvalidVersion) => {
                    id == "kubernetes-version"
                }
                _ => false,
            };
            if evidence.is_some() || !valid {
                return Err("invalid-evidence");
            }
        }
        self.items.insert(
            id.into(),
            Item {
                verdict,
                reason,
                evidence: evidence.map(str::to_owned),
            },
        );
        Ok(())
    }
    pub fn cell(&self, target: &str) -> Result<Verdict, &'static str> {
        if !TARGETS.contains(&target) {
            return Err("unknown-target");
        }
        let version = &self.item("kubernetes-version")?.evidence;
        let verdicts: Vec<_> = Self::ac3_ids()
            .iter()
            .map(|id| self.items[id].verdict)
            .collect();
        if target != "openshift-4.20" && version.is_none() && verdicts.contains(&Verdict::Failed) {
            return Ok(Verdict::Failed);
        }
        if version
            .as_deref()
            .and_then(|v| version_target(v).ok().flatten())
            != Some(target)
        {
            return Ok(Verdict::Unverified);
        }
        Ok(if verdicts.contains(&Verdict::Failed) {
            Verdict::Failed
        } else if verdicts.iter().all(|v| *v == Verdict::Passed) {
            Verdict::Passed
        } else {
            Verdict::Unverified
        })
    }
    pub fn summary(&self) -> String {
        let mut missing: Vec<_> = self
            .items
            .iter()
            .filter(|(_, item)| item.verdict != Verdict::Passed)
            .map(|(id, _)| id.as_str())
            .collect();
        for target in TARGETS {
            if self.cell(target) != Ok(Verdict::Passed) && !missing.contains(&target) {
                missing.push(target);
            }
        }
        format!("never-go\t{}", missing.join(","))
    }
    pub fn render(&self) -> String {
        let mut text = String::from("was01-report-v1\n");
        for (id, item) in &self.items {
            text.push_str(&format!(
                "{id}\t{}\t{}\t{}\n",
                item.verdict.code(),
                item.reason.code(),
                item.evidence.as_deref().unwrap_or("-")
            ));
        }
        for target in TARGETS {
            text.push_str(&format!(
                "cell\t{target}\t{}\n",
                self.cell(target).unwrap_or(Verdict::Unverified).code()
            ));
        }
        text.push_str(&format!("summary\t{}\n", self.summary()));
        text
    }
    /// Strict canonical format: fixed inventory, no duplicates, unknown lines, or arbitrary text.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let mut report = Self::new();
        let mut seen = BTreeSet::new();
        let mut lines = text.lines();
        if lines.next() != Some("was01-report-v1") {
            return Err("malformed-report");
        }
        for line in lines {
            let parts: Vec<_> = line.split('\t').collect();
            if parts
                .first()
                .is_some_and(|id| *id == "cell" || *id == "summary")
            {
                continue;
            }
            if parts.len() != 4 || !seen.insert(parts[0]) {
                return Err("malformed-report");
            }
            let verdict = [Verdict::Passed, Verdict::Failed, Verdict::Unverified]
                .into_iter()
                .find(|v| v.code() == parts[1])
                .ok_or("malformed-report")?;
            let reason = Reason::ALL
                .into_iter()
                .find(|r| r.code() == parts[2])
                .ok_or("malformed-report")?;
            report.record(
                parts[0],
                verdict,
                reason,
                (parts[3] != "-").then_some(parts[3]),
            )?;
        }
        if seen.len() != report.items.len() || report.render() != text {
            return Err("malformed-report");
        }
        Ok(report)
    }
}
