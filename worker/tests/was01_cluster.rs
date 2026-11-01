//! WAS-01 ac1/ac2: offline contract tests and an explicitly opted-in cluster run.
use serde_json::Value;
use std::{fs, path::PathBuf};

#[path = "support/kubernetes.rs"]
mod kubernetes;
#[path = "support/namespace.rs"]
mod namespace;
#[path = "support/observation.rs"]
mod observation;
#[path = "support/test_namespace.rs"]
mod test_namespace;
#[path = "support/was01.rs"]
mod was01;

use namespace::{Namespace, NamespaceMode};
use std::cell::RefCell;
use test_namespace::{NamespaceApi, test_namespace};
use was01::{Identity, Phase, Plan};

fn image() -> String {
    format!("registry.invalid/qualification@sha256:{}", "a".repeat(64))
}

fn plan() -> Plan {
    Plan::new("fs-test-unit", "abcdef123456", 1000770001, &image()).unwrap()
}

fn pod(ready: Option<&str>, exit: Option<i32>, restarts: i32) -> k8s_openapi::api::core::v1::Pod {
    let conditions = ready.map(|status| vec![serde_json::json!({"type":"Ready", "status":status, "lastTransitionTime":"2026-10-30T12:00:00Z"})]).unwrap_or_default();
    serde_json::from_value(serde_json::json!({"metadata":{"name":"fixture-pod", "uid":"fixture-pod-uid", "resourceVersion":"2"}, "status":{"conditions":conditions, "containerStatuses":[{"name":"runner", "image":"fixture", "imageID":"fixture", "ready":ready == Some("True"), "restartCount":restarts, "state":exit.map(|code| serde_json::json!({"terminated":{"exitCode":code}})).unwrap_or_else(|| serde_json::json!({"running":{}}))}]}})).unwrap()
}

fn runner_log() -> String {
    [
        ("engine_config", "engine succeeded"), ("compile_valid", "compile succeeded"),
        ("load_executable_memory", "load succeeded"), ("run_valid", "run returned 42"),
        ("interrupt_epoch", "observed epoch interruption"), ("interrupt_fuel", "observed fuel exhaustion"),
        ("root_fs_read_only", "root mount is ro and write probe failed"),
        ("tmp_writable", "/tmp random payload readback verified"),
        ("non_root_uid", "uid=1000770001 gid=65532 (effective); uid set=[1000770001, 1000770001, 1000770001, 1000770001], gid set=[65532, 65532, 65532, 65532]"),
    ].iter().map(|(check, reason)| serde_json::json!({"check":check,"status":"passed","reason":reason,"error_class":null}).to_string()+"\n").collect()
}

fn good_observation() -> observation::Observation {
    let mut observation = observation::Observation::default();
    observation.record(&pod(Some("True"), None, 0)).unwrap();
    observation.record(&pod(Some("False"), Some(0), 0)).unwrap();
    observation.logs = runner_log();
    observation
}

#[test]
fn qualification_requires_ready_and_verified_readback() {
    was01::qualify(&good_observation(), 1000770001).unwrap();
    for ready in [None, Some("False")] {
        let mut observation = observation::Observation::default();
        observation.record(&pod(ready, Some(0), 0)).unwrap();
        observation.logs = runner_log();
        assert!(
            was01::qualify(&observation, 1000770001)
                .unwrap_err()
                .contains("Ready")
        );
    }
    let mut observation = good_observation();
    assert_eq!(observation.conditions[0].status, "True");
    assert!(observation.conditions[0].last_transition_time.is_some());
    observation.logs = runner_log().replace(
        "/tmp random payload readback verified",
        "/tmp write probe succeeded",
    );
    assert!(
        was01::qualify(&observation, 1000770001)
            .unwrap_err()
            .contains("readback")
    );
    assert!(was01::qualify(&good_observation(), 1000770002).is_err());
}

