//! Operator-only execution API qualification, one restricted pod per case: the worker's real
//! execute and control listeners, cache, compile child and guests, with the documented
//! production deadlines. The cases are those of `src/qualification/execution_api.rs`.
//!
//! `production_image_ships_the_compile_child` runs the production worker image (not the
//! qualification image) and checks that `artifact-compile-child` is beside the worker.
#![cfg(target_os = "linux")]
#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

// The namespace is `fs-test-run01-<id>`, created and deleted under Pod Security restricted.
const QUALIFICATION_PREFIX: &str = "run01";
const QUALIFICATION_FLAG: &str = "--execute-api=identity";
const QUALIFICATION_CHECK: &str = "execute_api";
const CLUSTER_TEST: &str = "cluster_execute_api";
const SKIP_EXPLICIT_REQUEST_WITHOUT_GATE: bool = true;
include!("support/was02_cluster.rs");

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_execute_api() -> Result<(), String> {
    if std::env::var_os("FS_TEST_CLUSTER").is_none() {
        println!("UNVERIFIED: FS_TEST_CLUSTER unset; no client call");
        return Ok(());
    }
    live_qualification()
}

/// Runs the pod `build` renders from the qualification image, in a namespace this test creates
/// and deletes. `image_variable` names the variable holding the image the pod's first image is
/// taken from.
fn run_pod_of(
    image_variable: &str,
    build: impl FnOnce(&NamespacePlan, &str) -> Result<serde_json::Value, String>,
) -> Result<(), String> {
    let image = std::env::var(image_variable).ok();
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
    let result = cluster.run_pod(guard.namespace(), &pod);
    guard.close()?;
    result
}

/// Runs `command` in a restricted pod of the image `image_variable` names.
fn run_command(image_variable: &str, command: serde_json::Value) -> Result<(), String> {
    run_pod_of(image_variable, |plan, image| {
        let mut pod = pod_manifest(plan, image)?;
        pod["spec"]["containers"][0]["command"] = command;
        Ok(pod)
    })
}

fn run_case(case: &str) -> Result<(), String> {
    run_command(
        "FS_TEST_WAS02_IMAGE",
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            format!("--execute-api={case}")
        ]),
    )
}

macro_rules! cluster_cases {
    ($($name:ident => $case:literal),* $(,)?) => {$(
        #[test]
        #[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
        fn $name() -> Result<(), String> { run_case($case) }
    )*};
}
cluster_cases! {
    identity => "identity",
    non_wasm => "non-wasm",
    engine_version => "engine-version",
    credential => "credential",
    redaction => "redaction",
    imports => "imports",
    listeners => "listeners",
    saturation => "saturation",
    saturation_control => "saturation-control",
    admission_order => "admission-order",
    update_cancel => "update-cancel",
    http => "http",
    control_http => "control-http",
    slow_reader => "slow-reader",
    timeout_selection => "timeout-selection",
    budgets => "budgets",
    budget_entries => "budget-entries",
    deadline => "deadline",
    cache_stalls => "cache-stalls",
    validation => "validation",
}

/// The production worker image holds the trusted compile child beside the worker binary, where
/// the worker resolves it. `FS_TEST_WORKER_IMAGE` must be `<repo>@sha256:<digest>`.
#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn production_image_ships_the_compile_child() -> Result<(), String> {
    run_command(
        "FS_TEST_WORKER_IMAGE",
        serde_json::json!([
            "/usr/local/bin/falcone-serverless-worker",
            "--check-install"
        ]),
    )
}

