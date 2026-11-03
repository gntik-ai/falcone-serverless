//! WAS-02.t4 on the shared cluster, and the offline qualification of its harness.
//!
//! Operator evidence (never a CI gate): FS_TEST_CLUSTER=1 KUBECONFIG=<files>
//! FS_TEST_WAS02_IMAGE=<repo>@sha256:<digest> cargo test -p falcone-serverless-worker --test
//! cluster_capability_gates --locked, optionally with FS_TEST_NAMESPACE_MODE=shared and
//! FS_TEST_NAMESPACE=<existing fs-test-* namespace>. The image holds the trusted
//! `capability-gate-qualify` and `artifact-compile-child` binaries under /usr/local/bin. Without
//! FS_TEST_CLUSTER the live test reports "not run: FS_TEST_CLUSTER unset" and makes no client
//! call; with it, missing or invalid inputs fail closed. The credential is never printed.
//! SIGKILL can bypass cleanup.

#[path = "support/gate_mod.rs"]
mod support;

use falcone_serverless_worker::{Role, check};
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use kube::api::{LogParams, PostParams};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};
use support::cluster::Cluster;
use support::{
    Kubeconfigs, Namespace, NamespaceMode, NamespacePlan, RUN_LABEL, final_uid, pinned_image,
    test_namespace,
};

const CONTAINERFILE: &str = include_str!("../qualification/Containerfile");

const NOT_RUN: &str = "not run: FS_TEST_CLUSTER unset";
const BINARY: &str = "/usr/local/bin/capability-gate-qualify";
const SCENARIOS: [&str; 8] = [
    "wasi-imports-denied",
    "ungranted-interface-denied",
    "storage-scope",
    "events-workflows-scope",
    "http-allow-list",
    "dns-rebinding",
    "limits-deadlines",
    "compile-budget",
];
const POD_TIMEOUT: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(500);

// ---- the opt-in gate --------------------------------------------------------------------

#[derive(Debug)]
enum Gate {
    Disabled(&'static str),
    Enabled {
        kubeconfigs: Kubeconfigs,
        image: String,
        mode: NamespaceMode,
    },
}

/// Every input is a parameter, so the decision needs no process environment and no client.
/// Unset, empty, `0` and `false` leave the live suite off; any other value turns it on, and
/// then every missing or invalid input is an error (the suite fails closed).
fn gate(
    cluster: Option<&str>,
    kubeconfig: Option<&str>,
    image: Option<&str>,
    mode: Option<&str>,
    namespace: Option<&str>,
) -> Result<Gate, String> {
    if cluster.is_none_or(|value| matches!(value.trim(), "" | "0" | "false")) {
        return Ok(Gate::Disabled(NOT_RUN));
    }
    let image = image
        .filter(|image| pinned_image(image))
        .ok_or("UNVERIFIED: FS_TEST_WAS02_IMAGE must be <repo>@sha256:<digest>")?;
    let mode = NamespaceMode::parse(mode, namespace)?;
    let kubeconfigs = Kubeconfigs::parse(kubeconfig)?;
    Ok(Gate::Enabled {
        kubeconfigs,
        image: image.to_owned(),
        mode,
    })
}

// ---- the pod ----------------------------------------------------------------------------

/// An arbitrary non-root UID, as OpenShift restricted-v2 would assign.
fn arbitrary_uid() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    10_000 + RandomState::new().hash_one(std::process::id()) % 50_000
}

fn deployment(spec: &Value) -> Value {
    json!({"apiVersion": "apps/v1", "kind": "Deployment", "spec": {"template": {"spec": spec}}})
}

