//! Operator qualification: FS_TEST_CLUSTER=1 FS_TEST_WAS02_IMAGE=<repo>@sha256:<digest>
//! cargo test --locked -p falcone-serverless-worker --test cluster_capability_gates -- --ignored --nocapture
//! The image is built and pinned by the operator; evidence belongs on #52.
#![cfg(target_os = "linux")]

#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

use namespace::{NamespaceMode, NamespacePlan, RUN_LABEL};

#[path = "support/capability_cluster.rs"]
mod capability_cluster;
use capability_cluster::{Cluster, ClusterFixture, RealCluster, validated_inputs};

fn run(cluster: &impl Cluster, image: &str, mode: &NamespaceMode) -> Result<(), String> {
    let plan = NamespacePlan::new("was02-gates", mode)?;
    let pod = pod_manifest(&plan, image)?;
    let guard = plan.acquire(cluster)?;
    let result = cluster.run_pod(guard.namespace(), &pod);
    let cleanup = guard.close();
    cleanup?;
    result
}

fn opt_in<C: Cluster>(
    enabled: Option<&str>,
    config: Option<&str>,
    image: Option<&str>,
    mode: Option<&str>,
    namespace: Option<&str>,
    connect: impl FnOnce(&kubeconfig::Kubeconfigs) -> Result<C, String>,
) -> Result<bool, String> {
    capability_cluster::opt_in(enabled, config, image, mode, namespace, connect, run)
}

fn pod_manifest(plan: &NamespacePlan, image: &str) -> Result<serde_json::Value, String> {
    capability_cluster::pod_manifest(plan, image, "capabilities", "--capability-gates")
}

#[test]
fn owned_manifest_is_restricted_and_uses_only_its_run_namespace() {
    let mode = NamespaceMode::Create;
    let plan = NamespacePlan::new("was02", &mode).unwrap();
    let image = format!("registry.invalid/compiler@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert!(plan.namespace().name().starts_with("fs-test-"));
    assert_eq!(pod["metadata"]["namespace"], plan.namespace().name());
    assert_eq!(pod["metadata"]["labels"][RUN_LABEL], plan.run());
    assert!(
        pod["metadata"]["name"]
            .as_str()
            .unwrap()
            .ends_with(plan.run())
    );
    let spec = &pod["spec"];
    assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
    assert_eq!(
        spec["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert!(spec["securityContext"]["runAsUser"].as_u64().unwrap() > 65532);
    assert_eq!(spec["automountServiceAccountToken"], false);
    let container = &spec["containers"][0];
    assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
    assert_eq!(
        container["securityContext"]["allowPrivilegeEscalation"],
        false
    );
    assert_eq!(
        container["securityContext"]["capabilities"]["drop"],
        serde_json::json!(["ALL"])
    );
    assert_eq!(
        container["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--capability-gates"
        ])
    );
    assert_eq!(container["image"], image);
    assert!(spec["volumes"][0]["emptyDir"].is_object());
    assert!(pod_manifest(&plan, "compiler:latest").is_err());
}

#[test]
fn namespace_modes_cleanup_only_owned_resources_even_on_failure() {
    let image = format!("registry.invalid/compiler@sha256:{}", "a".repeat(64));
    for fails in [false, true] {
        let cluster = ClusterFixture {
            pod_fails: fails,
            ..Default::default()
        };
        let result = run(&cluster, &image, &NamespaceMode::Create);
        assert_eq!(result.is_err(), fails);
        let calls = cluster.calls.borrow();
        assert_eq!(calls.len(), 3);
        let ns = calls[0].strip_prefix("create ").unwrap();
        assert!(calls[1].starts_with(&format!("pod {ns} ")));
        assert_eq!(calls[2], format!("delete {ns}"));

        let cluster = ClusterFixture {
            pod_fails: fails,
            ..Default::default()
        };
        let mode = NamespaceMode::Shared(namespace::Namespace::new("fs-test-shared").unwrap());
        assert_eq!(run(&cluster, &image, &mode).is_err(), fails);
        let calls = cluster.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], calls[0].replacen("pod ", "cleanup ", 1));
    }
    let cluster = ClusterFixture {
        deny: true,
        ..Default::default()
    };
    assert!(
        run(&cluster, &image, &NamespaceMode::Create)
            .unwrap_err()
            .contains("FS_TEST_NAMESPACE_MODE")
    );
    assert!(cluster.calls.borrow().is_empty());
}

#[test]
fn unset_gate_prints_skip_without_constructing_a_client() {
    assert!(
        !opt_in(
            None,
            Some(":"),
            None,
            None,
            None,
            |_| -> Result<ClusterFixture, String> { panic!("client must not be constructed") }
        )
        .unwrap()
    );
    for image in [None, Some("registry.invalid/compiler:latest")] {
        assert!(
            opt_in(
                Some("1"),
                Some(":"),
                image,
                None,
                None,
                |_| -> Result<ClusterFixture, String> { panic!("client must not be constructed") }
            )
            .unwrap_err()
            .starts_with("UNVERIFIED")
        );
    }
}

