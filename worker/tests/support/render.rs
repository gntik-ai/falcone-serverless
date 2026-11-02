//! Rendering and the pre-apply refusal guard for the WAS-01 manifests. The committed
//! templates hold the static posture; the harness injects only the run-specific values
//! (names, namespace, run label, image digest and the chosen UID). A template that already
//! sets one of those, omits a posture field or reaches outside its own namespace is refused
//! before any client call.

use super::{RUN_LABEL, pinned_image};
use falcone_serverless_worker::{Role, check};
use serde_json::{Value, json};

pub const BOOTSTRAP: [&str; 3] = [
    include_str!("../../qualification/manifests/bootstrap/installer-serviceaccount.json"),
    include_str!("../../qualification/manifests/bootstrap/installer-role.json"),
    include_str!("../../qualification/manifests/bootstrap/installer-rolebinding.json"),
];

pub const FIXTURE: [&str; 2] = [
    include_str!("../../qualification/manifests/fixture/workload-serviceaccount.json"),
    include_str!("../../qualification/manifests/fixture/deployment.json"),
];

/// Base name of the installer ServiceAccount; the rendered name carries the run suffix.
pub const INSTALLER: &str = "was01-installer";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Bootstrap,
    Fixture,
}

/// Everything the harness decides for one run.
#[derive(Clone, Debug)]
pub struct Run {
    pub namespace: String,
    pub run: String,
    pub image: String,
    pub uid: u64,
    pub image_uid: u64,
}

impl Run {
    pub fn object_name(&self, base: &str) -> String {
        format!("{base}-{}", self.run)
    }
}

const MIN_UID: u64 = 1_000_000_000;
const UID_SPAN: u64 = 1_000_000_000;

/// A random non-root UID in an OpenShift-like range, never 0 and never the image USER.
pub fn choose_uid(image_uid: u64, mut entropy: impl FnMut() -> u64) -> u64 {
    loop {
        let uid = MIN_UID + entropy() % UID_SPAN;
        if uid != 0 && uid != image_uid {
            return uid;
        }
    }
}

pub fn entropy() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    RandomState::new().hash_one(std::process::id())
}

fn set(object: &mut Value, pointer: &str, value: Value) -> Result<(), String> {
    let (parent, key) = pointer.rsplit_once('/').ok_or("bad pointer")?;
    let parent = if parent.is_empty() {
        Some(&mut *object)
    } else {
        object.pointer_mut(parent)
    };
    match parent {
        Some(Value::Object(map)) => {
            map.insert(key.replace("~1", "/").replace("~0", "~"), value);
            Ok(())
        }
        _ => Err(format!("manifest has no {pointer} parent")),
    }
}

