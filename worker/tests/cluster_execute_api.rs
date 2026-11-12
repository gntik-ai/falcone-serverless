//! Operator-only qualification of the served worker execution API (RUN-01), one restricted pod
//! per criterion in a dedicated `fs-test-run01-*` namespace that the test creates and deletes.
//!
//! The pod runs `compile-cache-qualification --execute-api=<case>`, which starts the worker's
//! `serve` process beside it and calls it over the pod-local port with embedded guests. Pod
//! Security `restricted`, seccomp `RuntimeDefault`, a read-only root filesystem and an arbitrary
//! non-root UID are those of the shared WAS-02 pod manifest. Without `FS_TEST_CLUSTER` every
//! test here is ignored with an explicit UNVERIFIED reason and touches no namespace.
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
    ($($name:ident => $case:literal),* $(,)?) => {$(
        #[test]
        #[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
        fn $name() -> Result<(), String> { run_case($case) }
    )*};
}
cluster_cases! {
    // AC1: the legacy envelope carries the identity of the projection and the pinned engine.
    activation_identity_matches_the_projection => "identity",
    // AC2: another runtime mode, an image, or OCI image bytes are refused and create no instance.
    non_wasm_requests_create_no_instance => "refusals",
    // AC7: a missing artifact, a digest mismatch and an oversized body are refused before compile.
    unverified_artifacts_are_refused_before_compile => "artifacts",
    // AC4: no credential claim value in the guest's memory, and no WASI environment to read.
    the_credential_stays_out_of_the_guest => "credential",
    // AC6: cancelling a running attempt stops the guest and is acknowledged.
    cancel_stops_the_running_guest => "cancel",
}

#[test]
fn pod_runs_the_execute_api_case_in_a_run01_namespace() {
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &NamespaceMode::Create).unwrap();
    assert!(plan.namespace().name().starts_with("fs-test-run01-"));
    let image = format!("registry.invalid/worker@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--execute-api=identity"
        ])
    );
}
