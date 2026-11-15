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
