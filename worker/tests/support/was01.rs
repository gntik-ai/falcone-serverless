//! Five-phase WAS-01 qualification; fixture creation is installer-only.
use super::{
    namespace::{Namespace, NamespaceMode},
    observation::Observation,
    test_namespace::{NamespaceApi, NamespaceGuard, test_namespace},
};
use serde_json::Value;

pub const IMAGE_UID: u32 = 65532;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    Admin,
    Installer,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Bootstrap,
    Fixture,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Config {
    Disabled,
    Enabled { image: String, mode: NamespaceMode },
}

impl Config {
    pub fn parse(
        cluster: Option<&str>,
        kubeconfig: Option<&str>,
        image: Option<&str>,
        mode: Option<&str>,
        namespace: Option<&str>,
    ) -> Result<Self, String> {
        if cluster != Some("1") {
            return Ok(Self::Disabled);
        }
        if kubeconfig.is_none_or(|path| path.trim().is_empty()) {
            return Err("UNVERIFIED: KUBECONFIG required for WAS-01".into());
        }
        let image = image
            .filter(|image| pinned_image(image))
            .ok_or("UNVERIFIED: digest-pinned FS_TEST_WAS01_IMAGE required")?;
        let mode = NamespaceMode::parse(mode, namespace)
            .map_err(|error| format!("UNVERIFIED: {error}"))?;
        Ok(Self::Enabled {
            image: image.into(),
            mode,
        })
    }
}

/// Only the real Kubernetes boundary handles TokenRequest credentials; none leave this API.
pub trait ClusterApi: NamespaceApi {
    fn create(&self, identity: Identity, object: &Value) -> Result<Value, String>;
    fn installer_token(&self, namespace: &Namespace, account: &str) -> Result<(), String>;
    fn installer_can_create(&self, group: &str, resource: &str) -> Result<bool, String>;
    fn begin_observation(&self, namespace: &Namespace, run: &str) -> Result<String, String>;
    fn observe(
        &self,
        namespace: &Namespace,
        run: &str,
        version: &str,
    ) -> Result<Observation, String>;
}

pub struct Report {
    pub admissions: Vec<Value>,
    pub observation: Observation,
    pub denied: Vec<String>,
}

pub fn apply_checked<K: ClusterApi>(
    kube: &K,
    guard: &mut NamespaceGuard<'_, K>,
    plan: &Plan,
    phase: Phase,
    identity: Identity,
    object: &Value,
) -> Result<Value, String> {
    plan.validate(phase, identity, object)?;
    guard.track(object)?;
    let admitted = kube.create(identity, object)?;
    // Persist each response even if a later phase fails. TokenRequest never reaches this path.
    println!("{}", serde_json::json!({"admission_response": admitted}));
    let violations =
        falcone_serverless_worker::check(&admitted, falcone_serverless_worker::Role::GuestWorker);
    if !violations.is_empty() || admitted["metadata"]["labels"]["falcone.test/run"] != plan.run {
        return Err("server admission changed restricted posture or run ownership".into());
    }
    Ok(admitted)
}

