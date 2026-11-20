//! Operator-only qualification of prebuilt Go components in a restricted fs-test pod.
#![cfg(target_os = "linux")]

#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;
#[path = "support/pod.rs"]
pub mod pod;
use image::pinned_image;

const QUALIFICATION_PREFIX: &str = "profile-go";
const QUALIFICATION_FLAG: &str = "--profile-go";
const QUALIFICATION_CHECK: &str = "profile_go";
const CLUSTER_TEST: &str = "cluster_profile_go";
const SKIP_EXPLICIT_REQUEST_WITHOUT_GATE: bool = true;
const PRODUCTION_IMAGE_ENV: Option<&str> = None;

include!("support/was02_cluster.rs");

fn profile_pod(plan: &NamespacePlan, image: &str) -> Result<serde_json::Value, String> {
    let mut pod = pod_manifest(plan, image)?;
    let common = pod::pod_spec(image)?;
    pod["spec"]["enableServiceLinks"] = common["enableServiceLinks"].clone();
    pod["spec"]["containers"][0]["command"] = serde_json::json!([
        "/usr/local/bin/profile-go-runtime",
        "--exact",
        "built_profile_components",
        "--ignored",
        "--nocapture"
    ]);
    pod["spec"]["containers"][0]["env"] = serde_json::json!([
        {"name":"FS_TOOLCHAIN_CACHE","value":"/opt/falcone-toolchain-cache"},
        {"name":"FS_GO_COMPONENTS","value":"/opt/falcone-toolchain-cache/go-profile"},
        {"name":"TMPDIR","value":"/tmp"}
    ]);
    pod::validate_spec(&pod["spec"])?;
    Ok(pod)
}

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_profile_go() -> Result<(), String> {
    if std::env::var_os("FS_TEST_CLUSTER").is_none() {
        // Adapt the shared harness's explicit-request contract to this suite's visible skip.
        return live_qualification().or_else(|error| {
            if error
                == format!("UNVERIFIED: {CLUSTER_TEST} was requested but FS_TEST_CLUSTER is unset")
            {
                Ok(())
            } else {
                Err(error)
            }
        });
    }
    if std::env::var_os("FS_TOOLCHAIN_CACHE").is_none() {
        println!(
            "UNVERIFIED: FS_TOOLCHAIN_CACHE unset; build the Go components in the trusted compiler image first"
        );
        return Ok(());
    }
    let Some((image, mode, configs)) = validated_inputs(
        std::env::var("FS_TEST_CLUSTER").ok().as_deref(),
        std::env::var("KUBECONFIG").ok().as_deref(),
        std::env::var("FS_TEST_WAS02_IMAGE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE_MODE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE").ok().as_deref(),
    )?
    .map(|(image, mode, configs)| (image.to_owned(), mode, configs)) else {
        return Err("cluster gate missing".into());
    };
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &mode)?;
    let pod = profile_pod(&plan, &image)?;
    let cluster = RealCluster::connect(&configs)?;
    let owned = matches!(mode, NamespaceMode::Create);
    let namespace = plan.namespace().clone();
    let guard = plan.acquire(&cluster)?;
    let result = cluster.run_pod(guard.namespace(), &pod);
    guard.close()?;
    if owned {
        use namespace::NamespaceApi;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while cluster.namespace_labels(&namespace)?.is_some() {
            if std::time::Instant::now() >= deadline {
                return Err("UNVERIFIED: owned fs-test namespace deletion not observed".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    result
}

#[test]
fn profile_pod_runs_only_prebuilt_go_fixtures_with_the_restricted_posture() {
    let plan = NamespacePlan::new("profile-go", &NamespaceMode::Create).unwrap();
    let image = format!("registry.invalid/profile@sha256:{}", "a".repeat(64));
    let pod = profile_pod(&plan, &image).unwrap();
    assert!(
        falcone_serverless_worker::check(
            &pod::deployment(&pod["spec"]),
            falcone_serverless_worker::Role::GuestWorker
        )
        .is_empty()
    );
    assert!(plan.namespace().name().starts_with("fs-test-"));
    assert_eq!(pod["spec"]["securityContext"]["runAsNonRoot"], true);
    assert!(
        pod["spec"]["securityContext"]["runAsUser"]
            .as_u64()
            .unwrap()
            >= 100_000
    );
    assert_eq!(
        pod["spec"]["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert_eq!(
        pod["spec"]["containers"][0]["securityContext"]["readOnlyRootFilesystem"],
        true
    );
    assert_eq!(
        pod["spec"]["containers"][0]["securityContext"]["allowPrivilegeEscalation"],
        false
    );
    assert_eq!(pod["spec"]["automountServiceAccountToken"], false);
    assert_eq!(pod["metadata"]["labels"][RUN_LABEL], plan.run());
    assert_eq!(
        pod["spec"]["containers"][0]["command"][0],
        "/usr/local/bin/profile-go-runtime"
    );
    assert_eq!(
        pod["spec"]["containers"][0]["command"][2],
        "built_profile_components"
    );
}

#[test]
fn missing_cache_skips_before_attempting_to_read_cluster_configuration() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "cluster_profile_go", "--nocapture"])
        .env("FS_TEST_CLUSTER", "1")
        .env_remove("FS_TOOLCHAIN_CACHE")
        .env("KUBECONFIG", "/nonexistent-go-profile-kubeconfig")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("UNVERIFIED: FS_TOOLCHAIN_CACHE unset")
    );
}