fn restricted(spec: &Value) -> Result<(), String> {
    let violations = check(&deployment(spec), Role::GuestWorker);
    if violations.is_empty() {
        return Ok(());
    }
    Err(format!(
        "refused posture: {}",
        violations
            .iter()
            .map(|v| v.rule_id)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The restricted qualification pod: seccomp RuntimeDefault, read-only root filesystem, an
/// arbitrary non-root UID, no capabilities and no service-account token.
fn pod_spec(image: &str, uid: u64) -> Result<Value, String> {
    if !pinned_image(image) {
        return Err("image must be <repo>@sha256:<64 lowercase hex>".into());
    }
    if uid == 0 {
        return Err("the pod must run as a non-root UID".into());
    }
    let spec = json!({
        "restartPolicy": "Never",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": {
            "runAsNonRoot": true, "runAsUser": uid,
            "seccompProfile": {"type": "RuntimeDefault"}
        },
        "containers": [{
            "name": "runner", "image": image, "command": [BINARY],
            "resources": {"limits": {"cpu": "2", "memory": "2Gi"}},
            "securityContext": {
                "allowPrivilegeEscalation": false,
                "readOnlyRootFilesystem": true,
                "capabilities": {"drop": ["ALL"]}
            }
        }]
    });
    restricted(&spec)?;
    Ok(spec)
}

/// The log must hold a PASS record for every scenario and nothing else for them.
fn verify_records(log: &str) -> Result<(), String> {
    let mut seen = BTreeMap::new();
    for line in log.lines() {
        let mut words = line.split_whitespace();
        if let (Some("WAS02"), Some(name), Some(verdict)) =
            (words.next(), words.next(), words.next())
        {
            let detail: String = words
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(300)
                .collect();
            seen.insert(name.to_owned(), (verdict.to_owned(), detail));
        }
    }
    for name in SCENARIOS {
        match seen.get(name) {
            Some((verdict, _)) if verdict == "PASS" => {}
            Some((verdict, detail)) => return Err(format!("{name}: {verdict} {detail}")),
            None => return Err(format!("{name}: no record in the pod log")),
        }
    }
    Ok(())
}

// ---- the cluster ------------------------------------------------------------------------

fn failure(error: &kube::Error) -> String {
    match error {
        kube::Error::Api(status) => format!("{}: {}", status.reason, status.message),
        _ => "request failed".to_owned(),
    }
}

/// Runs the pod to a terminal phase and returns the admitted pod and its log.
fn run_pod(cluster: &Cluster, namespace: &Namespace, pod: &Value) -> Result<(Pod, String), String> {
    let object: Pod = serde_json::from_value(pod.clone()).map_err(|_| "pod manifest is invalid")?;
    let name = object.metadata.name.clone().ok_or("pod has no name")?;
    let pods: Api<Pod> = Api::namespaced(cluster.admin().clone(), namespace.name());
    cluster
        .block(pods.create(&PostParams::default(), &object))
        .map_err(|error| format!("pod refused: {}", failure(&error)))?;
    let deadline = Instant::now() + POD_TIMEOUT;
    let admitted = loop {
        let current = cluster
            .block(pods.get(&name))
            .map_err(|error| failure(&error))?;
        let phase = current
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or_default();
        if matches!(phase.as_str(), "Succeeded" | "Failed") {
            break current;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pod did not finish within {POD_TIMEOUT:?} (phase {phase})"
            ));
        }
        std::thread::sleep(POLL);
    };
    let log = cluster
        .block(pods.logs(&name, &LogParams::default()))
        .map_err(|error| format!("cannot read the pod log: {}", failure(&error)))?;
    Ok((admitted, log))
}

fn pod_manifest(name: &str, namespace: &Namespace, run: &str, spec: &Value) -> Value {
    json!({"apiVersion": "v1", "kind": "Pod", "metadata": {
        "name": name, "namespace": namespace.name(), "labels": {RUN_LABEL: run}}, "spec": spec})
}

/// What the API server admitted must still be restricted, with no service-account token.
fn verify_admitted(admitted: &Pod, uid: u64) -> Result<(), String> {
    let spec = serde_json::to_value(admitted.spec.as_ref().ok_or("admitted pod has no spec")?)
        .map_err(|_| "admitted pod is unreadable")?;
    restricted(&spec)?;
    let context = &spec["securityContext"];
    if context["runAsUser"] != json!(uid)
        || context["runAsNonRoot"] != json!(true)
        || context["seccompProfile"]["type"] != json!("RuntimeDefault")
        || spec["automountServiceAccountToken"] != json!(false)
        || spec["containers"][0]["securityContext"]["readOnlyRootFilesystem"] != json!(true)
    {
        return Err("the admitted pod is not the restricted pod that was requested".into());
    }
    let token_volume = spec["volumes"].as_array().is_some_and(|volumes| {
        volumes.iter().any(|v| {
            v["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("kube-api-access"))
        })
    });
    if token_volume {
        return Err("the admitted pod mounts a service-account token".into());
    }
    Ok(())
}

fn live(kubeconfigs: &Kubeconfigs, image: &str, mode: &NamespaceMode) -> Result<(), String> {
    let (cluster, _) = Cluster::connect(kubeconfigs)?;
    let uid = arbitrary_uid();
    let spec = pod_spec(image, uid)?;
    // Created here, deleted on every exit path, including a panic (the guard's Drop).
    let guard = test_namespace("was02", &cluster, mode)?;
    let namespace = guard.namespace().clone();
    let run = guard.run().to_owned();
    let outcome = (|| {
        let name = format!("was02-{run}");
        let (admitted, log) = run_pod(
            &cluster,
            &namespace,
            &pod_manifest(&name, &namespace, &run, &spec),
        )?;
        verify_admitted(&admitted, uid)?;
        let phase = admitted
            .status
            .and_then(|status| status.phase)
            .unwrap_or_default();
        verify_records(&log)?;
        if phase != "Succeeded" {
            return Err(format!("the pod ended in phase {phase}"));
        }
        Ok(())
    })();
    let closed = guard.close();
    let gone = match mode {
        NamespaceMode::Create => match cluster.namespace_gone_or_terminating(&namespace) {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("{} was not deleted", namespace.name())),
            Err(error) => Err(error),
        },
        NamespaceMode::Shared(_) => Ok(()),
    };
    outcome.and(closed).and(gone)
}

