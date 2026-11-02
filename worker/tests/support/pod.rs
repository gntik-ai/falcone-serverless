use super::pinned_image;
use serde_json::{Value, json};

pub fn pod_spec(image: &str) -> Result<Value, String> {
    if !pinned_image(image) {
        return Err("worker image must be <repo>@sha256:<64 lowercase hex>".into());
    }
    Ok(json!({
        "restartPolicy": "Never",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": {
            "runAsNonRoot": true, "runAsUser": 10001,
            "seccompProfile": {"type": "RuntimeDefault"}
        },
        "containers": [{"name": "worker", "image": image,
            "securityContext": {
                "allowPrivilegeEscalation": false,
                "readOnlyRootFilesystem": true,
                "capabilities": {"drop": ["ALL"]}
            }
        }]
    }))
}

pub fn deployment(spec: &Value) -> Value {
    json!({"apiVersion": "apps/v1", "kind": "Deployment",
        "spec": {"template": {"spec": spec}}})
}

pub(super) fn validate_spec(spec: &Value) -> Result<(), String> {
    let violations = falcone_serverless_worker::check(
        &deployment(spec),
        falcone_serverless_worker::Role::GuestWorker,
    );
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
    // Every container that can start in the pod is digest-pinned. Ephemeral containers are
    // not part of the single-worker harness pod and are refused outright.
    let pinned = |field: &str, required: bool| match spec.get(field) {
        None | Some(Value::Null) => !required,
        Some(containers) => containers.as_array().is_some_and(|containers| {
            (!required || !containers.is_empty())
                && containers
                    .iter()
                    .all(|container| container["image"].as_str().is_some_and(pinned_image))
        }),
    };
    if spec["restartPolicy"] != "Never"
        || !pinned("containers", true)
        || !pinned("initContainers", false)
        || spec.get("ephemeralContainers").is_some()
    {
        return Err("test pod requires restartPolicy Never and digest-pinned images".into());
    }
    Ok(())
}