#[test]
fn fixture_refusals_cover_posture_namespace_kind_digest_and_identity() {
    let plan = plan();
    for object in plan.fixture() {
        plan.validate(Phase::Fixture, Identity::Installer, &object)
            .unwrap();
    }
    let deployment = plan.fixture().pop().unwrap();
    for (path, bad) in [
        (
            "/spec/template/spec/securityContext/runAsNonRoot",
            serde_json::json!(false),
        ),
        (
            "/spec/template/spec/securityContext/runAsUser",
            serde_json::json!(65532),
        ),
        (
            "/spec/template/spec/securityContext/seccompProfile",
            serde_json::json!({"type":"Unconfined"}),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/readOnlyRootFilesystem",
            serde_json::json!(false),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/allowPrivilegeEscalation",
            serde_json::json!(true),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/capabilities/drop",
            serde_json::json!([]),
        ),
    ] {
        let mut object = deployment.clone();
        *object.pointer_mut(path).unwrap() = bad;
        assert!(
            plan.validate(Phase::Fixture, Identity::Installer, &object)
                .is_err(),
            "{path}"
        );
        let (parent, field) = path.rsplit_once('/').unwrap();
        object
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            plan.validate(Phase::Fixture, Identity::Installer, &object)
                .is_err(),
            "missing {path}"
        );
    }
    for (path, value) in [
        ("runtimeClassName", serde_json::json!("custom")),
        ("hostNetwork", serde_json::json!(true)),
        ("hostPID", serde_json::json!(true)),
        ("hostIPC", serde_json::json!(true)),
        (
            "volumes",
            serde_json::json!([{"name":"host", "hostPath":{"path":"/"}}]),
        ),
    ] {
        let mut object = deployment.clone();
        object["spec"]["template"]["spec"][path] = value;
        assert!(
            plan.validate(Phase::Fixture, Identity::Installer, &object)
                .is_err()
        );
    }
    for (field, value) in [
        ("privileged", serde_json::json!(true)),
        (
            "capabilities",
            serde_json::json!({"drop":["ALL"], "add":["NET_ADMIN"]}),
        ),
    ] {
        let mut object = deployment.clone();
        object["spec"]["template"]["spec"]["containers"][0]["securityContext"][field] = value;
        assert!(
            plan.validate(Phase::Fixture, Identity::Installer, &object)
                .is_err()
        );
    }
    for kind in [
        "ClusterRole",
        "ClusterRoleBinding",
        "RuntimeClass",
        "Namespace",
    ] {
        let mut object = deployment.clone();
        object["kind"] = kind.into();
        assert!(
            plan.validate(Phase::Fixture, Identity::Installer, &object)
                .is_err()
        );
    }
    for namespace in ["fs-test-other", "in-falcone-staging", "kube-system"] {
        let mut object = deployment.clone();
        object["metadata"]["namespace"] = namespace.into();
        assert!(
            plan.validate(Phase::Fixture, Identity::Installer, &object)
                .is_err()
        );
    }
    assert!(
        plan.validate(Phase::Fixture, Identity::Admin, &deployment)
            .is_err()
    );
    for object in plan.bootstrap() {
        plan.validate(Phase::Bootstrap, Identity::Admin, &object)
            .unwrap();
        assert!(
            plan.validate(Phase::Bootstrap, Identity::Installer, &object)
                .is_err()
        );
    }
    assert!(
        Plan::new(
            "fs-test-unit",
            "run",
            123,
            "registry.invalid/qualification:latest"
        )
        .is_err()
    );
    for _ in 0..1000 {
        let uid = was01::choose_uid();
        assert!(uid > 0 && uid != 65532);
    }
}

#[derive(Default)]
struct Fake {
    calls: RefCell<Vec<String>>,
    namespace: RefCell<Option<Value>>,
    objects: RefCell<Vec<Value>>,
    denied: bool,
    fail: Option<&'static str>,
    panic_fixture: bool,
}

