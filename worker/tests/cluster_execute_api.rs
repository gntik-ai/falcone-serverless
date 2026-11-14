//! Operator-only restricted qualification of the worker execution API (RUN-01.t3), in
//! `fs-test-run01-*` namespaces that the test creates and deletes (or the shared namespace of
//! `FS_TEST_NAMESPACE_MODE=shared`), under Pod Security restricted with seccomp
//! `RuntimeDefault`, a read-only root filesystem and an arbitrary non-root UID.
//!
//! Two pods, both gated by `FS_TEST_CLUSTER` and skipped with an explicit message without it:
//!
//! - `cluster_execute_api` runs every case of `execute_api_cases.rs` in the qualification
//!   image (`FS_TEST_WAS02_IMAGE`): the worker's server in process, with test hooks.
//! - `cluster_execute_api_production_image` runs the production worker image
//!   (`FS_TEST_WORKER_IMAGE`) unmodified as a sidecar and drives it over the pod's loopback
//!   with the cases that need only the network (`execute_api_production.rs`).
#![cfg(target_os = "linux")]

#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

const QUALIFICATION_PREFIX: &str = "run01";
const QUALIFICATION_FLAG: &str = "--execute-api";
const QUALIFICATION_CHECK: &str = "execute_api";
const CLUSTER_TEST: &str = "cluster_execute_api";
const SKIP_EXPLICIT_REQUEST_WITHOUT_GATE: bool = true;

include!("support/was02_cluster.rs");

const RUNNER: &str = "/usr/local/bin/execute-api-qualification";
const WORKER: &str = "/usr/local/bin/falcone-serverless-worker";
const WORKER_IMAGE_VARIABLE: &str = "FS_TEST_WORKER_IMAGE";
const PRODUCTION_CHECK: &str = "execute_api_production";

fn restricted_container(name: &str, image: &str, command: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "name": name, "image": image, "command": command,
        "securityContext": {
            "allowPrivilegeEscalation": false,
            "readOnlyRootFilesystem": true,
            "capabilities": {"drop": ["ALL"]}
        },
        "resources": {
            "requests": {"cpu": "1", "memory": "512Mi"},
            "limits": {"cpu": "2", "memory": "5Gi"}
        }
    })
}

/// The in-process pod: the qualification image's runner, for one case or all of them.
fn qualification_manifest(
    plan: &NamespacePlan,
    image: &str,
    case: Option<&str>,
) -> Result<serde_json::Value, String> {
    let mut pod = pod_manifest(plan, image)?;
    let flag = match case {
        Some(case) => format!("{QUALIFICATION_FLAG}={case}"),
        None => QUALIFICATION_FLAG.to_owned(),
    };
    pod["spec"]["containers"][0]["command"] = serde_json::json!([RUNNER, flag]);
    Ok(pod)
}

/// The production-image pod: an init container prepares the worker's configuration and
/// artifacts, the unmodified production worker runs as a sidecar, and the runner container
/// drives it over the pod's loopback. The pod succeeds when the runner exits.
fn production_manifest(
    plan: &NamespacePlan,
    runner_image: &str,
    worker_image: &str,
) -> Result<serde_json::Value, String> {
    for image in [runner_image, worker_image] {
        if !image::pinned_image(image) {
            return Err("UNVERIFIED: both images must be <repo>@sha256:<digest>".into());
        }
    }
    let mut pod = pod_manifest(plan, runner_image)?;
    let shared = serde_json::json!({"name": "shared", "mountPath": "/shared"});
    let scratch = serde_json::json!({"name": "scratch", "mountPath": "/tmp"});
    let mut prepare = restricted_container(
        "prepare",
        runner_image,
        serde_json::json!([RUNNER, "--prepare", "/shared"]),
    );
    prepare["volumeMounts"] = serde_json::json!([shared]);
    let mut worker = restricted_container(
        "worker",
        worker_image,
        serde_json::json!([WORKER, "serve", "/shared/worker.toml"]),
    );
    worker["restartPolicy"] = "Always".into();
    worker["volumeMounts"] = serde_json::json!([
        {"name": "shared", "mountPath": "/shared", "readOnly": true},
        scratch
    ]);
    let mut runner = restricted_container(
        "runner",
        runner_image,
        serde_json::json!([RUNNER, "--production-target"]),
    );
    runner["volumeMounts"] = serde_json::json!([scratch]);
    pod["spec"]["initContainers"] = serde_json::json!([prepare, worker]);
    pod["spec"]["containers"] = serde_json::json!([runner]);
    pod["spec"]["volumes"] = serde_json::json!([
        {"name": "shared", "emptyDir": {"sizeLimit": "64Mi"}},
        {"name": "scratch", "emptyDir": {"sizeLimit": "256Mi"}}
    ]);
    Ok(pod)
}