/// The same restricted pod runs three containers on one scratch volume: the qualification image
/// prepares the worker configuration and artifact (init), the production worker image serves
/// them (a native sidecar, `serve`), and the qualification image drives both listeners. The
/// execution, its runtime identity and the refusals are those of the production image.
fn production_pod(
    plan: &NamespacePlan,
    runner_image: &str,
    worker_image: &str,
) -> Result<serde_json::Value, String> {
    if !image::pinned_image(worker_image) {
        return Err("UNVERIFIED: FS_TEST_WORKER_IMAGE must be <repo>@sha256:<digest>".into());
    }
    let mut pod = pod_manifest(plan, runner_image)?;
    let runner = |name: &str, case: &str| {
        let mut container = pod["spec"]["containers"][0].clone();
        container["name"] = name.into();
        container["command"] = serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            format!("--execute-api={case}")
        ]);
        container
    };
    let mount = serde_json::json!({"name": "shared", "mountPath": "/shared"});
    let mut prepare = runner("prepare", "production-prepare");
    let mut client = runner("runner", "production-client");
    let mut worker = pod["spec"]["containers"][0].clone();
    worker["name"] = "worker".into();
    worker["image"] = worker_image.into();
    worker["command"] = serde_json::json!([
        "/usr/local/bin/falcone-serverless-worker",
        "serve",
        "/shared/worker.toml"
    ]);
    // A native sidecar: it serves for the life of the pod and does not keep it from completing.
    worker["restartPolicy"] = "Always".into();
    for container in [&mut prepare, &mut client, &mut worker] {
        container["volumeMounts"]
            .as_array_mut()
            .unwrap()
            .push(mount.clone());
    }
    pod["spec"]["initContainers"] = serde_json::json!([prepare, worker]);
    pod["spec"]["containers"] = serde_json::json!([client]);
    pod["spec"]["volumes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"name": "shared", "emptyDir": {"sizeLimit": "256Mi"}}));
    Ok(pod)
}

/// In the restricted pod, the production worker image executes a wasm revision through the API:
/// the legacy envelope with the pinned runtime identity and no credential, a non-wasm revision
/// refused, and each listener refusing the other's operations. Needs `FS_TEST_WAS02_IMAGE` (the
/// qualification image) and `FS_TEST_WORKER_IMAGE` (the production image).
#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn production_image_serves_the_execution_api() -> Result<(), String> {
    let worker = std::env::var("FS_TEST_WORKER_IMAGE").ok();
    if std::env::var_os("FS_TEST_CLUSTER").is_some() && worker.is_none() {
        return Err("UNVERIFIED: FS_TEST_WORKER_IMAGE must be <repo>@sha256:<digest>".into());
    }
    run_pod_of("FS_TEST_WAS02_IMAGE", |plan, runner| {
        production_pod(plan, runner, worker.as_deref().unwrap_or_default())
    })
}

#[test]
fn production_pod_runs_the_production_image_in_a_restricted_pod() {
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &NamespaceMode::Create).unwrap();
    assert!(
        plan.namespace().name().starts_with("fs-test-run01-"),
        "{}",
        plan.namespace().name()
    );
    let runner = format!("registry.invalid/runner@sha256:{}", "a".repeat(64));
    let worker = format!("registry.invalid/worker@sha256:{}", "b".repeat(64));
    assert!(production_pod(&plan, &runner, "worker:latest").is_err());
    let pod = production_pod(&plan, &runner, &worker).unwrap();
    let spec = &pod["spec"];
    assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
    assert_eq!(
        spec["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert_eq!(spec["automountServiceAccountToken"], false);
    let init = spec["initContainers"].as_array().unwrap();
    assert_eq!(init.len(), 2);
    assert_eq!(init[0]["image"], runner.as_str());
    assert_eq!(init[1]["image"], worker.as_str());
    assert_eq!(init[1]["restartPolicy"], "Always");
    assert_eq!(
        init[1]["command"],
        serde_json::json!([
            "/usr/local/bin/falcone-serverless-worker",
            "serve",
            "/shared/worker.toml"
        ])
    );
    let containers = spec["containers"].as_array().unwrap();
    assert_eq!(containers.len(), 1);
    assert_eq!(containers[0]["image"], runner.as_str());
    assert_eq!(
        containers[0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--execute-api=production-client"
        ])
    );
    for container in init.iter().chain(containers) {
        assert_eq!(
            container["securityContext"]["readOnlyRootFilesystem"], true,
            "{}",
            container["name"]
        );
        assert_eq!(
            container["securityContext"]["allowPrivilegeEscalation"],
            false
        );
        assert_eq!(
            container["securityContext"]["capabilities"]["drop"],
            serde_json::json!(["ALL"])
        );
        assert!(
            container["volumeMounts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|mount| mount["name"] == "shared" && mount["mountPath"] == "/shared")
        );
    }
}

#[test]
fn pod_selects_execute_api() {
    let plan = NamespacePlan::new("execute-api", &NamespaceMode::Create).unwrap();
    let image = format!("registry.invalid/worker@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--execute-api=identity"
        ])
    );
    assert_eq!(pod["spec"]["securityContext"]["runAsNonRoot"], true);
}