impl was01::ClusterApi for Fake {
    fn create(&self, identity: Identity, object: &Value) -> Result<Value, String> {
        let kind = object["kind"].as_str().unwrap();
        self.calls
            .borrow_mut()
            .push(format!("{identity:?} create {kind}"));
        if identity == Identity::Installer && self.panic_fixture {
            panic!("phase 4 panic");
        }
        if self.fail == Some("bootstrap") && identity == Identity::Admin {
            return Err("bootstrap forbidden".into());
        }
        self.objects.borrow_mut().push(object.clone());
        let mut admitted = object.clone();
        admitted["metadata"]["uid"] = "admission-uid".into();
        Ok(admitted)
    }
    fn installer_token(&self, _: &Namespace, _: &str) -> Result<(), String> {
        self.calls.borrow_mut().push("token request".into());
        if self.fail == Some("token") {
            return Err("TokenRequest forbidden".into());
        }
        Ok(())
    }
    fn installer_can_create(&self, _: &str, resource: &str) -> Result<bool, String> {
        self.calls
            .borrow_mut()
            .push(format!("Installer review {resource}"));
        Ok(false)
    }
    fn begin_observation(&self, _: &Namespace, _: &str) -> Result<String, String> {
        self.calls.borrow_mut().push("watch baseline".into());
        Ok("1".into())
    }
    fn observe(&self, _: &Namespace, _: &str, _: &str) -> Result<observation::Observation, String> {
        self.calls.borrow_mut().push("observe".into());
        let objects = self.objects.borrow();
        let deployment = objects
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        let uid = deployment["spec"]["template"]["spec"]["securityContext"]["runAsUser"]
            .as_u64()
            .unwrap();
        let mut observation = good_observation();
        observation.logs = observation.logs.replace("1000770001", &uid.to_string());
        Ok(observation)
    }
}

#[test]
fn five_phases_keep_bootstrap_and_fixture_identities_separate() {
    let fake = Fake::default();
    let report = was01::run(&fake, &image(), &NamespaceMode::Create).unwrap();
    assert_eq!(report.admissions.len(), 5);
    assert_eq!(
        *fake.calls.borrow(),
        [
            "review namespace",
            "create namespace",
            "Admin create ServiceAccount",
            "Admin create Role",
            "Admin create RoleBinding",
            "token request",
            "Installer review clusterroles",
            "Installer review clusterrolebindings",
            "Installer review runtimeclasses",
            "Installer review namespaces",
            "watch baseline",
            "Installer create ServiceAccount",
            "Installer create Deployment",
            "observe",
            "delete namespace"
        ]
    );
    assert!(
        report
            .admissions
            .iter()
            .all(|object| object["metadata"]["uid"] == "admission-uid")
    );
    let objects = fake.objects.borrow();
    for object in objects.iter() {
        let run = object["metadata"]["labels"]["falcone.test/run"]
            .as_str()
            .unwrap();
        assert!(
            object["metadata"]["name"]
                .as_str()
                .unwrap()
                .ends_with(&format!("-{run}"))
        );
    }
}

#[test]
fn forbidden_setup_and_phase_four_panic_always_cleanup() {
    for fail in ["bootstrap", "token"] {
        let fake = Fake {
            fail: Some(fail),
            ..Fake::default()
        };
        assert!(
            was01::run(&fake, &image(), &NamespaceMode::Create)
                .err()
                .unwrap()
                .contains("UNVERIFIED")
        );
        assert!(
            !fake
                .calls
                .borrow()
                .iter()
                .any(|call| call.starts_with("Installer create"))
        );
        assert_eq!(fake.calls.borrow().last().unwrap(), "delete namespace");
    }
    let fake = Fake {
        panic_fixture: true,
        ..Fake::default()
    };
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| was01::run(
            &fake,
            &image(),
            &NamespaceMode::Create
        )))
        .is_err()
    );
    assert_eq!(fake.calls.borrow().last().unwrap(), "delete namespace");
}

#[test]
fn opt_in_configuration_fails_closed_before_any_client_is_needed() {
    assert_eq!(
        was01::Config::parse(None, None, None, None, None).unwrap(),
        was01::Config::Disabled
    );
    for (kubeconfig, image) in [
        (None, Some(image())),
        (Some("ambient-path"), None),
        (Some(""), Some(image())),
    ] {
        assert!(
            was01::Config::parse(Some("1"), kubeconfig, image.as_deref(), None, None)
                .unwrap_err()
                .contains("UNVERIFIED")
        );
    }
    assert!(
        was01::Config::parse(
            Some("1"),
            Some("ambient-path"),
            Some(&image()),
            Some("shared"),
            None
        )
        .is_err()
    );
}

