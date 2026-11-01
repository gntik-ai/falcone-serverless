use serde_json::{Value, json};

pub fn final_uid(containerfile: &str) -> Result<u64, String> {
    let mut user = None;
    for line in containerfile.lines() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some(instruction) if instruction.eq_ignore_ascii_case("FROM") => user = None,
            Some(instruction) if instruction.eq_ignore_ascii_case("USER") => user = words.next(),
            _ => {}
        }
    }
    user.and_then(|user| user.split(':').next())
        .filter(|uid| !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|uid| uid.parse::<u64>().ok())
        .filter(|uid| *uid > 0)
        .ok_or_else(|| "final-stage USER must be a numeric non-zero UID".into())
}

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
    if spec["restartPolicy"] != "Never"
        || spec["containers"].as_array().is_none_or(|containers| {
            containers
                .iter()
                .any(|container| !container["image"].as_str().is_some_and(pinned_image))
        })
        || spec.get("initContainers").is_some_and(|value| {
            value.as_array().is_none_or(|containers| {
                containers
                    .iter()
                    .any(|container| !container["image"].as_str().is_some_and(pinned_image))
            })
        })
        || spec.get("ephemeralContainers").is_some()
    {
        return Err("test pod requires restartPolicy Never and digest-pinned images".into());
    }
    Ok(())
}