#[test]
fn live_qualification_reports_ignored_when_cluster_unset() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cluster_capability_gates"])
        .env_remove("FS_TEST_CLUSTER")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout
            .contains("test cluster_capability_gates ... ignored, not run: FS_TEST_CLUSTER unset"),
        "live qualification must visibly report its skip reason: {stdout}"
    );
    assert!(stdout.contains("0 passed; 0 failed; 1 ignored"), "{stdout}");
}

#[test]
#[ignore = "not run: FS_TEST_CLUSTER unset"]
fn cluster_capability_gates() -> Result<(), String> {
    // Only runs when requested (--ignored or --include-ignored); an unset gate is then a
    // failure, never a pass with no qualification executed.
    let ran = opt_in(
        std::env::var("FS_TEST_CLUSTER").ok().as_deref(),
        std::env::var("KUBECONFIG").ok().as_deref(),
        std::env::var("FS_TEST_WAS02_IMAGE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE_MODE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE").ok().as_deref(),
        |configs| RealCluster::connect(configs, verify_records),
    )?;
    if ran {
        Ok(())
    } else {
        Err(
            "UNVERIFIED: cluster_capability_gates was requested but FS_TEST_CLUSTER is unset"
                .into(),
        )
    }
}

/// Runs in every `cargo test`. With FS_TEST_CLUSTER set, invalid opt-in inputs fail here without
/// constructing a client, and a valid opt-in fails unless the ignored live qualification was
/// requested, so setting the gate can never yield a green run with no qualification executed.
#[test]
fn cluster_gate_inputs_fail_closed_when_set() -> Result<(), String> {
    let (enabled, config, image, mode, namespace) = (
        std::env::var("FS_TEST_CLUSTER").ok(),
        std::env::var("KUBECONFIG").ok(),
        std::env::var("FS_TEST_WAS02_IMAGE").ok(),
        std::env::var("FS_TEST_NAMESPACE_MODE").ok(),
        std::env::var("FS_TEST_NAMESPACE").ok(),
    );
    let inputs = validated_inputs(
        enabled.as_deref(),
        config.as_deref(),
        image.as_deref(),
        mode.as_deref(),
        namespace.as_deref(),
    )?;
    let live_requested =
        std::env::args().any(|arg| arg == "--ignored" || arg == "--include-ignored");
    if inputs.is_some() && !live_requested {
        return Err(
            "UNVERIFIED: FS_TEST_CLUSTER=1 runs the live qualification only with --include-ignored"
                .into(),
        );
    }
    Ok(())
}

fn run_self(args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(args)
        .env_remove("FS_TEST_CLUSTER")
        .env_remove("FS_TEST_WAS02_IMAGE")
        .env_remove("KUBECONFIG");
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text)
}

#[test]
fn gate_set_with_invalid_inputs_fails_the_default_run() {
    let (passed, output) = run_self(
        &["--exact", "cluster_gate_inputs_fail_closed_when_set"],
        &[
            ("FS_TEST_CLUSTER", "1"),
            ("FS_TEST_WAS02_IMAGE", "not-pinned"),
        ],
    );
    assert!(
        !passed,
        "an invalid opt-in must fail the default run: {output}"
    );
    assert!(
        output.contains("UNVERIFIED: FS_TEST_WAS02_IMAGE must be <repo>@sha256:<digest>"),
        "{output}"
    );
}

#[test]
fn requesting_the_live_qualification_without_the_gate_fails() {
    let (passed, output) = run_self(&["--exact", "cluster_capability_gates", "--ignored"], &[]);
    assert!(!passed, "an unset gate must not read as a pass: {output}");
    assert!(output.contains("FS_TEST_CLUSTER is unset"), "{output}");
}

#[test]
fn unset_gate_is_skipped_and_invalid_opt_in_fails_closed() {
    assert!(
        !opt_in(
            None,
            Some(":"),
            None,
            None,
            None,
            |_| -> Result<ClusterFixture, String> { panic!("client must not be constructed") }
        )
        .unwrap()
    );
    for gate in ["0", "true", ""] {
        assert!(
            opt_in(
                Some(gate),
                None,
                None,
                None,
                None,
                |_| -> Result<ClusterFixture, String> { panic!("client must not be constructed") }
            )
            .is_err()
        );
    }
    let pinned = format!("registry.invalid/worker@sha256:{}", "a".repeat(64));
    for config in [None, Some(":"), Some("/nonexistent-was02-kubeconfig")] {
        assert!(
            opt_in(
                Some("1"),
                config,
                Some(&pinned),
                None,
                None,
                |_| -> Result<ClusterFixture, String> {
                    panic!("invalid inputs reached client construction")
                }
            )
            .is_err()
        );
    }
    for (mode, namespace) in [
        ("invalid", None),
        ("shared", None),
        ("shared", Some("off-limits")),
    ] {
        assert!(
            opt_in(
                Some("1"),
                None,
                Some(&pinned),
                Some(mode),
                namespace,
                |_| -> Result<ClusterFixture, String> {
                    panic!("invalid namespace reached client construction")
                }
            )
            .is_err()
        );
    }
}

fn verify_records(logs: &str) -> Result<(), String> {
    if logs.lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line).is_ok_and(|record| {
            record == serde_json::json!({"check":"capability_gates","status":"passed"})
        })
    }) {
        Ok(())
    } else {
        Err("capability qualification success record missing".into())
    }
}
