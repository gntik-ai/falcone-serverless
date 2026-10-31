//! Five-phase WAS-01 fixture. Credentials stay behind the Kubernetes API boundary.
use super::{Namespace, NamespaceClient, pinned_image};
use falcone_serverless_worker::{Role, check};
use serde_json::{Value, json};

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

pub trait QualificationClient: NamespaceClient {
    /// POST only: collisions must fail, never update/adopt an existing object.
    fn create(
        &self,
        identity: Identity,
        namespace: &Namespace,
        object: &Value,
    ) -> Result<Value, String>;
    /// Request an ephemeral installer token; never return it to the caller.
    fn token_request(&self, namespace: &Namespace, serviceaccount: &str) -> Result<(), String>;
    fn installer_can_create(&self, group: &str, resource: &str) -> Result<bool, String>;
    /// First-run, exit-zero logs only. Restarts/nonzero exits/timeouts are errors.
    fn observe(&self, plan: &Plan) -> Result<String, String>;
}

pub struct Plan {
    pub namespace: Namespace,
    pub run: String,
    pub uid: u32,
    bootstrap: Vec<Value>,
    fixture: Vec<Value>,
}

impl Plan {
    pub fn new(namespace: Namespace, run: &str, image: &str, uid: u32) -> Result<Self, String> {
        Namespace::new(run)?;
        if !pinned_image(image) || uid == 0 || uid == IMAGE_UID {
            return Err(
                "UNVERIFIED: digest-pinned FS_TEST_WAS01_IMAGE and arbitrary non-root UID required"
                    .into(),
            );
        }
        let parse = |source: &str| {
            serde_json::from_str::<Value>(source)
                .map_err(|_| "invalid committed manifest".to_string())
        };
        let mut bootstrap = vec![
            parse(include_str!(
                "../../qualification/manifests/bootstrap/serviceaccount.json"
            ))?,
            parse(include_str!(
                "../../qualification/manifests/bootstrap/role.json"
            ))?,
            parse(include_str!(
                "../../qualification/manifests/bootstrap/rolebinding.json"
            ))?,
        ];
        let mut fixture = vec![
            parse(include_str!(
                "../../qualification/manifests/fixture/serviceaccount.json"
            ))?,
            parse(include_str!(
                "../../qualification/manifests/fixture/deployment.json"
            ))?,
        ];
        for object in bootstrap.iter_mut().chain(fixture.iter_mut()) {
            let base = object["metadata"]["name"]
                .as_str()
                .ok_or("missing manifest name")?;
            let name = format!("{base}-{run}");
            if name.len() > 63 {
                return Err("run-suffixed object name exceeds DNS label limit".into());
            }
            object["metadata"]["name"] = json!(name);
            object["metadata"]["namespace"] = json!(namespace.name());
            object["metadata"]["labels"] = json!({"falcone.test/run":run});
        }
        bootstrap[2]["roleRef"]["name"] = bootstrap[1]["metadata"]["name"].clone();
        bootstrap[2]["subjects"][0]["name"] = bootstrap[0]["metadata"]["name"].clone();
        bootstrap[2]["subjects"][0]["namespace"] = json!(namespace.name());
        let sa = fixture[0]["metadata"]["name"].clone();
        let deploy = &mut fixture[1];
        deploy["spec"]["selector"]["matchLabels"] = json!({"falcone.test/run":run});
        deploy["spec"]["template"]["metadata"]["labels"] = json!({"falcone.test/run":run});
        let pod = &mut deploy["spec"]["template"]["spec"];
        pod["serviceAccountName"] = sa;
        pod["securityContext"]["runAsUser"] = json!(uid);
        pod["containers"][0]["image"] = json!(image);
        Ok(Self {
            namespace,
            run: run.into(),
            uid,
            bootstrap,
            fixture,
        })
    }

    pub fn bootstrap(&self) -> &[Value] {
        &self.bootstrap
    }
    pub fn fixture(&self) -> &[Value] {
        &self.fixture
    }

