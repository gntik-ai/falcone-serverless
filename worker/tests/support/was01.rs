//! WAS-01 ac1/ac2 on a cluster: the five phases and the qualification of what they observed.
//!
//! 1. the test-admin acquires the namespace (create mode creates it, shared mode validates it);
//! 2. the test-admin applies the bootstrap RBAC that stands in for the pregranted one;
//! 3. the test-admin requests a token for the installer ServiceAccount;
//! 4. only the installer, with that token, applies the fixture and observes it;
//! 5. the namespace guard cleans up, also on failure and panic.

use super::observation::{Admission, Condition, Observation, Review, parse_records, pod_ready};
use super::render::{self, BOOTSTRAP, FIXTURE, INSTALLER, Phase, Run};
use super::{Namespace, NamespaceApi, NamespaceMode, NamespacePlan};
use serde_json::Value;

/// A bearer token. It is never printed: `Debug` redacts it.
pub struct Token(String);

impl Token {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<REDACTED>)")
    }
}

#[derive(Debug)]
pub enum Identity<'a> {
    Admin,
    Installer(&'a Token),
}

impl Identity<'_> {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Installer(_) => "installer",
        }
    }
}

/// What the pod looked like while the runner ran.
#[derive(Debug, Default)]
pub struct Observed {
    pub conditions: Vec<Condition>,
    pub log: String,
    pub startup: Option<String>,
}

/// The cluster as the harness sees it. Namespace operations are the test-admin's.
pub trait Was01Cluster: NamespaceApi {
    fn apply(
        &self,
        identity: &Identity<'_>,
        namespace: &Namespace,
        object: &Value,
    ) -> Result<Admission, String>;
    /// TokenRequest for a ServiceAccount, made with the test-admin identity.
    fn token_request(&self, namespace: &Namespace, service_account: &str) -> Result<Token, String>;
    /// The installer's SelfSubjectAccessReview for `create` on a cluster-scoped resource.
    fn review_create(&self, token: &Token, group: &str, resource: &str) -> Result<bool, String>;
    /// The pod's Ready condition and the runner's log, read with the installer token.
    fn observe(&self, token: &Token, namespace: &Namespace, run: &str) -> Result<Observed, String>;
}

/// Applies one rendered object, but only under the identity its phase allows.
pub fn guarded_apply<C: Was01Cluster>(
    cluster: &C,
    phase: Phase,
    identity: &Identity<'_>,
    namespace: &Namespace,
    object: &Value,
) -> Result<Admission, String> {
    match (phase, identity) {
        (Phase::Bootstrap, Identity::Admin) | (Phase::Fixture, Identity::Installer(_)) => {
            cluster.apply(identity, namespace, object)
        }
        _ => Err(format!(
            "refused: {phase:?} objects cannot be applied as {}",
            identity.name()
        )),
    }
}

#[derive(Debug)]
pub struct Settings {
    pub image: String,
    pub mode: NamespaceMode,
    pub image_uid: u64,
}

#[derive(Debug)]
pub struct Evidence {
    pub uid: u64,
    pub namespace: String,
    pub observation: Observation,
}

/// Cluster-scoped creates the installer must be denied (group, resource).
pub const DENIED: [(&str, &str); 4] = [
    ("rbac.authorization.k8s.io", "clusterroles"),
    ("rbac.authorization.k8s.io", "clusterrolebindings"),
    ("node.k8s.io", "runtimeclasses"),
    ("", "namespaces"),
];

fn render_all(phase: Phase, templates: &[&str], run: &Run) -> Result<Vec<Value>, String> {
    templates
        .iter()
        .map(|template| render::render(phase, template, run))
        .collect()
}

