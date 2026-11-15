//! Operator-only restricted qualification of the worker execution API (RUN-01.t3), in the
//! `fs-test-run01-*` namespace: the local suite (`execute_api_local`) runs unchanged inside a
//! Pod Security restricted pod with seccomp RuntimeDefault, a read-only root filesystem and an
//! arbitrary non-root UID. It covers identity, refusal, credential absence, imports, saturation,
//! timeout selection, budgets, HTTP ceilings and deadlines, and supervised validation and cache
//! stalls, and records one evidence line per case next to the saturation cases. Gated by
//! FS_TEST_CLUSTER; without it the case is skipped with an explicit message and no client call.
//! Test namespaces follow testNamespaceMode: `create` (the default) creates and deletes a
//! dedicated namespace; `shared` uses FS_TEST_NAMESPACE and touches only labelled objects.
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

/// The digest-pinned production worker image under qualification: the pod runs it beside the
/// runner (`add_production`) and the suite drives it over the pod's loopback.
const PRODUCTION_IMAGE_ENV: Option<&str> = Some("FS_TEST_WORKER_IMAGE");

include!("support/was02_cluster.rs");

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_execute_api() -> Result<(), String> {
    if std::env::var_os("FS_TEST_CLUSTER").is_none() {
        println!("UNVERIFIED: FS_TEST_CLUSTER unset; no client call");
        return Ok(());
    }
    // Without the production image nothing production would be qualified: refuse to run.
    if !std::env::var("FS_TEST_WORKER_IMAGE").is_ok_and(|image| image::pinned_image(&image)) {
        return Err("UNVERIFIED: FS_TEST_WORKER_IMAGE must be <repo>@sha256:<digest>".into());
    }
    live_qualification()
}

#[test]
fn pod_runs_the_execution_api_suite_in_the_run01_namespace() {
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &NamespaceMode::Create).unwrap();
    assert!(plan.namespace().name().starts_with("fs-test-run01-"));
    let image = format!("registry.invalid/worker@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--execute-api"
        ])
    );
}

#[test]
fn the_pod_runs_the_digest_pinned_production_image_beside_the_runner_with_the_same_posture() {
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &NamespaceMode::Create).unwrap();
    let qualification = format!("registry.invalid/qualification@sha256:{}", "a".repeat(64));
    let production = format!("registry.invalid/worker@sha256:{}", "b".repeat(64));
    let mut pod = base_pod_manifest(&plan, &qualification).unwrap();
    add_production(&mut pod, &qualification, &production);
    let spec = &pod["spec"];
    // The production image runs as a native sidecar with its own entrypoint: its image is the
    // pinned one, it is restarted for as long as the pod runs, and it only gets arguments.
    let sidecar = &spec["initContainers"][1];
    assert_eq!(sidecar["name"], "worker");
    assert_eq!(sidecar["image"], production);
    assert_eq!(sidecar["restartPolicy"], "Always");
    assert!(sidecar.get("command").is_none());
    assert_eq!(
        sidecar["args"],
        serde_json::json!(["serve", "/work/worker.toml"])
    );
    // The runner names the image under qualification; the init step writes the configuration.
    assert_eq!(spec["containers"][0]["image"], qualification);
    assert_eq!(
        spec["containers"][0]["env"],
        serde_json::json!([{"name": "EXECUTE_API_PRODUCTION_IMAGE", "value": production}])
    );
    assert_eq!(
        spec["initContainers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--execute-api-prepare",
            "/work"
        ])
    );
    // Every container is restricted, and the shared directory is a pod-local emptyDir.
    for container in [
        &spec["initContainers"][0],
        &spec["initContainers"][1],
        &spec["containers"][0],
    ] {
        let security = &container["securityContext"];
        assert_eq!(security["readOnlyRootFilesystem"], true);
        assert_eq!(security["allowPrivilegeEscalation"], false);
        assert_eq!(security["capabilities"]["drop"], serde_json::json!(["ALL"]));
        assert!(
            container["volumeMounts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|mount| mount["mountPath"] == "/work")
        );
    }
    assert!(
        spec["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|volume| volume["name"] == "work" && volume["emptyDir"].is_object())
    );
    assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
    assert_eq!(spec["automountServiceAccountToken"], false);
}