#[test]
fn was01_cluster() -> Result<(), String> {
    let cluster = std::env::var("FS_TEST_CLUSTER").ok();
    let kubeconfig = std::env::var("KUBECONFIG").ok();
    let image = std::env::var("FS_TEST_WAS01_IMAGE").ok();
    let mode = std::env::var("FS_TEST_NAMESPACE_MODE").ok();
    let namespace = std::env::var("FS_TEST_NAMESPACE").ok();
    let config = was01::Config::parse(
        cluster.as_deref(),
        kubeconfig.as_deref(),
        image.as_deref(),
        mode.as_deref(),
        namespace.as_deref(),
    )?;
    let was01::Config::Enabled { image, mode } = config else {
        println!("UNVERIFIED: WAS-01 cluster qualification requires FS_TEST_CLUSTER=1");
        return Ok(());
    };
    let client = kubernetes::Clients::new()?;
    let report = was01::run(&client, &image, &mode)?;
    println!(
        "{}",
        serde_json::json!({"admission_responses":report.admissions, "ready_conditions":report.observation.conditions, "runner":report.observation.logs, "installer_create_denied":report.denied, "cleanup":"complete"})
    );
    Ok(())
}

impl NamespaceApi for Fake {
    fn can_create_namespace(&self) -> Result<bool, String> {
        self.calls.borrow_mut().push("review namespace".into());
        Ok(!self.denied)
    }
    fn get_namespace(&self, _: &Namespace) -> Result<Option<Value>, String> {
        self.calls.borrow_mut().push("get namespace".into());
        Ok(self.namespace.borrow().clone())
    }
    fn create_namespace(&self, object: &Value) -> Result<(), String> {
        self.calls.borrow_mut().push("create namespace".into());
        *self.namespace.borrow_mut() = Some(object.clone());
        if self.fail == Some("namespace") {
            return Err("namespace response lost".into());
        }
        Ok(())
    }
    fn delete_namespace(&self, _: &Namespace, _: &str) -> Result<(), String> {
        self.calls.borrow_mut().push("delete namespace".into());
        Ok(())
    }
    fn get_object(&self, _: &Namespace, object: &Value) -> Result<Option<Value>, String> {
        Ok(self
            .objects
            .borrow()
            .iter()
            .find(|found| {
                found["kind"] == object["kind"]
                    && found["metadata"]["name"] == object["metadata"]["name"]
            })
            .cloned())
    }
    fn delete_object(&self, _: &Namespace, object: &Value) -> Result<(), String> {
        self.calls.borrow_mut().push(format!(
            "delete {}",
            object["metadata"]["name"].as_str().unwrap()
        ));
        Ok(())
    }
}

#[test]
fn namespace_create_is_reviewed_before_mutation_and_guarded() {
    let fake = Fake::default();
    let guard = test_namespace(&fake, "was01", &NamespaceMode::Create).unwrap();
    assert!(guard.namespace.name().starts_with("fs-test-was01-"));
    assert_eq!(
        *fake.calls.borrow(),
        ["review namespace", "create namespace"]
    );
    guard.close().unwrap();
    assert_eq!(fake.calls.borrow().last().unwrap(), "delete namespace");
    let fake = Fake {
        denied: true,
        ..Fake::default()
    };
    let error = test_namespace(&fake, "was01", &NamespaceMode::Create)
        .err()
        .unwrap();
    assert!(error.contains("UNVERIFIED"));
    assert!(error.contains("FS_TEST_NAMESPACE_MODE=shared"));
    assert_eq!(*fake.calls.borrow(), ["review namespace"]);
}

#[test]
fn namespace_guard_cleans_up_even_if_create_response_is_lost() {
    let fake = Fake {
        fail: Some("namespace"),
        ..Fake::default()
    };
    assert!(test_namespace(&fake, "was01", &NamespaceMode::Create).is_err());
    assert_eq!(
        *fake.calls.borrow(),
        ["review namespace", "create namespace", "delete namespace"]
    );
}