/// Runs the five phases. Every refusal happens before the first client call; a forbidden
/// bootstrap or token request is UNVERIFIED and no fixture object has been applied.
pub fn run<C: Was01Cluster>(
    cluster: &C,
    settings: &Settings,
    entropy: impl FnMut() -> u64,
) -> Result<Evidence, String> {
    let plan = NamespacePlan::new("was01", &settings.mode)?;
    let run = Run {
        namespace: plan.namespace().name().to_owned(),
        run: plan.run().to_owned(),
        image: settings.image.clone(),
        uid: render::choose_uid(settings.image_uid, entropy),
        image_uid: settings.image_uid,
    };
    let bootstrap = render_all(Phase::Bootstrap, &BOOTSTRAP, &run)?;
    let fixture = render_all(Phase::Fixture, &FIXTURE, &run)?;

    // Phase 1. The guard cleans up when it is dropped, on every path below.
    let guard = plan.acquire(cluster)?;
    let namespace = guard.namespace().clone();
    let mut observation = Observation::default();

    // Phase 2.
    for object in &bootstrap {
        let admission = guarded_apply(
            cluster,
            Phase::Bootstrap,
            &Identity::Admin,
            &namespace,
            object,
        )
        .map_err(|error| format!("UNVERIFIED: bootstrap was not admitted: {error}"))?;
        observation.admissions.push(admission);
    }
    // Phase 3.
    let token = cluster
        .token_request(&namespace, &run.object_name(INSTALLER))
        .map_err(|error| format!("UNVERIFIED: installer token was not issued: {error}"))?;
    // Phase 4: the installer alone.
    let installer = Identity::Installer(&token);
    for object in &fixture {
        let admission = guarded_apply(cluster, Phase::Fixture, &installer, &namespace, object)
            .map_err(|error| format!("fixture was not admitted: {error}"))?;
        observation.admissions.push(admission);
    }
    for (group, resource) in DENIED {
        let allowed = cluster.review_create(&token, group, resource)?;
        observation.reviews.push(Review {
            resource: resource.to_owned(),
            allowed,
        });
    }
    let observed = cluster.observe(&token, &namespace, &run.run)?;
    observation.conditions = observed.conditions;
    observation.log = observed.log;
    observation.startup = observed.startup;

    // Phase 5, reported instead of only logged.
    guard.close()?;
    Ok(Evidence {
        uid: run.uid,
        namespace: run.namespace,
        observation,
    })
}

const CHECKS: [&str; 9] = [
    "engine_config",
    "compile_valid",
    "load_executable_memory",
    "run_valid",
    "interrupt_epoch",
    "interrupt_fuel",
    "root_fs_read_only",
    "tmp_writable",
    "non_root_uid",
];

/// WAS-01 ac1/ac2 hold only when every part of the evidence is present and good. Failures
/// are reported with the runner's recorded message.
pub fn qualify(evidence: &Evidence) -> Result<(), String> {
    let observation = &evidence.observation;
    let mut failures = Vec::new();
    if !pod_ready(&observation.conditions) {
        let ready = observation
            .conditions
            .iter()
            .find(|condition| condition.kind == "Ready")
            .map_or("absent", |condition| condition.status.as_str());
        failures.push(format!("pod Ready condition is {ready}, not True"));
    }
    match parse_records(&observation.log) {
        Err(error) => failures.push(error),
        Ok(records) => {
            for check in CHECKS {
                let mut found = records.iter().filter(|record| record.check == check);
                match (found.next(), found.next()) {
                    (None, _) => failures.push(format!("{check}: no record")),
                    (Some(_), Some(_)) => failures.push(format!("{check}: more than one record")),
                    (Some(record), None) if record.status != "passed" => failures.push(format!(
                        "{check} {}: {} ({})",
                        record.status,
                        record.reason,
                        record.error_class.as_deref().unwrap_or("no class")
                    )),
                    (Some(record), None) if check == "non_root_uid" => {
                        let effective = record
                            .reason
                            .strip_prefix("uid=")
                            .and_then(|rest| rest.split_whitespace().next())
                            .and_then(|uid| uid.parse::<u64>().ok());
                        if effective != Some(evidence.uid) {
                            failures.push(format!(
                                "non_root_uid: effective UID {effective:?} is not the chosen {}",
                                evidence.uid
                            ));
                        }
                    }
                    (Some(_), None) => {}
                }
            }
        }
    }
    let expected = BOOTSTRAP.len() + FIXTURE.len();
    if observation.admissions.len() != expected
        || observation.admissions.iter().any(|a| !a.admitted)
    {
        failures.push(format!(
            "expected {expected} admitted objects, recorded {}",
            observation.admissions.len()
        ));
    }
    for (_, resource) in DENIED {
        match observation.reviews.iter().find(|r| r.resource == resource) {
            Some(review) if !review.allowed => {}
            Some(_) => failures.push(format!("installer may create {resource}")),
            None => failures.push(format!("no access review for {resource}")),
        }
    }
    if !failures.is_empty()
        && let Some(startup) = &observation.startup
    {
        failures.push(format!("runner container {startup}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