// ---- offline qualification of the harness -----------------------------------------------

fn image() -> String {
    format!("registry.invalid/was02@sha256:{}", "b".repeat(64))
}

fn kubeconfig_file(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fs-test-was02-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("config");
    let text = json!({
        "apiVersion": "v1", "kind": "Config", "current-context": "ctx",
        "clusters": [{"name": "c", "cluster": {"server": "https://127.0.0.1:1"}}],
        "contexts": [{"name": "ctx", "context": {"cluster": "c", "user": "u"}}],
        "users": [{"name": "u", "user": {"token": "placeholder-not-a-credential"}}]
    });
    std::fs::write(&file, text.to_string()).unwrap();
    file
}

#[test]
fn without_fs_test_cluster_the_suite_reports_not_run_and_builds_no_client() {
    for cluster in [None, Some(""), Some("0"), Some("false")] {
        // Even with every other input present or invalid, nothing is read or contacted.
        for kubeconfig in [None, Some(":"), Some("/does/not/exist")] {
            match gate(
                cluster,
                kubeconfig,
                Some("not-an-image"),
                Some("bogus"),
                None,
            )
            .unwrap()
            {
                Gate::Disabled(reason) => assert_eq!(reason, "not run: FS_TEST_CLUSTER unset"),
                Gate::Enabled { .. } => panic!("must stay disabled for {cluster:?}"),
            }
        }
    }
}

#[test]
fn with_fs_test_cluster_set_missing_or_invalid_inputs_fail_closed() {
    let file = kubeconfig_file("closed");
    let path = file.to_str().unwrap();
    for (kubeconfig, image, mode, namespace) in [
        (None, Some(image()), None, None),
        (Some(path), None, None, None),
        (
            Some(path),
            Some("registry.invalid/was02:latest".to_owned()),
            None,
            None,
        ),
        (Some(path), Some(image()), Some("bogus"), None),
        (Some(path), Some(image()), Some("shared"), None),
        (
            Some(path),
            Some(image()),
            Some("shared"),
            Some("kube-system"),
        ),
        (Some("/does/not/exist"), Some(image()), None, None),
    ] {
        let error = gate(Some("1"), kubeconfig, image.as_deref(), mode, namespace).unwrap_err();
        assert!(!error.contains(path), "paths are not echoed: {error}");
    }
    assert!(matches!(
        gate(Some("1"), Some(path), Some(&image()), None, None).unwrap(),
        Gate::Enabled {
            mode: NamespaceMode::Create,
            ..
        }
    ));
    std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
}

#[test]
fn the_kubeconfig_boundary_lists_absolute_files_and_keeps_the_merge_private() {
    use std::os::unix::fs::PermissionsExt;
    let file = kubeconfig_file("boundary");
    let kubeconfigs = Kubeconfigs::parse(file.to_str()).unwrap();
    assert_eq!(kubeconfigs.paths(), std::slice::from_ref(&file));
    assert!(kubeconfigs.paths().iter().all(|path| path.is_absolute()));
    let flags = kubeconfigs.kubectl_flags().unwrap();
    let merged = std::path::PathBuf::from(flags[0].strip_prefix("--kubeconfig=").unwrap());
    assert_eq!(
        std::fs::metadata(&merged).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!format!("{kubeconfigs:?}").contains("placeholder-not-a-credential"));
    drop(kubeconfigs);
    assert!(
        !merged.exists(),
        "the private merged file is removed with its owner"
    );
    std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
}