pub fn run<K: ClusterApi>(kube: &K, image: &str, mode: &NamespaceMode) -> Result<Report, String> {
    if !pinned_image(image) {
        return Err("UNVERIFIED: digest-pinned FS_TEST_WAS01_IMAGE required".into());
    }
    // Phase 1: the namespace lease is acquired only after the authorization/read-only preflight.
    let mut guard = test_namespace(kube, "was01", mode)?;
    let plan = Plan::new(guard.namespace.name(), &guard.run, choose_uid(), image)?;
    let mut admissions = Vec::new();
    // Phase 2: bootstrap objects are the only objects created with the admin identity.
    for object in plan.bootstrap() {
        admissions.push(
            apply_checked(
                kube,
                &mut guard,
                &plan,
                Phase::Bootstrap,
                Identity::Admin,
                &object,
            )
            .map_err(|error| format!("UNVERIFIED: bootstrap: {error}"))?,
        );
    }
    // Phase 3: short-lived installer credentials stay entirely inside the client boundary.
    kube.installer_token(&plan.namespace, &plan.name("was01-installer"))
        .map_err(|error| format!("UNVERIFIED: TokenRequest: {error}"))?;
    let mut denied = Vec::new();
    for (group, resource) in [
        ("rbac.authorization.k8s.io", "clusterroles"),
        ("rbac.authorization.k8s.io", "clusterrolebindings"),
        ("node.k8s.io", "runtimeclasses"),
        ("", "namespaces"),
    ] {
        if kube.installer_can_create(group, resource)? {
            return Err(format!(
                "UNVERIFIED: installer unexpectedly allowed create {resource}"
            ));
        }
        denied.push(resource.into());
    }
    // Start from an API resourceVersion obtained before fixture creation. A late GET loses Ready.
    let version = kube.begin_observation(&plan.namespace, &plan.run)?;
    // Phase 4: never use admin to create the fixture, including after an admission failure.
    for object in plan.fixture() {
        admissions.push(apply_checked(
            kube,
            &mut guard,
            &plan,
            Phase::Fixture,
            Identity::Installer,
            &object,
        )?);
    }
    let observation = kube.observe(&plan.namespace, &plan.run, &version)?;
    println!(
        "{}",
        serde_json::json!({"ready_conditions": observation.conditions, "runner": observation.logs, "exit_code": observation.exit_code})
    );
    qualify(&observation, plan.uid)?;
    // Phase 5: explicit cleanup reports failures; Drop also covers early returns and panic.
    guard.close()?;
    Ok(Report {
        admissions,
        observation,
        denied,
    })
}

pub fn choose_uid() -> u32 {
    loop {
        let uid = rand::random_range(10000..2_000_000_000);
        if uid != IMAGE_UID {
            return uid;
        }
    }
}

pub struct Plan {
    pub namespace: Namespace,
    pub run: String,
    pub uid: u32,
    image: String,
}

impl Plan {
    pub fn new(namespace: &str, run: &str, uid: u32, image: &str) -> Result<Self, String> {
        Namespace::new(&format!("fs-test-{run}"))?;
        if uid == 0 || uid == IMAGE_UID || !pinned_image(image) {
            return Err(
                "UNVERIFIED: arbitrary non-root UID and digest-pinned FS_TEST_WAS01_IMAGE required"
                    .into(),
            );
        }
        Ok(Self {
            namespace: Namespace::new(namespace)?,
            run: run.into(),
            uid,
            image: image.into(),
        })
    }

    pub fn name(&self, prefix: &str) -> String {
        format!("{prefix}-{}", self.run)
    }

    fn object(&self, source: &str) -> Value {
        let mut object: Value = serde_json::from_str(source).expect("committed qualification JSON");
        let name = object["metadata"]["name"].as_str().unwrap().to_owned();
        object["metadata"]["name"] = self.name(&name).into();
        object["metadata"]["namespace"] = self.namespace.name().into();
        object["metadata"]["labels"] = serde_json::json!({"falcone.test/run": self.run});
        object
    }

    pub fn bootstrap(&self) -> Vec<Value> {
        let mut objects = vec![
            self.object(include_str!(
                "../../qualification/manifests/bootstrap/serviceaccount.json"
            )),
            self.object(include_str!(
                "../../qualification/manifests/bootstrap/role.json"
            )),
            self.object(include_str!(
                "../../qualification/manifests/bootstrap/rolebinding.json"
            )),
        ];
        let binding = &mut objects[2];
        binding["roleRef"]["name"] = self.name("was01-installer").into();
        binding["subjects"][0]["name"] = self.name("was01-installer").into();
        binding["subjects"][0]["namespace"] = self.namespace.name().into();
        objects
    }

