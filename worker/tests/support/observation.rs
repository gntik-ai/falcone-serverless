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

/// How a container instance ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Exit {
    pub reason: String,
    pub code: i32,
}

/// The runner container's state, reduced to what a qualification report needs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContainerView {
    pub waiting_reason: Option<String>,
    pub running: bool,
    pub terminated: Option<Exit>,
    /// The previous instance of a restarted container.
    pub last_exit: Option<Exit>,
}

/// Waiting reasons that no amount of polling turns into a started container.
const FATAL_WAITING: [&str; 7] = [
    "ErrImagePull",
    "ImagePullBackOff",
    "CrashLoopBackOff",
    "CreateContainerConfigError",
    "CreateContainerError",
    "InvalidImageName",
    "RunContainerError",
];

impl ContainerView {
    /// Whether the container has ever started, so there can be a log to read.
    pub fn has_output(&self) -> bool {
        self.running || self.terminated.is_some() || self.last_exit.is_some()
    }

    fn exited(&self) -> bool {
        self.terminated.is_some() || self.last_exit.is_some()
    }

    /// The container failed to start or ended in error: waiting longer will not help.
    pub fn failed(&self) -> bool {
        self.terminated.as_ref().is_some_and(|exit| exit.code != 0)
            || self
                .waiting_reason
                .as_deref()
                .is_some_and(|reason| FATAL_WAITING.contains(&reason))
    }

    pub fn describe(&self) -> Option<String> {
        let exit = |exit: &Exit| format!("{} (exit code {})", exit.reason, exit.code);
        let mut parts = Vec::new();
        if let Some(reason) = &self.waiting_reason {
            parts.push(format!("waiting: {reason}"));
        }
        if self.running {
            parts.push("running".to_owned());
        }
        if let Some(terminated) = &self.terminated {
            parts.push(format!("terminated: {}", exit(terminated)));
        }
        if let Some(last) = &self.last_exit {
            parts.push(format!("previous instance terminated: {}", exit(last)));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

fn record_lines(log: &str) -> usize {
    log.lines().filter(|line| !line.trim().is_empty()).count()
}

/// Gathers what the pod showed over time. The runner exits after its checks and may fail
/// before the pod is ever Ready, so the log and the container state are kept whatever the
/// readiness: a startup failure still reaches the report with the runner's own message.
#[derive(Debug, Default)]
pub struct Collector {
    conditions: Vec<Condition>,
    ready_seen: bool,
    log: String,
    view: ContainerView,
}

impl Collector {
    /// One look at the pod. A snapshot taken while the pod was Ready is kept for good.
    pub fn pod(&mut self, conditions: Vec<Condition>, view: ContainerView) {
        if !self.ready_seen {
            self.ready_seen = pod_ready(&conditions);
            self.conditions = conditions;
        }
        self.view = view;
    }

    pub fn container(&self) -> &ContainerView {
        &self.view
    }

    /// One log read, of the current or the previous container instance. The fullest log is
    /// kept; an empty or shorter read never replaces partial output already held.
    pub fn log(&mut self, candidate: String) {
        if record_lines(&candidate) > record_lines(&self.log) {
            self.log = candidate;
        }
    }

    /// Polling can stop: the log is complete and the pod has been Ready (or the container has
    /// exited, so it never will be), or the container failed and its log has been read.
    pub fn finished(&self, records: usize) -> bool {
        self.view.failed()
            || (record_lines(&self.log) >= records && (self.ready_seen || self.view.exited()))
    }

    pub fn into_observed(self) -> super::was01::Observed {
        super::was01::Observed {
            conditions: self.conditions,
            log: self.log,
            startup: self.view.describe(),
        }
    }
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
    /// The runner container's last known state, for the report when a check failed.
    pub startup: Option<String>,
    pub admissions: Vec<Admission>,
    pub reviews: Vec<Review>,
}