#[test]
fn shared_namespace_is_verified_without_any_mutation() {
    assert!(NamespaceMode::parse(Some("shared"), None).is_err());
    for forbidden in [
        "in-falcone-staging",
        "kube-system",
        "default",
        "family-rentals-prod",
        "musematic",
        "llmwiki",
    ] {
        assert!(NamespaceMode::parse(Some("shared"), Some(forbidden)).is_err());
    }
    assert_eq!(
        NamespaceMode::parse(None, None).unwrap(),
        NamespaceMode::Create
    );
    let mode = NamespaceMode::parse(Some("shared"), Some("fs-test-shared")).unwrap();
    for object in [
        None,
        Some(serde_json::json!({"metadata":{"name":"fs-test-shared"}})),
        Some(
            serde_json::json!({"metadata":{"name":"fs-test-shared", "labels":{"pod-security.kubernetes.io/enforce":"baseline"}}}),
        ),
    ] {
        let fake = Fake {
            namespace: RefCell::new(object),
            ..Fake::default()
        };
        assert!(
            test_namespace(&fake, "was01", &mode)
                .err()
                .unwrap()
                .contains("UNVERIFIED")
        );
        assert_eq!(*fake.calls.borrow(), ["get namespace"]);
    }
    let fake = Fake {
        namespace: RefCell::new(Some(Namespace::new("fs-test-shared").unwrap().manifest())),
        ..Fake::default()
    };
    test_namespace(&fake, "was01", &mode)
        .unwrap()
        .close()
        .unwrap();
    assert_eq!(*fake.calls.borrow(), ["get namespace"]);
}

#[test]
fn shared_cleanup_rechecks_run_ownership_and_survives_unwinding() {
    let fake = Fake {
        namespace: RefCell::new(Some(Namespace::new("fs-test-shared").unwrap().manifest())),
        ..Fake::default()
    };
    let mode = NamespaceMode::Shared(Namespace::new("fs-test-shared").unwrap());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut guard = test_namespace(&fake, "was01", &mode).unwrap();
        for name in ["ours", "relabelled", "foreign"] {
            let mut object = serde_json::json!({"kind":"ServiceAccount", "metadata":{"name":format!("{name}-{}", guard.run),"namespace":"fs-test-shared", "labels":{"falcone.test/run":guard.run}}});
            guard.track(&object).unwrap();
            if name != "ours" {
                object["metadata"]["labels"]["falcone.test/run"] = "someone-else".into();
            }
            fake.objects.borrow_mut().push(object);
        }
        panic!("phase 4 panic");
    }));
    assert!(result.is_err());
    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], "get namespace");
    assert!(calls[1].starts_with("delete ours-"));
}

#[test]
fn shared_flow_creates_and_cleans_only_suffixed_run_objects() {
    let fake = Fake {
        namespace: RefCell::new(Some(Namespace::new("fs-test-shared").unwrap().manifest())),
        ..Fake::default()
    };
    fake.objects.borrow_mut().push(serde_json::json!({"kind":"ServiceAccount", "metadata":{"name":"foreign", "namespace":"fs-test-shared", "labels":{"falcone.test/run":"another-run"}}}));
    let mode = NamespaceMode::Shared(Namespace::new("fs-test-shared").unwrap());
    let report = was01::run(&fake, &image(), &mode).unwrap();
    assert_eq!(
        report.denied,
        [
            "clusterroles",
            "clusterrolebindings",
            "runtimeclasses",
            "namespaces"
        ]
    );
    let calls = fake.calls.borrow();
    assert!(!calls.iter().any(|call| call == "create namespace"
        || call == "delete namespace"
        || call == "review namespace"));
    let deleted: Vec<_> = calls
        .iter()
        .filter_map(|call| call.strip_prefix("delete "))
        .collect();
    let names: Vec<_> = report
        .admissions
        .iter()
        .rev()
        .map(|object| {
            assert_eq!(object["metadata"]["namespace"], "fs-test-shared");
            let run = object["metadata"]["labels"]["falcone.test/run"]
                .as_str()
                .unwrap();
            let name = object["metadata"]["name"].as_str().unwrap();
            assert!(name.ends_with(&format!("-{run}")));
            name
        })
        .collect();
    assert_eq!(deleted, names);
    assert!(!deleted.contains(&"foreign"));
}

