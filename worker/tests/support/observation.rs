//! What a WAS-01 run observed: the runner's JSON-lines records, the server's admission
//! responses, the installer's access reviews and the pod's conditions.

use serde_json::Value;

/// One line of the qualification runner's output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    pub check: String,
    pub status: String,
    pub reason: String,
    pub error_class: Option<String>,
}

/// The runner prints one JSON object per line; anything else is a malformed log.
pub fn parse_records(log: &str) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)
            .map_err(|_| "runner log has a line that is not JSON".to_string())?;
        let text = |key: &str| value[key].as_str().map(str::to_owned);
        records.push(Record {
            check: text("check").ok_or("runner record has no check")?,
            status: text("status").ok_or("runner record has no status")?,
            reason: text("reason").unwrap_or_default(),
            error_class: text("error_class"),
        });
    }
    Ok(records)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Condition {
    pub kind: String,
    pub status: String,
}

/// A pod is Ready only when its `Ready` condition is present and `True`; absent, `False`
/// and `Unknown` all fail.
pub fn pod_ready(conditions: &[Condition]) -> bool {
    conditions
        .iter()
        .any(|condition| condition.kind == "Ready" && condition.status == "True")
}

/// The server's answer to one apply, recorded per identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Admission {
    pub identity: &'static str,
    pub kind: String,
    pub name: String,
    pub admitted: bool,
    pub message: String,
}

/// The installer's own SelfSubjectAccessReview for one cluster-scoped verb.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Review {
    pub resource: String,
    pub allowed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Observation {
    pub conditions: Vec<Condition>,
    pub log: String,
    pub admissions: Vec<Admission>,
    pub reviews: Vec<Review>,
}