impl RealCluster {
    /// Runs the production pod to completion and reads the runner container's records.
    fn run_production_pod(
        &self,
        ns: &namespace::Namespace,
        pod: &serde_json::Value,
    ) -> Result<(), String> {
        use kube::api::{LogParams, PostParams};
        let pods = self.pods(ns);
        let object = serde_json::from_value(pod.clone()).map_err(|_| "invalid pod manifest")?;
        // Create only; a name collision never authorizes update, patch or deletion.
        self.runtime
            .block_on(pods.create(&PostParams::default(), &object))
            .map_err(api_error)?;
        let name = pod["metadata"]["name"].as_str().ok_or("pod name missing")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1500);
        loop {
            let observed = self.runtime.block_on(pods.get(name)).map_err(api_error)?;
            let phase = observed
                .status
                .and_then(|status| status.phase)
                .unwrap_or_default();
            let logs = || {
                let params = LogParams {
                    container: Some("runner".into()),
                    ..LogParams::default()
                };
                self.runtime
                    .block_on(pods.logs(name, &params))
                    .map_err(api_error)
            };
            match phase.as_str() {
                "Failed" => return Err("execution API production qualification pod failed".into()),
                "Succeeded" => {
                    let passed = serde_json::json!({"check": PRODUCTION_CHECK, "status": "passed"});
                    return if logs()?.lines().any(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .is_ok_and(|record| record == passed)
                    }) {
                        Ok(())
                    } else {
                        Err("execution API production qualification success record missing".into())
                    };
                }
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err("execution API production qualification observation deadline".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

fn run_pod_of(
    build: impl FnOnce(&NamespacePlan, &str) -> Result<serde_json::Value, String>,
    production: bool,
) -> Result<(), String> {
    let image = std::env::var("FS_TEST_WAS02_IMAGE").ok();
    let Some((image, mode, configs)) = validated_inputs(
        std::env::var("FS_TEST_CLUSTER").ok().as_deref(),
        std::env::var("KUBECONFIG").ok().as_deref(),
        image.as_deref(),
        std::env::var("FS_TEST_NAMESPACE_MODE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE").ok().as_deref(),
    )?
    else {
        println!("UNVERIFIED: FS_TEST_CLUSTER unset; no client call");
        return Ok(());
    };
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &mode)?;
    let pod = build(&plan, image)?;
    let cluster = RealCluster::connect(&configs)?;
    let guard = plan.acquire(&cluster)?;
    let result = if production {
        cluster.run_production_pod(guard.namespace(), &pod)
    } else {
        cluster.run_pod(guard.namespace(), &pod)
    };
    guard.close()?;
    result
}

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_execute_api() -> Result<(), String> {
    run_pod_of(
        |plan, image| qualification_manifest(plan, image, None),
        false,
    )
}

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_execute_api_production_image() -> Result<(), String> {
    run_pod_of(
        |plan, image| {
            let worker = std::env::var(WORKER_IMAGE_VARIABLE).map_err(|_| {
                format!("UNVERIFIED: {WORKER_IMAGE_VARIABLE} must be the production worker image <repo>@sha256:<digest>")
            })?;
            production_manifest(plan, image, &worker)
        },
        true,
    )
}

// --- offline checks of the manifests ----------------------------------------------------------

fn pinned(letter: char) -> String {
    format!(
        "registry.invalid/image@sha256:{}",
        letter.to_string().repeat(64)
    )
}

fn every_container(pod: &serde_json::Value) -> Vec<&serde_json::Value> {
    ["initContainers", "containers"]
        .iter()
        .flat_map(|key| pod["spec"][key].as_array().into_iter().flatten())
        .collect()
}

fn assert_restricted(pod: &serde_json::Value, plan: &NamespacePlan) {
    assert!(plan.namespace().name().starts_with("fs-test-run01-"));
    assert_eq!(pod["metadata"]["namespace"], plan.namespace().name());
    assert_eq!(pod["metadata"]["labels"][RUN_LABEL], plan.run());
    let spec = &pod["spec"];
    assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
    assert_eq!(
        spec["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert!(spec["securityContext"]["runAsUser"].as_u64().unwrap() > 65532);
    assert_eq!(spec["automountServiceAccountToken"], false);
    assert!(spec.get("hostNetwork").is_none() && spec.get("hostPID").is_none());
    for container in every_container(pod) {
        let context = &container["securityContext"];
        assert_eq!(
            context["readOnlyRootFilesystem"], true,
            "{}",
            container["name"]
        );
        assert_eq!(
            context["allowPrivilegeEscalation"], false,
            "{}",
            container["name"]
        );
        assert_eq!(context["capabilities"]["drop"], serde_json::json!(["ALL"]));
        assert!(context.get("privileged").is_none());
        assert!(image::pinned_image(container["image"].as_str().unwrap()));
    }
    for volume in spec["volumes"].as_array().unwrap() {
        assert!(
            volume["emptyDir"].is_object(),
            "only emptyDir volumes: {volume}"
        );
    }
}

#[test]
fn the_in_process_pod_runs_the_runner_in_a_restricted_run01_namespace() {
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &NamespaceMode::Create).unwrap();
    let pod = qualification_manifest(&plan, &pinned('a'), None).unwrap();
    assert_restricted(&pod, &plan);
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([RUNNER, "--execute-api"])
    );
    let one = qualification_manifest(&plan, &pinned('a'), Some("echo")).unwrap();
    assert_eq!(
        one["spec"]["containers"][0]["command"],
        serde_json::json!([RUNNER, "--execute-api=echo"])
    );
}

#[test]
fn the_production_pod_runs_the_unmodified_worker_image_beside_the_runner() {
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &NamespaceMode::Create).unwrap();
    let (runner, worker) = (pinned('a'), pinned('b'));
    let pod = production_manifest(&plan, &runner, &worker).unwrap();
    assert_restricted(&pod, &plan);
    let init = pod["spec"]["initContainers"].as_array().unwrap();
    let sidecar = &init[1];
    // The sidecar is the production image and the production entrypoint: nothing of the
    // qualification image runs inside the worker.
    assert_eq!(sidecar["image"], worker.as_str());
    assert_eq!(
        sidecar["command"],
        serde_json::json!([WORKER, "serve", "/shared/worker.toml"])
    );
    assert_eq!(sidecar["restartPolicy"], "Always");
    assert_eq!(sidecar["volumeMounts"][0]["readOnly"], true);
    assert_eq!(init[0]["image"], runner.as_str());
    let runner_container = &pod["spec"]["containers"][0];
    assert_eq!(
        runner_container["command"],
        serde_json::json!([RUNNER, "--production-target"])
    );
    // Both images must be digest-pinned.
    assert!(production_manifest(&plan, &runner, "worker:latest").is_err());
    assert!(production_manifest(&plan, "runner:latest", &worker).is_err());
}

#[test]
fn an_explicit_live_request_without_the_gate_is_a_failure_of_the_shared_runner() {
    // The shared runner (used by the sibling qualification suites) never reports a pass when
    // nothing was qualified; this suite's own tests above print the skip and return instead.
    if std::env::var_os("FS_TEST_CLUSTER").is_none() {
        assert!(live_qualification().is_err());
    }
}
