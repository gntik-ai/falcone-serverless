//! Static restricted-posture contract v1; never executes a workload.

use serde_json::Value;

const RULE_IDS: [&str; 12] = [
    "RP-01", "RP-02", "RP-03", "RP-04", "RP-05", "RP-06", "RP-07", "RP-08", "RP-09", "RP-10",
    "RP-11", "RP-12",
];

/// The deployment's trusted platform role, supplied by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    GuestWorker,
    Controller,
}

/// One failed contract rule. Each rule appears at most once per object.
#[derive(Debug, Eq, PartialEq)]
pub struct Violation {
    pub rule_id: &'static str,
}

/// Check one JSON Kubernetes object against restricted-posture contract v1.
#[must_use]
pub fn check(object: &Value, role: Role) -> Vec<Violation> {
    let allowed = match (object["apiVersion"].as_str(), object["kind"].as_str()) {
        (Some("apps/v1"), Some("Deployment"))
        | (Some("v1"), Some("Service" | "ServiceAccount"))
        | (Some("coordination.k8s.io/v1"), Some("Lease"))
        | (Some("rbac.authorization.k8s.io/v1"), Some("Role")) => true,
        (Some("rbac.authorization.k8s.io/v1"), Some("RoleBinding")) => {
            object["roleRef"]["kind"].as_str() == Some("Role")
        }
        _ => false,
    };
    if !allowed {
        return vec![Violation { rule_id: "RP-12" }];
    }
    if object["kind"].as_str() != Some("Deployment") {
        return Vec::new();
    }
    let pod = &object["spec"]["template"]["spec"];
    if !pod.is_object() {
        return RULE_IDS[..11]
            .iter()
            .filter(|id| role != Role::Controller || **id != "RP-10")
            .map(|id| Violation { rule_id: id })
            .collect();
    }
    let mut valid = [true; 12];
    if pod
        .get("containers")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
        || pod
            .get("initContainers")
            .is_some_and(|value| !value.is_array())
    {
        valid[..6].fill(false);
        valid[8] = false;
    }
    let pod_context = &pod["securityContext"];
    if pod
        .get("securityContext")
        .is_some_and(|context| !context.is_object())
    {
        valid[0] = false;
        valid[4] = false;
    }
    for container in pod["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(pod["initContainers"].as_array().into_iter().flatten())
    {
        if !container.is_object() {
            valid[..6].fill(false);
            valid[8] = false;
            continue;
        }
        let context = &container["securityContext"];
        if container
            .get("securityContext")
            .is_some_and(|context| !context.is_object())
        {
            valid[..6].fill(false);
        }
        let effective = |field: &str| context.get(field).or_else(|| pod_context.get(field));
        valid[0] &= effective("runAsNonRoot").and_then(Value::as_bool) == Some(true)
            && effective("runAsUser").is_none_or(|uid| uid.as_u64().is_some_and(|uid| uid > 0));
        valid[1] &= context["allowPrivilegeEscalation"].as_bool() == Some(false);
        valid[2] &= context
            .get("privileged")
            .is_none_or(|value| value.as_bool() == Some(false));
        valid[3] &= context["capabilities"]["drop"]
            .as_array()
            .is_some_and(|drop| {
                drop.iter().all(Value::is_string)
                    && drop.iter().any(|cap| cap.as_str() == Some("ALL"))
            })
            && context["capabilities"]
                .get("add")
                .is_none_or(|add| add.as_array().is_some_and(Vec::is_empty));
        valid[4] &= effective("seccompProfile")
            .and_then(|profile| profile.get("type"))
            .and_then(Value::as_str)
            == Some("RuntimeDefault");
        valid[8] &= container.get("ports").is_none_or(|ports| {
            ports.as_array().is_some_and(|ports| {
                ports
                    .iter()
                    .all(|port| port.is_object() && port.get("hostPort").is_none())
            })
        });
        valid[5] &= context["readOnlyRootFilesystem"].as_bool() == Some(true);
    }
    valid[6] = ["hostNetwork", "hostPID", "hostIPC"].iter().all(|field| {
        pod.get(field)
            .is_none_or(|value| value.as_bool() == Some(false))
    });
    valid[7] = pod.get("volumes").is_none_or(|volumes| {
        volumes.as_array().is_some_and(|volumes| {
            volumes
                .iter()
                .all(|volume| volume.is_object() && volume.get("hostPath").is_none())
        })
    });
    valid[10] = pod.get("runtimeClassName").is_none();
    valid[9] =
        role == Role::Controller || pod["automountServiceAccountToken"].as_bool() == Some(false);
    valid
        .into_iter()
        .enumerate()
        .filter_map(|(index, valid)| {
            (!valid).then_some(Violation {
                rule_id: RULE_IDS[index],
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Role, check};
    use serde_json::json;

    #[test]
    fn missing_security_controls_fail_closed() {
        let object = json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "spec": {"template": {"spec": {"containers": [{"name": "worker"}]}}}
        });
        let ids: Vec<_> = check(&object, Role::GuestWorker)
            .iter()
            .map(|violation| violation.rule_id)
            .collect();
        assert_eq!(ids, ["RP-01", "RP-02", "RP-04", "RP-05", "RP-06", "RP-10"]);
    }
}

#[cfg(test)]
mod malformed_tests {
    use super::{Role, check};
    use serde_json::{Value, json};

    fn deployment() -> Value {
        json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "spec": {"template": {"spec": {
                "automountServiceAccountToken": false,
                "securityContext": {"runAsNonRoot": true, "seccompProfile": {"type": "RuntimeDefault"}},
                "containers": [{"name": "worker", "securityContext": {
                    "allowPrivilegeEscalation": false, "readOnlyRootFilesystem": true,
                    "capabilities": {"drop": ["ALL"]}
                }}]
            }}}
        })
    }

    #[test]
    fn malformed_container_lists_fail_closed_without_panicking() {
        for field in ["containers", "initContainers"] {
            for value in [
                Value::Null,
                json!(false),
                json!({}),
                json!([null]),
                json!([42]),
            ] {
                let mut object = deployment();
                object["spec"]["template"]["spec"][field] = value;
                let violations = check(&object, Role::GuestWorker);
                let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
                assert_eq!(
                    ids,
                    [
                        "RP-01", "RP-02", "RP-03", "RP-04", "RP-05", "RP-06", "RP-09"
                    ],
                    "{object}"
                );
            }
        }
        for containers in [None, Some(json!([]))] {
            let mut object = deployment();
            let pod = object["spec"]["template"]["spec"].as_object_mut().unwrap();
            pod.remove("containers");
            if let Some(containers) = containers {
                pod.insert("containers".into(), containers);
            }
            let violations = check(&object, Role::GuestWorker);
            let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
            assert_eq!(
                ids,
                [
                    "RP-01", "RP-02", "RP-03", "RP-04", "RP-05", "RP-06", "RP-09"
                ]
            );
        }
    }
    #[test]
    fn wrongly_typed_security_contexts_are_not_missing_fields() {
        for value in [Value::Null, json!(false), json!("restricted"), json!([])] {
            let mut object = deployment();
            object["spec"]["template"]["spec"]["containers"][0]["securityContext"] = value.clone();
            let violations = check(&object, Role::GuestWorker);
            let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
            assert_eq!(ids, ["RP-01", "RP-02", "RP-03", "RP-04", "RP-05", "RP-06"]);

            let mut object = deployment();
            let pod = &mut object["spec"]["template"]["spec"];
            pod["containers"][0]["securityContext"]["runAsNonRoot"] = json!(true);
            pod["containers"][0]["securityContext"]["seccompProfile"] =
                json!({"type":"RuntimeDefault"});
            pod["securityContext"] = value;
            let violations = check(&object, Role::GuestWorker);
            let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
            assert_eq!(ids, ["RP-01", "RP-05"]);
        }
    }

    #[test]
    fn absent_or_malformed_pod_specs_fail_all_applicable_pod_rules() {
        let expected = [
            "RP-01", "RP-02", "RP-03", "RP-04", "RP-05", "RP-06", "RP-07", "RP-08", "RP-09",
            "RP-10", "RP-11",
        ];
        for value in [Value::Null, json!(false), json!([]), json!(42)] {
            for path in ["/spec", "/spec/template", "/spec/template/spec"] {
                let mut object = deployment();
                *object.pointer_mut(path).unwrap() = value.clone();
                let violations = check(&object, Role::GuestWorker);
                let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
                assert_eq!(ids, expected, "{path}: {value}");
                let violations = check(&object, Role::Controller);
                let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
                assert_eq!(
                    ids,
                    [
                        "RP-01", "RP-02", "RP-03", "RP-04", "RP-05", "RP-06", "RP-07", "RP-08",
                        "RP-09", "RP-11"
                    ]
                );
            }
        }
        let object = json!({"apiVersion": "apps/v1", "kind": "Deployment"});
        let violations = check(&object, Role::GuestWorker);
        let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn wrongly_typed_fields_fail_their_rule_instead_of_defaulting() {
        for (path, value, expected) in [
            (
                "/spec/template/spec/securityContext/runAsNonRoot",
                json!("true"),
                "RP-01",
            ),
            (
                "/spec/template/spec/containers/0/securityContext/runAsUser",
                Value::Null,
                "RP-01",
            ),
            (
                "/spec/template/spec/containers/0/securityContext/runAsUser",
                json!(-1),
                "RP-01",
            ),
            (
                "/spec/template/spec/containers/0/securityContext/allowPrivilegeEscalation",
                json!("false"),
                "RP-02",
            ),
            (
                "/spec/template/spec/containers/0/securityContext/privileged",
                Value::Null,
                "RP-03",
            ),
            (
                "/spec/template/spec/containers/0/securityContext/capabilities/drop",
                json!("ALL"),
                "RP-04",
            ),
            (
                "/spec/template/spec/containers/0/securityContext/capabilities/drop",
                json!(["ALL", 1]),
                "RP-04",
            ),
            (
                "/spec/template/spec/containers/0/securityContext/capabilities/add",
                Value::Null,
                "RP-04",
            ),
            (
                "/spec/template/spec/securityContext/seccompProfile",
                json!("RuntimeDefault"),
                "RP-05",
            ),
            (
                "/spec/template/spec/containers/0/securityContext/readOnlyRootFilesystem",
                json!(1),
                "RP-06",
            ),
            ("/spec/template/spec/hostNetwork", json!("false"), "RP-07"),
            ("/spec/template/spec/hostPID", Value::Null, "RP-07"),
            ("/spec/template/spec/hostIPC", json!(true), "RP-07"),
            ("/spec/template/spec/volumes", json!({}), "RP-08"),
            ("/spec/template/spec/volumes", json!([null]), "RP-08"),
            (
                "/spec/template/spec/volumes",
                json!([{"hostPath":null}]),
                "RP-08",
            ),
            ("/spec/template/spec/containers/0/ports", json!({}), "RP-09"),
            (
                "/spec/template/spec/containers/0/ports",
                json!([null]),
                "RP-09",
            ),
            (
                "/spec/template/spec/containers/0/ports",
                json!([{"hostPort":0}]),
                "RP-09",
            ),
            (
                "/spec/template/spec/automountServiceAccountToken",
                json!("false"),
                "RP-10",
            ),
            ("/spec/template/spec/runtimeClassName", Value::Null, "RP-11"),
        ] {
            let mut object = deployment();
            // Insert at a public JSON path, including fields intentionally absent in the baseline.
            let (parent, field) = path.rsplit_once('/').unwrap();
            object
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(field.into(), value);
            let violations = check(&object, Role::GuestWorker);
            let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
            assert_eq!(ids, [expected], "{path}: {object}");
        }
    }

    #[test]
    fn every_container_and_init_container_is_checked_and_rules_are_deduplicated() {
        for field in ["containers", "initContainers"] {
            let mut object = deployment();
            let pod = &mut object["spec"]["template"]["spec"];
            let good = pod["containers"][0].clone();
            let mut bad = good.clone();
            bad["securityContext"]["allowPrivilegeEscalation"] = json!(true);
            pod[field] = json!([good, bad.clone(), bad]);
            let violations = check(&object, Role::GuestWorker);
            let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
            assert_eq!(ids, ["RP-02"], "{field}");
        }
    }

    #[test]
    fn pod_values_cannot_supply_container_only_controls() {
        for (field, value, expected) in [
            ("allowPrivilegeEscalation", json!(false), "RP-02"),
            ("capabilities", json!({"drop":["ALL"]}), "RP-04"),
            ("readOnlyRootFilesystem", json!(true), "RP-06"),
        ] {
            let mut object = deployment();
            let pod = &mut object["spec"]["template"]["spec"];
            pod["securityContext"][field] = value;
            pod["containers"][0]["securityContext"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            let violations = check(&object, Role::GuestWorker);
            let ids: Vec<_> = violations.iter().map(|v| v.rule_id).collect();
            assert_eq!(ids, [expected]);
        }
    }
}