#[test]
fn the_pod_is_restricted_with_an_arbitrary_non_root_uid_and_no_token() {
    let spec = pod_spec(&image(), 23_456).unwrap();
    assert_eq!(
        spec["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
    assert_eq!(spec["securityContext"]["runAsUser"], 23_456);
    assert_eq!(spec["automountServiceAccountToken"], false);
    assert_eq!(
        spec["containers"][0]["securityContext"]["readOnlyRootFilesystem"],
        true
    );
    assert_eq!(
        spec["containers"][0]["securityContext"]["capabilities"]["drop"][0],
        "ALL"
    );
    assert!(spec.get("hostNetwork").is_none() && spec.get("volumes").is_none());
    assert!(restricted(&spec).is_ok());
    assert!((10_000..60_000).contains(&arbitrary_uid()));
    // The posture checker refuses each regression of the restricted contract.
    for (pointer, value) in [
        ("/automountServiceAccountToken", json!(true)),
        ("/securityContext/runAsNonRoot", json!(false)),
        ("/securityContext/seccompProfile/type", json!("Unconfined")),
        (
            "/containers/0/securityContext/readOnlyRootFilesystem",
            json!(false),
        ),
        (
            "/containers/0/securityContext/allowPrivilegeEscalation",
            json!(true),
        ),
    ] {
        let mut weakened = spec.clone();
        *weakened.pointer_mut(pointer).unwrap() = value;
        assert!(restricted(&weakened).is_err(), "{pointer}");
    }
    assert!(pod_spec("registry.invalid/was02:latest", 23_456).is_err());
    assert!(pod_spec(&image(), 0).is_err());
}

#[test]
fn the_pod_log_must_hold_a_pass_record_for_every_scenario() {
    let good: String = SCENARIOS
        .iter()
        .map(|name| format!("WAS02 {name} PASS\n"))
        .collect();
    assert!(verify_records(&good).is_ok());
    let missing = good.replace("WAS02 http-allow-list PASS\n", "");
    assert!(
        verify_records(&missing)
            .unwrap_err()
            .starts_with("http-allow-list")
    );
    let failed = good.replace(
        "dns-rebinding PASS",
        "dns-rebinding FAIL resolutions: expected 2",
    );
    assert!(verify_records(&failed).unwrap_err().contains("expected 2"));
    let not_run = good.replace("compile-budget PASS", "compile-budget NOT_RUN no child");
    assert!(verify_records(&not_run).is_err());
    assert!(verify_records("").is_err());
}

#[test]
fn the_derived_image_runs_as_a_numeric_non_root_user_and_namespaces_are_fs_test() {
    // The operator image derives from this Containerfile; the pod overrides the UID anyway.
    assert!(final_uid(CONTAINERFILE).unwrap() > 0);
    let plan = NamespacePlan::new("was02", &NamespaceMode::Create).unwrap();
    assert!(plan.namespace().name().starts_with("fs-test-was02-"));
    assert!(!plan.run().is_empty() && plan.run().len() <= 16);
    let shared = NamespaceMode::parse(Some("shared"), Some("fs-test-existing")).unwrap();
    let plan = NamespacePlan::new("was02", &shared).unwrap();
    assert_eq!(plan.namespace().name(), "fs-test-existing");
    assert!(NamespaceMode::parse(Some("shared"), Some("in-falcone-staging")).is_err());
}

#[test]
fn pod_manifests_carry_the_run_label_the_guard_cleans_up_by() {
    let namespace = Namespace::new("fs-test-was02-unit").unwrap();
    let manifest = pod_manifest(
        "was02-run",
        &namespace,
        "run1",
        &pod_spec(&image(), 20_000).unwrap(),
    );
    assert_eq!(manifest["metadata"]["labels"][RUN_LABEL], "run1");
    assert_eq!(manifest["metadata"]["namespace"], "fs-test-was02-unit");
}

/// The live suite: only with FS_TEST_CLUSTER set. Unset, it reports why it did not run.
#[test]
fn cluster_capability_gates() -> Result<(), String> {
    let env = |name: &str| std::env::var(name).ok();
    match gate(
        env("FS_TEST_CLUSTER").as_deref(),
        env("KUBECONFIG").as_deref(),
        env("FS_TEST_WAS02_IMAGE").as_deref(),
        env("FS_TEST_NAMESPACE_MODE").as_deref(),
        env("FS_TEST_NAMESPACE").as_deref(),
    )? {
        Gate::Disabled(reason) => {
            // The real stderr handle, so the libtest capture does not hide the reason.
            let _ = writeln!(std::io::stderr(), "{reason}");
            Ok(())
        }
        Gate::Enabled {
            kubeconfigs,
            image,
            mode,
        } => live(&kubeconfigs, &image, &mode),
    }
}