#[test]
fn rejected_objects_never_reach_the_api_boundary() {
    let fake = Fake::default();
    let mut guard = test_namespace(&fake, "was01", &NamespaceMode::Create).unwrap();
    let plan = Plan::new(guard.namespace.name(), &guard.run, 1000770001, &image()).unwrap();
    let baseline = plan.fixture().pop().unwrap();
    let calls = fake.calls.borrow().len();
    let own_namespace = guard.namespace.name().to_owned();
    for (identity, kind, namespace) in [
        (Identity::Admin, "Deployment", own_namespace.as_str()),
        (Identity::Installer, "ClusterRole", own_namespace.as_str()),
        (Identity::Installer, "Deployment", "kube-system"),
    ] {
        let mut object = baseline.clone();
        object["kind"] = kind.into();
        object["metadata"]["namespace"] = namespace.into();
        assert!(
            was01::apply_checked(&fake, &mut guard, &plan, Phase::Fixture, identity, &object)
                .is_err()
        );
    }
    assert_eq!(fake.calls.borrow().len(), calls);
    guard.close().unwrap();
}

#[test]
fn observer_rejects_restarts_replacement_and_failed_exit() {
    {
        let mut observation = observation::Observation::default();
        assert!(observation.record(&pod(Some("True"), Some(0), 1)).is_err());
    }
    let mut observation = good_observation();
    let mut replacement = pod(Some("True"), Some(0), 0);
    replacement.metadata.uid = Some("replacement-uid".into());
    assert!(observation.record(&replacement).is_err());
    for logs in [
        runner_log().replace("\"passed\"", "\"failed\""),
        String::new(),
        format!("{}{}", runner_log(), runner_log()),
        "not json".into(),
    ] {
        let mut observation = good_observation();
        observation.logs = logs;
        assert!(was01::qualify(&observation, 1000770001).is_err());
    }
}

#[test]
fn failed_runner_exit_retains_its_diagnostic_evidence() {
    let mut observation = observation::Observation::default();
    observation.record(&pod(Some("True"), Some(1), 0)).unwrap();
    observation.logs = runner_log().replace(
        "\"reason\":\"compile succeeded\",\"status\":\"passed\"",
        "\"reason\":\"executable compilation denied\",\"status\":\"failed\"",
    );
    let error = was01::qualify(&observation, 1000770001).unwrap_err();
    assert!(
        error.contains("compile_valid") && error.contains("executable compilation denied"),
        "{error}"
    );
}

fn qualification_file(path: &str) -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("qualification")
            .join(path),
    )
    .unwrap()
}

#[test]
fn committed_image_and_bootstrap_have_bounded_authority() {
    let file = qualification_file("Containerfile");
    for line in file.lines().filter(|line| line.starts_with("FROM ")) {
        assert!(line.contains("@sha256:"));
    }
    assert!(file.contains("--locked"));
    assert!(file.contains("USER 65532:65532"));
    assert!(file.contains("ENTRYPOINT [\"/usr/local/bin/qualification_runner\"]"));
    assert!(file.contains("--chmod=0555"));
    let role: Value =
        serde_json::from_str(&qualification_file("manifests/bootstrap/role.json")).unwrap();
    assert_eq!(
        role["rules"],
        serde_json::json!([
            {"apiGroups":[""],"resources":["serviceaccounts"],"verbs":["create","get","delete"]},
            {"apiGroups":["apps"],"resources":["deployments"],"verbs":["create","get","delete"]},
            {"apiGroups":[""],"resources":["pods"],"verbs":["get","list","watch"]},
            {"apiGroups":[""],"resources":["pods/log"],"verbs":["get"]}
        ])
    );
}
