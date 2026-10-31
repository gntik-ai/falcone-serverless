//! WAS-01 admission/runtime qualification; offline tests never contact Kubernetes.
use std::{fs, path::PathBuf};

fn qualification() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("qualification")
}

#[test]
fn committed_image_is_pinned_non_root_and_exec_only() {
    let source = fs::read_to_string(qualification().join("Containerfile")).unwrap();
    let mut from = 0;
    let mut user = None;
    let mut entrypoint = false;
    for line in source.lines() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("FROM") => {
                from += 1;
                assert!(words.next().unwrap().contains("@sha256:"));
                user = None;
            }
            Some("USER") => user = words.next().and_then(|s| s.parse::<u32>().ok()),
            Some("ENTRYPOINT") => {
                let args: Vec<String> =
                    serde_json::from_str(line.trim_start_matches("ENTRYPOINT").trim()).unwrap();
                assert_eq!(args, ["/usr/local/bin/qualification_runner"]);
                entrypoint = true;
            }
            _ => {}
        }
    }
    assert_eq!(from, 2);
    assert_eq!(user, Some(65532));
    assert!(entrypoint);
    assert!(source.contains("--locked"));
    assert!(source.contains("chmod 0555"));
}

#[test]
fn committed_bootstrap_grants_only_the_installer_contract() {
    let role: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(qualification().join("manifests/bootstrap/role.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(role["kind"], "Role");
    assert_eq!(
        role["rules"],
        serde_json::json!([
            {"apiGroups":[""],"resources":["serviceaccounts"],"verbs":["create","get","delete"]},
            {"apiGroups":["apps"],"resources":["deployments"],"verbs":["create","get","delete"]},
            {"apiGroups":[""],"resources":["pods"],"verbs":["get","list","watch"]},
            {"apiGroups":[""],"resources":["pods/log"],"verbs":["get"]}
        ])
    );
    let sa: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(qualification().join("manifests/bootstrap/serviceaccount.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(sa["automountServiceAccountToken"], false);
    let binding: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(qualification().join("manifests/bootstrap/rolebinding.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(binding["roleRef"]["kind"], "Role");
    assert_eq!(binding["subjects"][0]["kind"], "ServiceAccount");
}

#[test]
fn committed_fixture_is_restricted_with_bounded_scratch_and_resources() {
    use falcone_serverless_worker::{Role, check};
    use serde_json::Value;
    let deployment: Value = serde_json::from_str(
        &fs::read_to_string(qualification().join("manifests/fixture/deployment.json")).unwrap(),
    )
    .unwrap();
    assert!(check(&deployment, Role::GuestWorker).is_empty());
    assert_eq!(deployment["spec"]["replicas"], 1);
    let pod = &deployment["spec"]["template"]["spec"];
    assert_eq!(pod["securityContext"]["runAsUser"], 10001);
    assert_eq!(
        pod["volumes"],
        serde_json::json!([{"name":"tmp","emptyDir":{"sizeLimit":"16Mi"}}])
    );
    assert_eq!(
        pod["containers"][0]["volumeMounts"],
        serde_json::json!([{"name":"tmp","mountPath":"/tmp"}])
    );
    assert_eq!(
        pod["containers"][0]["resources"]["limits"],
        serde_json::json!({"cpu":"1","memory":"512Mi"})
    );
    let sa: Value = serde_json::from_str(
        &fs::read_to_string(qualification().join("manifests/fixture/serviceaccount.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(sa["automountServiceAccountToken"], false);
    assert_eq!(pod["serviceAccountName"], sa["metadata"]["name"]);
}

pub mod support;
use serde_json::{Value, json};
use std::cell::RefCell;
use support::was01::{Identity, Phase, Plan, QualificationClient};
use support::{Namespace, NamespaceClient};

#[derive(Default)]
struct Fake {
    events: RefCell<Vec<String>>,
    objects: RefCell<Vec<Value>>,
    created: RefCell<Vec<Value>>,
    deny: Option<&'static str>,
    panic_fixture: bool,
    admission_override: Option<(&'static str, Value)>,
    shared: Option<Value>,
}

impl NamespaceClient for Fake {
    fn can_create_namespace(&self) -> Result<bool, String> {
        self.events.borrow_mut().push("namespace SSAR".into());
        Ok(self.deny != Some("namespace"))
    }
    fn get_namespace(&self, ns: &Namespace) -> Result<Value, String> {
        self.events.borrow_mut().push("get namespace".into());
        Ok(self.shared.clone().unwrap_or_else(|| ns.manifest()))
    }
    fn create_namespace(&self, _: &Namespace) -> Result<(), String> {
        self.events.borrow_mut().push("create namespace".into());
        Ok(())
    }
    fn delete_namespace(&self, _: &Namespace) -> Result<(), String> {
        self.events.borrow_mut().push("delete namespace".into());
        Ok(())
    }
    fn cleanup_run(&self, _: &Namespace, run: &str) -> Result<(), String> {
        self.events.borrow_mut().push("cleanup run".into());
        self.objects
            .borrow_mut()
            .retain(|o| o["metadata"]["labels"]["falcone.test/run"] != run);
        Ok(())
    }
}

impl QualificationClient for Fake {
    fn create(&self, identity: Identity, _: &Namespace, object: &Value) -> Result<Value, String> {
        self.events
            .borrow_mut()
            .push(format!("{identity:?} {}", object["kind"].as_str().unwrap()));
        if identity == Identity::Admin && self.deny == Some("bootstrap") {
            return Err("forbidden".into());
        }
        assert!(
            !(identity == Identity::Installer && self.panic_fixture),
            "phase 4 panic"
        );
        self.created.borrow_mut().push(object.clone());
        self.objects.borrow_mut().push(object.clone());
        let mut admitted = object.clone();
        if let Some((path, replacement)) = &self.admission_override {
            *admitted.pointer_mut(path).unwrap() = replacement.clone();
        }
        admitted["metadata"]["uid"] = json!("server-uid");
        admitted["metadata"]["resourceVersion"] = json!("7");
        Ok(admitted)
    }
    fn token_request(&self, _: &Namespace, _: &str) -> Result<(), String> {
        self.events.borrow_mut().push("TokenRequest".into());
        if self.deny == Some("token") {
            Err("forbidden".into())
        } else {
            Ok(())
        }
    }
    fn installer_can_create(&self, _: &str, resource: &str) -> Result<bool, String> {
        self.events
            .borrow_mut()
            .push(format!("installer SSAR {resource}"));
        Ok(false)
    }
    fn observe(&self, _: &Plan) -> Result<String, String> {
        self.events.borrow_mut().push("observe".into());
        let uid = self
            .objects
            .borrow()
            .iter()
            .find(|o| o["kind"] == "Deployment")
            .unwrap()["spec"]["template"]["spec"]["securityContext"]["runAsUser"]
            .as_u64()
            .unwrap();
        Ok(runner_records(uid))
    }
}

fn runner_records(uid: u64) -> String {
    [
        ("engine_config", "engine succeeded".into()),
        ("compile_valid", "compile succeeded".into()),
        ("load_executable_memory", "load succeeded".into()),
        ("run_valid", "run returned 42".into()),
        ("interrupt_epoch", "observed epoch interruption".into()),
        ("interrupt_fuel", "observed fuel exhaustion".into()),
        ("root_fs_read_only", "root mount is ro and write probe failed".into()),
        ("tmp_writable", "/tmp write probe succeeded".into()),
        ("non_root_uid", format!("uid={uid} gid=1000 (effective); uid set=[{uid}, {uid}, {uid}, {uid}], gid set=[1000, 1000, 1000, 1000]")),
    ].into_iter().map(|(check, reason)| json!({"check":check,"status":"passed","reason":reason,"error_class":null}).to_string()+"\n").collect()
}

fn plan() -> Plan {
    Plan::new(
        Namespace::new("fs-test-was01-unit").unwrap(),
        "fs-test-was01-unit",
        &format!("registry.invalid/was01@sha256:{}", "a".repeat(64)),
        1000770000,
    )
    .unwrap()
}

#[test]
fn manifest_refusals_happen_before_any_api_call() {
    let plan = plan();
    let baseline = plan.fixture()[1].clone();
    for (parent, field, bad) in [
        (
            "/spec/template/spec/securityContext",
            "runAsNonRoot",
            json!(false),
        ),
        ("/spec/template/spec/securityContext", "runAsUser", json!(0)),
        (
            "/spec/template/spec/securityContext",
            "seccompProfile",
            json!({"type":"Unconfined"}),
        ),
        (
            "/spec/template/spec/containers/0/securityContext",
            "readOnlyRootFilesystem",
            json!(false),
        ),
        (
            "/spec/template/spec/containers/0/securityContext",
            "allowPrivilegeEscalation",
            json!(true),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/capabilities",
            "drop",
            json!([]),
        ),
    ] {
        for value in [None, Some(bad)] {
            let mut object = baseline.clone();
            let map = object.pointer_mut(parent).unwrap().as_object_mut().unwrap();
            map.remove(field);
            if let Some(value) = value {
                map.insert(field.into(), value);
            }
            let fake = Fake::default();
            assert!(
                plan.apply(&fake, Phase::Fixture, Identity::Installer, &object)
                    .is_err(),
                "{field}"
            );
            assert!(fake.events.borrow().is_empty());
        }
    }
    for (parent, field, bad) in [
        ("/spec/template/spec", "runtimeClassName", json!("custom")),
        ("/spec/template/spec", "hostNetwork", json!(true)),
        ("/spec/template/spec", "hostPID", json!(true)),
        ("/spec/template/spec", "hostIPC", json!(true)),
        (
            "/spec/template/spec",
            "volumes",
            json!([{"name":"host","hostPath":{"path":"/"}}]),
        ),
        (
            "/spec/template/spec/containers/0/securityContext",
            "privileged",
            json!(true),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/capabilities",
            "add",
            json!(["NET_ADMIN"]),
        ),
        (
            "/spec/template/spec/containers/0/securityContext",
            "runAsUser",
            json!(65532),
        ),
        (
            "/spec/template/spec/containers/0/securityContext",
            "seccompProfile",
            json!({"type":"Unconfined"}),
        ),
        (
            "/spec/template/spec/containers/0",
            "image",
            json!("repo:latest"),
        ),
    ] {
        let mut object = baseline.clone();
        object
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(field.into(), bad);
        let fake = Fake::default();
        assert!(
            plan.apply(&fake, Phase::Fixture, Identity::Installer, &object)
                .is_err(),
            "{field}"
        );
        assert!(fake.events.borrow().is_empty());
    }
    for kind in [
        "ClusterRole",
        "ClusterRoleBinding",
        "RuntimeClass",
        "Namespace",
    ] {
        let mut object = baseline.clone();
        object["kind"] = json!(kind);
        let fake = Fake::default();
        assert!(
            plan.apply(&fake, Phase::Fixture, Identity::Installer, &object)
                .is_err()
        );
        assert!(fake.events.borrow().is_empty());
    }
    for name in [
        "in-falcone-staging",
        "kube-system",
        "default",
        "family-rentals-prod",
        "musematic-prod",
        "llmwiki-prod",
        "fs-test-foreign",
    ] {
        let mut object = baseline.clone();
        object["metadata"]["namespace"] = json!(name);
        let fake = Fake::default();
        assert!(
            plan.apply(&fake, Phase::Fixture, Identity::Installer, &object)
                .is_err()
        );
        assert!(fake.events.borrow().is_empty());
        if !name.starts_with("fs-test-") {
            assert!(Namespace::new(name).is_err());
        }
    }
    for (phase, identity, object) in [
        (Phase::Bootstrap, Identity::Installer, &plan.bootstrap()[0]),
        (Phase::Fixture, Identity::Admin, &baseline),
    ] {
        let fake = Fake::default();
        assert!(plan.apply(&fake, phase, identity, object).is_err());
        assert!(fake.events.borrow().is_empty());
    }
    let fake = Fake::default();
    for object in plan.fixture() {
        plan.apply(&fake, Phase::Fixture, Identity::Installer, object)
            .unwrap();
    }
    assert_eq!(fake.objects.borrow().len(), 2);
}

#[test]
fn five_phases_use_separate_identities_and_cleanup_even_on_phase_four_panic() {
    for mode in [
        support::NamespaceMode::Create,
        support::NamespaceMode::Shared(Namespace::new("fs-test-existing").unwrap()),
    ] {
        for panic_fixture in [false, true] {
            let fake = Fake {
                panic_fixture,
                ..Fake::default()
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                support::was01::execute(
                    &fake,
                    &mode,
                    &format!("registry.invalid/was01@sha256:{}", "a".repeat(64)),
                )
            }));
            assert_eq!(result.is_err(), panic_fixture);
            if !panic_fixture {
                result.unwrap().unwrap();
            }
            let events = fake.events.borrow();
            let start: Vec<&str> = events
                .iter()
                .take(if mode == support::NamespaceMode::Create {
                    7
                } else {
                    5
                })
                .map(String::as_str)
                .collect();
            if mode == support::NamespaceMode::Create {
                assert_eq!(
                    start,
                    [
                        "namespace SSAR",
                        "create namespace",
                        "get namespace",
                        "Admin ServiceAccount",
                        "Admin Role",
                        "Admin RoleBinding",
                        "TokenRequest"
                    ]
                );
                assert_eq!(events.last().unwrap(), "delete namespace");
            } else {
                assert_eq!(
                    start,
                    [
                        "get namespace",
                        "Admin ServiceAccount",
                        "Admin Role",
                        "Admin RoleBinding",
                        "TokenRequest"
                    ]
                );
                assert!(
                    !events
                        .iter()
                        .any(|e| e == "create namespace" || e == "delete namespace")
                );
                assert_eq!(events.last().unwrap(), "cleanup run");
                assert!(fake.objects.borrow().is_empty());
            }
            assert!(events.iter().any(|e| e == "Installer ServiceAccount"));
            assert!(!events.iter().any(|e| e == "Admin Deployment"));
        }
    }
}

#[test]
fn runner_evidence_requires_every_observed_outcome_and_the_chosen_effective_uid() {
    use support::was01::validate_evidence;
    let good = runner_records(1000770000);
    assert!(validate_evidence(&good, 1000770000).is_ok());
    for bad in [
        String::new(),
        good.lines().take(8).collect::<Vec<_>>().join("\n"),
        format!("{good}{good}"),
        good.replace("run returned 42", "run returned 41"),
        good.replace("observed epoch interruption", "unrelated trap"),
        good.replace("observed fuel exhaustion", "unrelated trap"),
        good.replace(
            "root mount is ro and write probe failed",
            "root mount is rw",
        ),
        good.replace("/tmp write probe succeeded", "/tmp denied"),
        good.replace("\"passed\"", "\"unverified\""),
        good.replace("\"passed\"", "\"failed\""),
        runner_records(65532),
    ] {
        assert!(validate_evidence(&bad, 1000770000).is_err());
    }
    let mut records: Vec<Value> = good
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    records[0]["unexpected"] = json!("credential-output-canary");
    let noisy = records
        .iter()
        .map(|r| r.to_string() + "\n")
        .collect::<String>();
    assert!(
        !format!("{:?}", validate_evidence(&noisy, 1000770000))
            .contains("credential-output-canary")
    );
}

#[test]
fn opt_in_and_prerequisite_denials_are_unverified_and_apply_no_fixture() {
    use support::was01::Settings;
    let image = format!("registry.invalid/was01@sha256:{}", "a".repeat(64));
    assert!(
        Settings::parse(None, None, None, None, None)
            .unwrap()
            .is_none()
    );
    for (config, image, mode, ns) in [
        (None, Some(image.as_str()), None, None),
        (Some("present"), None, None, None),
        (Some("present"), Some("repo:latest"), None, None),
        (Some("present"), Some(image.as_str()), Some("shared"), None),
        (
            Some("present"),
            Some(image.as_str()),
            Some("shared"),
            Some("default"),
        ),
        (
            Some("present"),
            Some(image.as_str()),
            Some("shared"),
            Some("in-falcone-staging"),
        ),
    ] {
        let fake = Fake::default();
        let result = Settings::parse(Some("1"), config, image, mode, ns)
            .and_then(|settings| settings.unwrap().execute(&fake));
        assert!(result.unwrap_err().contains("UNVERIFIED"));
        assert!(fake.events.borrow().is_empty());
    }
    for deny in ["namespace", "bootstrap", "token"] {
        let fake = Fake {
            deny: Some(deny),
            ..Fake::default()
        };
        let settings = Settings::parse(Some("1"), Some("present"), Some(&image), None, None)
            .unwrap()
            .unwrap();
        let error = settings.execute(&fake).unwrap_err();
        assert!(error.contains("UNVERIFIED"));
        let events = fake.events.borrow();
        assert!(!events.iter().any(|e| e.starts_with("Installer ")));
        if deny == "namespace" {
            assert!(error.contains("FS_TEST_NAMESPACE_MODE=shared"));
            assert_eq!(*events, ["namespace SSAR"]);
        } else {
            assert_eq!(events.last().unwrap(), "delete namespace");
        }
    }
    for shared in [
        Value::Null,
        json!({"metadata":{"name":"fs-test-existing"}}),
        json!({"metadata":{"name":"fs-test-existing","labels":{"pod-security.kubernetes.io/enforce":"baseline"}}}),
    ] {
        let fake = Fake {
            shared: Some(shared),
            ..Fake::default()
        };
        let settings = Settings::parse(
            Some("1"),
            Some("present"),
            Some(&image),
            Some("shared"),
            Some("fs-test-existing"),
        )
        .unwrap()
        .unwrap();
        assert!(settings.execute(&fake).unwrap_err().contains("UNVERIFIED"));
        assert_eq!(*fake.events.borrow(), ["get namespace"]);
        assert!(fake.objects.borrow().is_empty());
    }
}

#[test]
fn one_shot_startup_evidence_rejects_restarts_and_nonzero_exits() {
    use support::was01::first_run;
    let mut pod = json!({"metadata":{"name":"runner-pod"},"status":{
        "containerStatuses":[{"name":"runner","restartCount":0,"containerID":"containerd://first","state":{"terminated":{"exitCode":0}}}]
    }});
    assert_eq!(first_run(&pod).unwrap(), Some("containerd://first".into()));
    pod["status"]["containerStatuses"][0]["restartCount"] = json!(1);
    assert!(first_run(&pod).is_err());
    pod["status"]["containerStatuses"][0]["restartCount"] = json!(0);
    pod["status"]["containerStatuses"][0]["state"]["terminated"]["exitCode"] = json!(1);
    assert!(first_run(&pod).is_err());
    pod["status"]["containerStatuses"][0]["state"] = json!({"running":{}});
    assert_eq!(first_run(&pod).unwrap(), None);
    assert_eq!(first_run(&json!({"status":{}})).unwrap(), None);
}

#[test]
fn was01_cluster() -> Result<(), String> {
    let cluster = std::env::var("FS_TEST_CLUSTER").ok();
    let kubeconfig = std::env::var("KUBECONFIG").ok();
    let image = std::env::var("FS_TEST_WAS01_IMAGE").ok();
    let mode = std::env::var("FS_TEST_NAMESPACE_MODE").ok();
    let namespace = std::env::var("FS_TEST_NAMESPACE").ok();
    let settings = support::was01::Settings::parse(
        cluster.as_deref(),
        kubeconfig.as_deref(),
        image.as_deref(),
        mode.as_deref(),
        namespace.as_deref(),
    )?;
    let Some(settings) = settings else {
        eprintln!("UNVERIFIED: WAS-01 cluster qualification requires FS_TEST_CLUSTER=1");
        return Ok(());
    };
    let client = support::live::Live::new()?;
    for record in settings.execute(&client)? {
        println!("{record}");
    }
    Ok(())
}

#[test]
fn admission_cannot_widen_bootstrap_permissions_or_substitute_the_runner() {
    let plan = plan();
    for (phase, identity, object, path, replacement) in [
        (
            Phase::Bootstrap,
            Identity::Admin,
            &plan.bootstrap()[1],
            "/rules/0/verbs",
            json!(["*"]),
        ),
        (
            Phase::Bootstrap,
            Identity::Admin,
            &plan.bootstrap()[2],
            "/roleRef/name",
            json!("other-role"),
        ),
        (
            Phase::Fixture,
            Identity::Installer,
            &plan.fixture()[1],
            "/spec/template/spec/containers/0/image",
            json!(format!(
                "registry.invalid/substituted@sha256:{}",
                "b".repeat(64)
            )),
        ),
    ] {
        let fake = Fake {
            admission_override: Some((path, replacement)),
            ..Fake::default()
        };
        assert!(
            plan.apply(&fake, phase, identity, object).is_err(),
            "{path}"
        );
    }
}

#[test]
fn created_namespace_must_retain_restricted_admission_and_is_cleaned_on_rejection() {
    let fake = Fake {
        shared: Some(
            json!({"metadata":{"name":"ignored","labels":{"pod-security.kubernetes.io/enforce":"baseline"}}}),
        ),
        ..Fake::default()
    };
    let image = format!("registry.invalid/was01@sha256:{}", "a".repeat(64));
    assert!(support::was01::execute(&fake, &support::NamespaceMode::Create, &image).is_err());
    assert_eq!(
        *fake.events.borrow(),
        [
            "namespace SSAR",
            "create namespace",
            "get namespace",
            "delete namespace"
        ]
    );
    assert!(fake.objects.borrow().is_empty());
}

#[test]
fn actual_pod_must_preserve_the_admitted_fixture_and_run_identity() {
    let plan = plan();
    let pod = json!({"apiVersion":"v1","kind":"Pod", "metadata":{"name":"was01-generated-pod","namespace":plan.namespace.name(),"labels":{"falcone.test/run":plan.run}}, "spec":plan.fixture()[1]["spec"]["template"]["spec"]});
    assert!(plan.validate_pod(&pod).is_ok());
    for path in [
        "/spec/securityContext/runAsUser",
        "/metadata/namespace",
        "/spec/containers/0/image",
        "/spec/containers/0/securityContext/readOnlyRootFilesystem",
    ] {
        let mut changed = pod.clone();
        *changed.pointer_mut(path).unwrap() = Value::Null;
        assert!(plan.validate_pod(&changed).is_err(), "{path}");
    }
    let mut changed = pod.clone();
    changed["spec"]["initContainers"] = json!([pod["spec"]["containers"][0]]);
    assert!(plan.validate_pod(&changed).is_err());
}

#[test]
fn shared_mode_creates_only_suffixed_owned_objects_and_preserves_foreign_objects() {
    let foreign = json!({"kind":"ServiceAccount","metadata":{"name":"foreign","labels":{"falcone.test/run":"foreign-run"}}});
    let fake = Fake {
        objects: RefCell::new(vec![foreign.clone()]),
        ..Fake::default()
    };
    let image = format!("registry.invalid/was01@sha256:{}", "a".repeat(64));
    let records = support::was01::execute(
        &fake,
        &support::NamespaceMode::Shared(Namespace::new("fs-test-existing").unwrap()),
        &image,
    )
    .unwrap();
    let run = records[0]["run"].as_str().unwrap();
    assert_eq!(fake.created.borrow().len(), 5);
    for object in fake.created.borrow().iter() {
        assert_eq!(object["metadata"]["namespace"], "fs-test-existing");
        assert_eq!(object["metadata"]["labels"]["falcone.test/run"], run);
        assert!(
            object["metadata"]["name"]
                .as_str()
                .unwrap()
                .ends_with(&format!("-{run}"))
        );
    }
    assert_eq!(*fake.objects.borrow(), [foreign]);
    let mut foreign_namespace = plan().fixture()[1].clone();
    foreign_namespace["metadata"]["namespace"] = json!("fs-test-another");
    let shared_plan = Plan::new(
        Namespace::new("fs-test-existing").unwrap(),
        "fs-test-was01-unit",
        &image,
        1000770000,
    )
    .unwrap();
    let clean = Fake::default();
    assert!(
        shared_plan
            .apply(
                &clean,
                Phase::Fixture,
                Identity::Installer,
                &foreign_namespace
            )
            .is_err()
    );
    assert!(clean.created.borrow().is_empty());
}
