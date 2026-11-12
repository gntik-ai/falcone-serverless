//! RUN-01.t3: the worker execution API in the restricted fs-test pod, one pod per case.
//!
//! Each case runs `compile-cache-qualification --execute-api=<case>` in a Pod Security restricted
//! pod (seccomp RuntimeDefault, read-only root filesystem, arbitrary non-root UID) in an
//! `fs-test-run01-*` namespace that the test creates and deletes (or, with
//! `FS_TEST_NAMESPACE_MODE=shared`, in the named namespace with run-labelled objects only). The
//! pod starts the service on loopback and drives it over real TCP: runtime identity, the non-wasm
//! refusal, the legacy refusals, credential absence, declared imports, saturation and cancel, and
//! every HTTP size ceiling and transfer deadline. Gated by `FS_TEST_CLUSTER`; skipped with an
//! explicit message when it is unset.
#![cfg(target_os = "linux")]
#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

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

fn run_case(case: &str) -> Result<(), String> {
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
    let mut pod = pod_manifest(&plan, image)?;
    pod["spec"]["containers"][0]["command"][1] = format!("--execute-api={case}").into();
    let cluster = RealCluster::connect(&configs)?;
    let guard = plan.acquire(&cluster)?;
    let result = cluster.run_pod(guard.namespace(), &pod);
    guard.close()?;
    result
}

macro_rules! cluster_cases {
    ($($name:ident => $case:literal),* $(,)?) => {
        const CASES: &[&str] = &[$($case),*];
        $(
            #[test]
            #[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
            fn $name() -> Result<(), String> { run_case($case) }
        )*
    };
}
cluster_cases! {
    identity => "identity",
    non_wasm => "non-wasm",
    legacy => "legacy",
    credential => "credential",
    imports => "imports",
    saturation => "saturation",
    saturation_stalled => "saturation-stalled",
    http_headers => "http-headers",
    http_request => "http-request",
    http_action_body => "http-action-body",
    http_silent => "http-silent",
    http_drip => "http-drip",
    http_stall => "http-stall",
    http_control_body => "http-control-body",
    http_body_drip => "http-body-drip",
}

#[test]
fn pod_selects_the_execute_api_runner() {
    let plan = NamespacePlan::new("run01", &NamespaceMode::Create).unwrap();
    let image = format!("registry.invalid/worker@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert!(plan.namespace().name().starts_with("fs-test-run01-"));
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--execute-api=identity"
        ])
    );
}

/// Every case the runner knows has a cluster test, and the other way round, so a security-relevant
/// case cannot be added to the runner without a pod that runs it.
#[test]
fn every_runner_case_has_a_cluster_test() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/qualification/execution_api.rs"),
    )
    .unwrap();
    let table = source
        .split("pub const CASES")
        .nth(1)
        .and_then(|rest| rest.split("];").next())
        .unwrap();
    let runner: std::collections::BTreeSet<_> = table
        .lines()
        .filter_map(|line| line.trim().strip_prefix("(\""))
        .filter_map(|line| line.split('"').next())
        .collect();
    let cluster: std::collections::BTreeSet<_> = CASES.iter().copied().collect();
    assert_eq!(runner, cluster);
    // The ceilings and deadlines of the HTTP service each have a case.
    for case in [
        "http-headers",
        "http-request",
        "http-action-body",
        "http-silent",
        "http-drip",
        "http-stall",
        "http-control-body",
        "http-body-drip",
        "saturation",
        "saturation-stalled",
    ] {
        assert!(cluster.contains(case), "{case}");
    }
}
