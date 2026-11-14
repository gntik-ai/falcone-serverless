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

const QUALIFICATION_PREFIX: &str = "execute-api";
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

/// Runs `command` in a restricted pod of `image` in a namespace this test creates and deletes.
fn run_command(image: Option<(&str, &str)>, command: serde_json::Value) -> Result<(), String> {
    let image_variable = image.map_or("FS_TEST_WAS02_IMAGE", |(variable, _)| variable);
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
    let mut pod = pod_manifest(&plan, image)?;
    pod["spec"]["containers"][0]["command"] = command;
    let cluster = RealCluster::connect(&configs)?;
    let guard = plan.acquire(&cluster)?;
    let result = cluster.run_pod(guard.namespace(), &pod);
    guard.close()?;
    result
}

fn run_case(case: &str) -> Result<(), String> {
    run_command(
        None,
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
    slow_reader => "slow-reader",
    timeout_selection => "timeout-selection",
    budgets => "budgets",
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
        Some(("FS_TEST_WORKER_IMAGE", "")),
        serde_json::json!([
            "/usr/local/bin/falcone-serverless-worker",
            "--check-install"
        ]),
    )
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
