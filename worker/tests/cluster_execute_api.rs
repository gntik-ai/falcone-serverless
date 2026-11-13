//! Operator-only RUN-01 execution API qualification in the restricted fs-test pod harness: the
//! worker binary serves the API inside the pod and the qualification runner drives it over TCP
//! (`worker/src/qualification/execution_api.rs`), one pod per security criterion. The pod runs
//! under Pod Security restricted, seccomp `RuntimeDefault`, a read-only root filesystem and an
//! arbitrary non-root UID, in a namespace `fs-test-run01-<run-id>` that the test creates and
//! deletes. Gated by `FS_TEST_CLUSTER`; it skips with an explicit message when the gate is
//! unset. The image (`FS_TEST_WAS02_IMAGE`, `worker/qualification/Containerfile`) carries the
//! worker, the trusted compiler and the runner. Credentials never appear here: the cases use
//! recognizable canary strings, and a real credential would come from OpenBao through the
//! existing External Secrets.
#![cfg(target_os = "linux")]
#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

const QUALIFICATION_PREFIX: &str = "run01";
const QUALIFICATION_FLAG: &str = "--execution-api=all";
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
    identity => "identity",
    runtime_mode => "runtime-mode",
    credential => "credential",
    imports => "imports",
    budgets => "budgets",
    timeouts => "timeouts",
    duplicate_attempt => "duplicate-attempt",
    saturation => "saturation",
    control_stall => "control-stall",
    http_limits => "http-limits",
    body_deadline => "body-deadline",
    slow_reader => "slow-reader",
    adversarial_redaction => "adversarial-redaction",
}

#[test]
fn pod_selects_the_execution_api_qualification_in_a_run01_namespace() {
    let plan = NamespacePlan::new("run01", &NamespaceMode::Create).unwrap();
    assert!(plan.namespace().name().starts_with("fs-test-run01-"));
    let image = format!("registry.invalid/worker@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--execution-api=all"
        ])
    );
}