    pub fn fixture(&self) -> Vec<Value> {
        let account = self.object(include_str!(
            "../../qualification/manifests/fixture/serviceaccount.json"
        ));
        let mut deployment = self.object(include_str!(
            "../../qualification/manifests/fixture/deployment.json"
        ));
        deployment["spec"]["selector"]["matchLabels"]["falcone.test/run"] = self.run.clone().into();
        deployment["spec"]["template"]["metadata"]["labels"]["falcone.test/run"] =
            self.run.clone().into();
        let pod = &mut deployment["spec"]["template"]["spec"];
        pod["serviceAccountName"] = self.name("was01-workload").into();
        pod["securityContext"]["runAsUser"] = self.uid.into();
        pod["containers"][0]["image"] = self.image.clone().into();
        vec![account, deployment]
    }

    pub fn validate(&self, phase: Phase, identity: Identity, object: &Value) -> Result<(), String> {
        if (phase == Phase::Bootstrap && identity != Identity::Admin)
            || (phase == Phase::Fixture && identity != Identity::Installer)
        {
            return Err("refused wrong phase identity".into());
        }
        let violations =
            falcone_serverless_worker::check(object, falcone_serverless_worker::Role::GuestWorker);
        if !violations.is_empty() {
            return Err(format!("refused restricted posture: {violations:?}"));
        }
        let allowed = match phase {
            Phase::Bootstrap => self.bootstrap(),
            Phase::Fixture => self.fixture(),
        };
        if !allowed.contains(object) {
            return Err("refused object outside exact namespaced qualification plan".into());
        }
        Ok(())
    }
}

pub fn pinned_image(image: &str) -> bool {
    image.split_once("@sha256:").is_some_and(|(repo, hex)| {
        !repo.is_empty()
            && !repo.contains('@')
            && !repo.chars().any(char::is_whitespace)
            && hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub fn qualify(observation: &super::observation::Observation, uid: u32) -> Result<(), String> {
    if !observation.conditions.iter().any(|condition| {
        condition.type_ == "Ready"
            && condition.status == "True"
            && condition.last_transition_time.is_some()
    }) {
        return Err("fixture Ready=True with lastTransitionTime was not observed".into());
    }
    let mut records = std::collections::BTreeMap::new();
    for line in observation.logs.lines() {
        let record: Value =
            serde_json::from_str(line).map_err(|_| "malformed qualification record")?;
        let check = record["check"]
            .as_str()
            .ok_or("qualification check missing")?
            .to_owned();
        if record["status"] != "passed" || !record["error_class"].is_null() {
            return Err(format!(
                "qualification {check} failed: {}",
                record["reason"]
            ));
        }
        if records.insert(check, record).is_some() {
            return Err("duplicate qualification check".into());
        }
    }
    for (check, reason) in [
        ("engine_config", "engine succeeded"),
        ("compile_valid", "compile succeeded"),
        ("load_executable_memory", "load succeeded"),
        ("run_valid", "run returned 42"),
        ("interrupt_epoch", "observed epoch interruption"),
        ("interrupt_fuel", "observed fuel exhaustion"),
        (
            "root_fs_read_only",
            "root mount is ro and write probe failed",
        ),
        ("tmp_writable", "/tmp random payload readback verified"),
    ] {
        if records
            .get(check)
            .is_none_or(|record| record["reason"] != reason)
        {
            return Err(format!(
                "missing verified {check} evidence (including readback for tmp_writable)"
            ));
        }
    }
    if records.len() != 9
        || records
            .get("non_root_uid")
            .and_then(|record| record["reason"].as_str())
            .is_none_or(|reason| !reason.starts_with(&format!("uid={uid} ")))
    {
        return Err("effective UID does not match the chosen arbitrary UID".into());
    }
    if observation.exit_code != Some(0) {
        return Err("successful runner exit was not observed".into());
    }
    Ok(())
}