/// Parses a template and injects the run values. Refuses a template that already sets one.
pub fn render(phase: Phase, template: &str, run: &Run) -> Result<Value, String> {
    let mut object: Value =
        serde_json::from_str(template).map_err(|_| "refused: manifest is not JSON".to_string())?;
    for owned in [
        "/metadata/namespace",
        "/metadata/labels/falcone.test~1run",
        "/spec/template/spec/securityContext/runAsUser",
    ] {
        if object.pointer(owned).is_some() {
            return Err(format!("refused: manifest overrides harness-owned {owned}"));
        }
    }
    let name = object["metadata"]["name"]
        .as_str()
        .ok_or("refused: manifest has no name")?
        .to_owned();
    set(&mut object, "/metadata/name", json!(run.object_name(&name)))?;
    set(&mut object, "/metadata/namespace", json!(run.namespace))?;
    if object["metadata"]["labels"].is_null() {
        set(&mut object, "/metadata/labels", json!({}))?;
    }
    set(
        &mut object,
        &format!("/metadata/labels/{}", escape(RUN_LABEL)),
        json!(run.run),
    )?;
    match object["kind"].as_str() {
        Some("RoleBinding") => {
            let role = object["roleRef"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            set(&mut object, "/roleRef/name", json!(run.object_name(&role)))?;
            if let Some(subjects) = object["subjects"].as_array_mut() {
                for subject in subjects {
                    let base = subject["name"].as_str().unwrap_or_default().to_owned();
                    subject["name"] = json!(run.object_name(&base));
                    subject["namespace"] = json!(run.namespace);
                }
            }
        }
        Some("Deployment") => render_deployment(&mut object, run)?,
        _ => {}
    }
    guard(phase, &object, run)?;
    Ok(object)
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn render_deployment(object: &mut Value, run: &Run) -> Result<(), String> {
    for pointer in [
        "/spec/selector/matchLabels",
        "/spec/template/metadata/labels",
    ] {
        if !object.pointer(pointer).is_some_and(Value::is_object) {
            return Err(format!("refused: manifest has no {pointer}"));
        }
        set(
            object,
            &format!("{pointer}/{}", escape(RUN_LABEL)),
            json!(run.run),
        )?;
    }
    let pod = object
        .pointer_mut("/spec/template/spec")
        .and_then(Value::as_object_mut)
        .ok_or("refused: manifest has no pod spec")?;
    if let Some(account) = pod.get("serviceAccountName").and_then(Value::as_str) {
        let renamed = run.object_name(account);
        pod.insert("serviceAccountName".into(), json!(renamed));
    }
    let context = pod
        .entry("securityContext")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("refused: pod securityContext is not an object")?;
    context.insert("runAsUser".into(), json!(run.uid));
    for container in pod
        .get_mut("containers")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if container.get("image").is_some() {
            return Err("refused: manifest overrides harness-owned container image".into());
        }
        container["image"] = json!(run.image);
    }
    Ok(())
}

fn allowed_kind(phase: Phase, object: &Value) -> bool {
    matches!(
        (
            phase,
            object["apiVersion"].as_str(),
            object["kind"].as_str()
        ),
        (
            Phase::Bootstrap | Phase::Fixture,
            Some("v1"),
            Some("ServiceAccount")
        ) | (
            Phase::Bootstrap,
            Some("rbac.authorization.k8s.io/v1"),
            Some("Role" | "RoleBinding")
        ) | (Phase::Fixture, Some("apps/v1"), Some("Deployment"))
    )
}

/// Violations of the installer Role's boundary: only the namespaced verbs the installer
/// needs, no wildcard, no RBAC objects and no token subresource.
pub fn role_violations(role: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    for rule in role["rules"].as_array().into_iter().flatten() {
        let list = |key: &str| -> Vec<&str> {
            rule[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect()
        };
        for key in ["apiGroups", "resources", "verbs", "resourceNames"] {
            if list(key).contains(&"*") {
                violations.push(format!("wildcard in {key}"));
            }
        }
        for resource in list("resources") {
            if matches!(
                resource,
                "roles" | "rolebindings" | "clusterroles" | "clusterrolebindings"
            ) || resource == "serviceaccounts/token"
                || resource.ends_with("/token")
            {
                violations.push(format!("forbidden resource {resource}"));
            }
        }
        for verb in list("verbs") {
            if !matches!(verb, "create" | "get" | "list" | "watch" | "delete") {
                violations.push(format!("forbidden verb {verb}"));
            }
        }
    }
    violations
}

/// The refusal guard: every rendered object passes it before the first apply.
pub fn guard(phase: Phase, object: &Value, run: &Run) -> Result<(), String> {
    if !allowed_kind(phase, object) {
        return Err(format!(
            "refused: {} {} is not allowed in the {phase:?} phase",
            object["apiVersion"].as_str().unwrap_or("?"),
            object["kind"].as_str().unwrap_or("?"),
        ));
    }
    if object["metadata"]["namespace"].as_str() != Some(run.namespace.as_str()) {
        return Err("refused: object is not in the run's own namespace".into());
    }
    if object["metadata"]["labels"][RUN_LABEL].as_str() != Some(run.run.as_str()) {
        return Err("refused: object does not carry this run's label".into());
    }
    let violations = check(object, Role::GuestWorker);
    if !violations.is_empty() {
        return Err(format!(
            "refused posture: {}",
            violations
                .iter()
                .map(|v| v.rule_id)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    match object["kind"].as_str() {
        Some("Role") => {
            let violations = role_violations(object);
            if !violations.is_empty() {
                return Err(format!("refused role: {}", violations.join(", ")));
            }
        }
        Some("RoleBinding") => {
            let installer = run.object_name(INSTALLER);
            let only_installer = object["subjects"].as_array().is_some_and(|subjects| {
                !subjects.is_empty()
                    && subjects.iter().all(|subject| {
                        subject["kind"] == "ServiceAccount"
                            && subject["name"] == installer.as_str()
                            && subject["namespace"] == run.namespace.as_str()
                    })
            });
            if !only_installer {
                return Err("refused: binding subject is not the run's installer".into());
            }
        }
        Some("Deployment") => guard_deployment(object, run)?,
        _ => {}
    }
    Ok(())
}

fn guard_deployment(object: &Value, run: &Run) -> Result<(), String> {
    let pod = &object["spec"]["template"]["spec"];
    let uid = pod["securityContext"]["runAsUser"].as_u64();
    if uid != Some(run.uid) || run.uid == 0 || run.uid == run.image_uid {
        return Err("refused: runAsUser must be the harness-chosen non-zero UID that differs from the image USER".into());
    }
    if pod.get("ephemeralContainers").is_some()
        || pod.get("initContainers").is_some()
        || pod["containers"].as_array().is_none_or(|c| c.len() != 1)
    {
        return Err("refused: the pod must run exactly one container".into());
    }
    let container = &pod["containers"][0];
    if !container["image"].as_str().is_some_and(pinned_image) {
        return Err("refused: image must be pinned by @sha256 digest".into());
    }
    for resource in ["cpu", "memory"] {
        if container["resources"]["limits"][resource].is_null() {
            return Err(format!("refused: container has no {resource} limit"));
        }
    }
    let volumes = pod["volumes"].as_array().map_or(&[][..], Vec::as_slice);
    if volumes
        .iter()
        .any(|volume| volume["emptyDir"]["sizeLimit"].is_null())
    {
        return Err("refused: only emptyDir volumes with a sizeLimit are allowed".into());
    }
    if object["spec"]["replicas"] != 1 {
        return Err("refused: the fixture runs exactly one replica".into());
    }
    Ok(())
}