    pub fn apply(
        &self,
        client: &impl QualificationClient,
        phase: Phase,
        identity: Identity,
        object: &Value,
    ) -> Result<Value, String> {
        let (expected_identity, allowed) = match phase {
            Phase::Bootstrap => (Identity::Admin, &self.bootstrap),
            Phase::Fixture => (Identity::Installer, &self.fixture),
        };
        if identity != expected_identity {
            return Err("refused wrong phase identity".into());
        }
        // A closed allowlist of the rendered committed manifests also rejects extra
        // objects, namespaces, kinds, name collisions, and any posture override.
        // This is deliberately stricter than Kubernetes' admission defaults.
        if !allowed.contains(object) {
            return Err("refused manifest outside this phase's run-owned committed fixture".into());
        }
        self.validate_admitted(object)?;
        let admitted =
            client
                .create(identity, &self.namespace, object)
                .map_err(|_| match phase {
                    Phase::Bootstrap => {
                        "UNVERIFIED: bootstrap forbidden or unavailable".to_string()
                    }
                    Phase::Fixture => "fixture admission failed; API output suppressed".to_string(),
                })?;
        self.validate_admitted(&admitted)?;
        // Never retain raw API responses (a ServiceAccount response may contain
        // sensitive data). Only these server admission identifiers are evidence.
        let record = json!({"phase":format!("{phase:?}"), "kind":admitted["kind"],
            "name":admitted["metadata"]["name"], "uid":admitted["metadata"]["uid"],
            "resourceVersion":admitted["metadata"]["resourceVersion"]});
        if !record["uid"].is_string() || !record["resourceVersion"].is_string() {
            return Err("admission response lacks server identity".into());
        }
        Ok(record)
    }

    pub fn validate_pod(&self, object: &Value) -> Result<(), String> {
        let spec = &object["spec"];
        if object["kind"] != "Pod"
            || object["metadata"]["namespace"] != self.namespace.name()
            || object["metadata"]["labels"]["falcone.test/run"] != self.run
            || !contains_fields(&self.fixture[1]["spec"]["template"]["spec"], spec)
            || !check(&super::deployment(spec), Role::GuestWorker).is_empty()
            || ["initContainers", "ephemeralContainers"]
                .iter()
                .any(|field| {
                    spec.get(field).is_some_and(|value| {
                        value.as_array().is_none_or(|values| !values.is_empty())
                    })
                })
        {
            return Err("actual pod changed run identity or restricted fixture posture".into());
        }
        Ok(())
    }

    fn validate_admitted(&self, object: &Value) -> Result<(), String> {
        let expected = self
            .bootstrap
            .iter()
            .chain(self.fixture.iter())
            .find(|expected| {
                expected["kind"] == object["kind"]
                    && expected["metadata"]["name"] == object["metadata"]["name"]
            })
            .ok_or("admission substituted an unexpected object")?;
        if !contains_fields(expected, object) {
            return Err(
                "admission changed requested permissions, identity or fixture fields".into(),
            );
        }
        if object["metadata"]["namespace"] != self.namespace.name()
            || object["metadata"]["labels"]["falcone.test/run"] != self.run
            || !object["metadata"]["name"]
                .as_str()
                .is_some_and(|name| name.ends_with(&format!("-{}", self.run)))
            || !check(object, Role::GuestWorker).is_empty()
        {
            return Err("refused namespace, run ownership or restricted posture".into());
        }
        match object["kind"].as_str() {
            Some("Deployment") => {
                let pod = &object["spec"]["template"]["spec"];
                if pod["securityContext"]["runAsUser"] != self.uid
                    || pod["securityContext"]["runAsNonRoot"] != true
                    || pod["securityContext"]["seccompProfile"]["type"] != "RuntimeDefault"
                    || pod["containers"].as_array().is_none_or(|containers| {
                        containers.iter().any(|container| {
                            !container["image"].as_str().is_some_and(pinned_image)
                                || container["securityContext"]
                                    .get("runAsUser")
                                    .is_some_and(|uid| uid != self.uid)
                        })
                    })
                {
                    return Err("refused UID, seccomp or image override".into());
                }
            }
            Some("ServiceAccount") if object["automountServiceAccountToken"] != false => {
                return Err("refused service account automount".into());
            }
            _ => {}
        }
        Ok(())
    }
}

