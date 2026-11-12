//! Operator-only restricted RUN-01.t3 qualification of the worker execution API: one restricted
//! fs-test pod per case. The pod starts the worker service beside the runner and drives both
//! listeners over real TCP (`src/qualification/execution_api.rs`); no live access in ordinary
//! tests. Cases cover every HTTP size ceiling and deadline and the saturation cases.
#![cfg(target_os = "linux")]
#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

const QUALIFICATION_PREFIX: &str = "run01";
const QUALIFICATION_FLAG: &str = "--execution-api=envelope";
const QUALIFICATION_CHECK: &str = "execution_api";
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
    pod["spec"]["containers"][0]["command"][1] = format!("--execution-api={case}").into();
    let cluster = RealCluster::connect(&configs)?;
    let guard = plan.acquire(&cluster)?;
    let result = cluster.run_pod(guard.namespace(), &pod);
    guard.close()?;
    result
}
macro_rules! cluster_cases {
    ($($name:ident => $case:literal),* $(,)?) => {$(
        #[test]
        #[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
        fn $name() -> Result<(), String> { run_case($case) }
    )*};
}
cluster_cases! {
    envelope => "envelope",
    runtime_mode => "runtime-mode",
    credential => "credential",
    imports => "imports",
    redaction_load => "redaction-load",
    saturation => "saturation",
    control_stall => "control-stall",
    cross_listener => "cross-listener",
    headers => "headers",
    request => "request",
    action_body => "action-body",
    silent => "silent",
    drip => "drip",
    stalled_body => "stalled-body",
}

#[test]
fn pod_selects_the_execution_api_qualification() {
    let plan = NamespacePlan::new("run01", &NamespaceMode::Create).unwrap();
    let image = format!("registry.invalid/worker@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert!(plan.namespace().name().starts_with("fs-test-run01-"));
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--execution-api=envelope"
        ])
    );
}

/// Every local case has a restricted-pod case of the same name.
#[test]
fn every_case_has_a_cluster_case() {
    let local = include_str!("execute_api_local.rs");
    let cluster = include_str!("cluster_execute_api.rs");
    for line in local.lines().filter(|line| line.contains("=> \"")) {
        let case = line
            .split("=> \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next());
        let case = case.unwrap_or_default();
        assert!(cluster.contains(&format!("=> \"{case}\"")), "{case}");
    }
}