/// Run all five phases. The guard is installed before bootstrap and unwinds on panic.
pub fn execute(
    client: &impl QualificationClient,
    mode: &super::NamespaceMode,
    image: &str,
) -> Result<Vec<Value>, String> {
    if !pinned_image(image) {
        return Err("UNVERIFIED: FS_TEST_WAS01_IMAGE must be digest-pinned".into());
    }
    let guard = super::test_namespace(client, "was01", mode)?;
    let result = (|| {
        let plan = Plan::new(
            guard.namespace.clone(),
            &guard.run,
            image,
            super::choose_uid(IMAGE_UID),
        )?;
        let mut records = vec![
            json!({"phase":"Namespace", "namespace":guard.namespace.name(), "run":guard.run,
            "enforce":"restricted", "uid":plan.uid}),
        ];
        for object in plan.bootstrap() {
            records.push(plan.apply(client, Phase::Bootstrap, Identity::Admin, object)?);
        }
        let installer = plan.bootstrap()[0]["metadata"]["name"]
            .as_str()
            .ok_or("missing installer name")?;
        client
            .token_request(&plan.namespace, installer)
            .map_err(|_| "UNVERIFIED: installer TokenRequest forbidden or unavailable")?;
        for (group, resource) in [
            ("rbac.authorization.k8s.io", "clusterroles"),
            ("rbac.authorization.k8s.io", "clusterrolebindings"),
            ("node.k8s.io", "runtimeclasses"),
            ("", "namespaces"),
        ] {
            if client
                .installer_can_create(group, resource)
                .map_err(|_| "UNVERIFIED: installer SSAR unavailable")?
            {
                return Err(format!(
                    "installer unexpectedly authorized to create {resource}"
                ));
            }
            records.push(json!({"phase":"Fixture", "installer_create":resource,"allowed":false}));
        }
        for object in plan.fixture() {
            records.push(plan.apply(client, Phase::Fixture, Identity::Installer, object)?);
        }
        let output = client.observe(&plan)?;
        records.extend(validate_evidence(&output, plan.uid)?);
        Ok(records)
    })();
    let cleanup = guard.close();
    match (result, cleanup) {
        (Ok(records), Ok(())) => Ok(records),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
    }
}

pub fn validate_evidence(output: &str, uid: u32) -> Result<Vec<Value>, String> {
    let records = output
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "invalid runner JSON; raw output suppressed")?;
    let expected = [
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
        ("tmp_writable", "/tmp write probe succeeded"),
        ("non_root_uid", ""),
    ];
    if records.len() != expected.len() {
        return Err("incomplete/duplicate runner evidence".into());
    }
    let mut sanitized = Vec::new();
    for (record, (check, reason)) in records.iter().zip(expected) {
        if record["check"] != check
            || record["status"] != "passed"
            || !record["error_class"].is_null()
        {
            return Err(format!(
                "runner {check} failed/unverified/missing; raw output suppressed"
            ));
        }
        let observed = record["reason"]
            .as_str()
            .ok_or("runner reason is missing")?;
        if check == "non_root_uid" {
            if !uid_evidence(observed, uid) {
                return Err("runner effective UID differs from chosen UID or is unverified".into());
            }
            sanitized.push(json!({"check":check, "status":"passed", "effective_uid":uid}));
        } else {
            if observed != reason {
                return Err(format!(
                    "runner {check} has unexpected outcome; raw output suppressed"
                ));
            }
            sanitized.push(json!({"check":check, "status":"passed", "reason":reason}));
        }
    }
    Ok(sanitized)
}

fn uid_evidence(reason: &str, uid: u32) -> bool {
    let Some(rest) = reason.strip_prefix(&format!("uid={uid} gid=")) else {
        return false;
    };
    let Some((gid, rest)) = rest.split_once(" (effective); uid set=[") else {
        return false;
    };
    let Ok(gid) = gid.parse::<u32>() else {
        return false;
    };
    let Some((uids, gids)) = rest.split_once("], gid set=[") else {
        return false;
    };
    let Some(gids) = gids.strip_suffix(']') else {
        return false;
    };
    let parse = |text: &str| {
        text.split(", ")
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
    };
    let (Ok(uids), Ok(gids)) = (parse(uids), parse(gids)) else {
        return false;
    };
    uids.len() == 4 && gids.len() == 4 && !uids.contains(&0) && uids[1] == uid && gids[1] == gid
}

pub struct Settings {
    pub mode: super::NamespaceMode,
    pub image: String,
}

impl Settings {
    pub fn parse(
        cluster: Option<&str>,
        kubeconfig: Option<&str>,
        image: Option<&str>,
        mode: Option<&str>,
        namespace: Option<&str>,
    ) -> Result<Option<Self>, String> {
        if cluster != Some("1") {
            return Ok(None);
        }
        if kubeconfig.is_none_or(|value| value.trim().is_empty()) {
            return Err(
                "UNVERIFIED: KUBECONFIG is required for FS_TEST_CLUSTER=1 (value suppressed)"
                    .into(),
            );
        }
        let image = image.filter(|image| pinned_image(image)).ok_or(
            "UNVERIFIED: digest-pinned FS_TEST_WAS01_IMAGE is required for FS_TEST_CLUSTER=1",
        )?;
        let mode = super::NamespaceMode::parse(mode, namespace)
            .map_err(|error| format!("UNVERIFIED: {error}"))?;
        Ok(Some(Self {
            mode,
            image: image.into(),
        }))
    }

    pub fn execute(&self, client: &impl QualificationClient) -> Result<Vec<Value>, String> {
        execute(client, &self.mode, &self.image)
    }
}

/// A fast one-shot may exit before a poll observes Ready. Only its first run counts.
pub fn first_run(pod: &Value) -> Result<Option<String>, String> {
    let Some(statuses) = pod["status"]["containerStatuses"].as_array() else {
        return Ok(None);
    };
    if statuses.len() != 1 {
        return Err("unexpected container status count".into());
    }
    let status = &statuses[0];
    if status["name"] != "runner"
        || status["restartCount"] != 0
        || status
            .get("lastState")
            .is_some_and(|last| last.as_object().is_none_or(|map| !map.is_empty()))
    {
        return Err("runner restarted or has unverified first-run status".into());
    }
    if let Some(terminated) = status["state"].get("terminated") {
        if terminated["exitCode"] != 0 {
            return Err("runner exited nonzero".into());
        }
        return status["containerID"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(|id| Some(id.into()))
            .ok_or_else(|| "runner container identity missing".into());
    }
    Ok(None)
}

// Permit server defaults, but never remove/change requested fields or append to
// lists such as RBAC rules, subjects, containers, capabilities or volumes.
fn contains_fields(expected: &Value, admitted: &Value) -> bool {
    match expected {
        Value::Object(fields) => admitted.as_object().is_some_and(|object| {
            fields.iter().all(|(key, value)| {
                object
                    .get(key)
                    .is_some_and(|observed| contains_fields(value, observed))
            })
        }),
        Value::Array(items) => admitted.as_array().is_some_and(|observed| {
            items.len() == observed.len()
                && items
                    .iter()
                    .zip(observed)
                    .all(|(expected, actual)| contains_fields(expected, actual))
        }),
        _ => expected == admitted,
    }
}
