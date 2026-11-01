//! Offline qualification of the opt-in trusted-worker cluster harness.
//!
//! Manual operator evidence (never a CI gate): FS_TEST_CLUSTER=1
//! FS_TEST_IMAGE_REPO=<registry/repo> cargo test -p falcone-serverless-worker
//! --test cluster_harness --locked. Uses ambient kubeconfig and rootless podman login.
//! Test images remain in the registry. Shared mode retains run-labelled pods;
//! it never mutates namespaces or existing objects. SIGKILL can bypass cleanup.

mod support;

use falcone_serverless_worker::{Role, check};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::time::{Duration, Instant};
use support::cli::{Cli, Process, Runner};
use support::{
    Config, Kube, Namespace, NamespaceMode, Session, deployment, final_uid, namespace_name,
    pod_spec,
};

const CONTAINERFILE: &str = include_str!("../Containerfile");

#[test]
fn final_image_user_is_numeric_and_non_root() {
    assert_eq!(final_uid(CONTAINERFILE).unwrap(), 65532);
    for fixture in [
        "FROM build\nUSER 12\nFROM runtime\n",
        "FROM runtime\nUSER root\n",
        "FROM runtime\nUSER 0\n",
        "FROM runtime\nUSER 0:0\n",
        "FROM runtime\nUSER 12\nUSER root\n",
    ] {
        assert!(final_uid(fixture).is_err(), "{fixture}");
    }
}

fn image() -> String {
    format!("registry.invalid/worker@sha256:{}", "a".repeat(64))
}

#[test]
fn worker_spec_is_restricted_and_uses_an_arbitrary_uid() {
    let spec = pod_spec(&image()).unwrap();
    assert!(check(&deployment(&spec), Role::GuestWorker).is_empty());
    assert_eq!(spec["restartPolicy"], "Never");
    assert_eq!(spec["automountServiceAccountToken"], false);
    assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
    let uid = spec["securityContext"]["runAsUser"].as_u64().unwrap();
    assert!(uid > 0);
    assert_ne!(uid, final_uid(CONTAINERFILE).unwrap());
    assert_eq!(
        spec["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    let container = &spec["containers"][0];
    assert_eq!(container["image"], image());
    assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
    assert_eq!(
        container["securityContext"]["allowPrivilegeEscalation"],
        false
    );
    assert_eq!(
        container["securityContext"]["capabilities"],
        json!({"drop": ["ALL"]})
    );
    for field in ["hostNetwork", "hostPID", "hostIPC", "volumes"] {
        assert!(spec.get(field).is_none(), "{field}");
    }
    assert!(container.get("ports").is_none());
    for invalid in [
        "repo:latest".into(),
        format!("repo@sha256:{}", "a".repeat(63)),
        format!("repo@sha256:{}", "A".repeat(64)),
        format!("@sha256:{}", "a".repeat(64)),
    ] {
        assert!(pod_spec(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn namespaces_are_unique_guarded_dns_labels_with_restricted_admission() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..128 {
        let name = namespace_name();
        assert!(seen.insert(name.clone()));
        assert!(name.starts_with("fs-test-"));
        assert!(name.len() <= 63);
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
        assert!(name.as_bytes().last().unwrap().is_ascii_alphanumeric());
        let ns = Namespace::new(&name).unwrap();
        assert_eq!(ns.manifest()["metadata"]["name"], name);
        assert_eq!(
            ns.manifest()["metadata"]["labels"]["pod-security.kubernetes.io/enforce"],
            "restricted"
        );
    }
    for name in [
        "default",
        "kube-system",
        "in-falcone-staging",
        "test-123",
        "fs-test-",
        "fs-test-UPPER",
        "fs-test-a.b",
        "fs-test-a_b",
        "fs-test-a-",
        "fs-test-a/b",
        &format!("fs-test-{}", "a".repeat(56)),
    ] {
        assert!(Namespace::new(name).is_err(), "{name}");
    }
    assert!(Namespace::new(&format!("fs-test-{}", "a".repeat(55))).is_ok());
}

#[test]
fn config_depends_only_on_supplied_values() {
    assert_eq!(Config::parse(None, None).unwrap(), Config::Disabled);
    assert_eq!(Config::parse(Some("0"), None).unwrap(), Config::Disabled);
    assert_eq!(
        Config::parse(Some("true"), Some("ignored")).unwrap(),
        Config::Disabled
    );
    assert_eq!(
        Config::parse(Some("1"), Some("registry.invalid/team/worker")).unwrap(),
        Config::Enabled {
            repo: "registry.invalid/team/worker".into()
        }
    );
    for repo in [None, Some(""), Some("  ")] {
        assert!(
            Config::parse(Some("1"), repo)
                .unwrap_err()
                .contains("FS_TEST_IMAGE_REPO")
        );
    }
    // These calls require no process environment access, including in parallel tests.
    assert_eq!(
        Config::parse(None, Some("ignored")).unwrap(),
        Config::Disabled
    );
}

#[derive(Default)]
struct FakeKube {
    calls: RefCell<Vec<&'static str>>,
    cleanup_deadlines: RefCell<Vec<Instant>>,
    fail_pod: bool,
    fail_namespace: bool,
    fail_delete: bool,
    fail_wait: bool,
    phase: Option<&'static str>,
    log: Option<&'static str>,
}

impl Kube for FakeKube {
    fn create_namespace(&self, _: &Namespace, _: Instant) -> Result<(), String> {
        self.calls.borrow_mut().push("create namespace");
        if self.fail_namespace {
            Err("namespace denied".into())
        } else {
            Ok(())
        }
    }
    fn create_pod(&self, _: &Namespace, pod: &Value, _: Instant) -> Result<(), String> {
        self.calls.borrow_mut().push("create pod");
        assert_eq!(pod["kind"], "Pod");
        assert_eq!(pod["metadata"]["namespace"], "fs-test-unit");
        assert_eq!(
            pod["metadata"]["labels"]["falcone.test/run"],
            "fs-test-unit"
        );
        if self.fail_pod {
            Err("create failed".into())
        } else {
            Ok(())
        }
    }
    fn wait_terminal(&self, _: &Namespace, _: &str, deadline: Instant) -> Result<String, String> {
        self.calls.borrow_mut().push("wait");
        let timeout = deadline.saturating_duration_since(Instant::now());
        assert!(timeout > Duration::ZERO && timeout <= Duration::from_secs(180));
        if self.fail_wait {
            return Err("worker pod terminal-phase wait timed out".into());
        }
        Ok(self.phase.unwrap_or("Succeeded").into())
    }
    fn logs(&self, _: &Namespace, _: &str, _: Instant) -> Result<String, String> {
        self.calls.borrow_mut().push("logs");
        Ok(self
            .log
            .unwrap_or("falcone-serverless-worker 0.1.0\n")
            .into())
    }
    fn delete_namespace(&self, _: &Namespace, deadline: Instant) -> Result<(), String> {
        self.calls.borrow_mut().push("delete namespace");
        self.cleanup_deadlines.borrow_mut().push(deadline);
        if self.fail_delete {
            Err("delete denied".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn refused_posture_makes_zero_client_calls() {
    let context = "/containers/0/securityContext";
    for (parent, field, value, rule) in [
        (context, "allowPrivilegeEscalation", json!(true), "RP-02"),
        (context, "readOnlyRootFilesystem", json!(false), "RP-06"),
        (
            "",
            "volumes",
            json!([{"name":"host", "hostPath":{"path":"/"}}]),
            "RP-08",
        ),
        ("", "automountServiceAccountToken", json!(true), "RP-10"),
        ("", "automountServiceAccountToken", Value::Null, "RP-10"),
        (
            "/containers/0/securityContext/capabilities",
            "add",
            json!(["NET_ADMIN"]),
            "RP-04",
        ),
        ("/securityContext", "runAsUser", json!(0), "RP-01"),
    ] {
        let mut spec = pod_spec(&image()).unwrap();
        let object = spec.pointer_mut(parent).unwrap().as_object_mut().unwrap();
        if value.is_null() {
            object.remove(field);
        } else {
            object.insert(field.into(), value);
        }
        let kube = FakeKube::default();
        let error = Session::create(&kube, "fs-test-unit", &spec)
            .err()
            .expect("refused");
        assert_eq!(error, format!("refused posture: {rule}"), "{field}");
        assert!(kube.calls.borrow().is_empty(), "{field}");
    }
}

#[test]
fn namespace_cleanup_runs_once_on_success_error_and_unwind() {
    for panic in [false, true] {
        let kube = FakeKube::default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _session =
                Session::create(&kube, "fs-test-unit", &pod_spec(&image()).unwrap()).unwrap();
            if panic {
                panic!("exercise unwinding");
            }
        }));
        assert_eq!(result.is_err(), panic);
        assert_eq!(
            *kube.calls.borrow(),
            ["create namespace", "create pod", "delete namespace"]
        );
    }
    let kube = FakeKube {
        fail_pod: true,
        ..FakeKube::default()
    };
    assert!(Session::create(&kube, "fs-test-unit", &pod_spec(&image()).unwrap()).is_err());
    assert_eq!(
        *kube.calls.borrow(),
        ["create namespace", "create pod", "delete namespace"]
    );
}

#[test]
fn verify_timeout_still_allows_close_and_drop_their_own_cleanup_budget() {
    for explicit_close in [false, true] {
        let kube = FakeKube {
            fail_wait: true,
            ..FakeKube::default()
        };
        let session = Session::create(&kube, "fs-test-unit", &pod_spec(&image()).unwrap()).unwrap();
        assert!(session.verify().unwrap_err().contains("timed out"));
        let cleanup_start = Instant::now();
        if explicit_close {
            session.close().unwrap();
        } else {
            drop(session);
        }
        assert_eq!(
            *kube.calls.borrow(),
            ["create namespace", "create pod", "wait", "delete namespace"]
        );
        let deadlines = kube.cleanup_deadlines.borrow();
        assert_eq!(deadlines.len(), 1);
        assert!(deadlines[0] >= cleanup_start + Duration::from_secs(60));
        assert!(deadlines[0] <= Instant::now() + Duration::from_secs(60));
    }
}

#[test]
fn guarded_create_refuses_bad_names_and_images_before_client_access() {
    for name in ["default", "in-falcone-staging", "fs-test-invalid."] {
        let kube = FakeKube::default();
        assert!(Session::create(&kube, name, &pod_spec(&image()).unwrap()).is_err());
        assert!(kube.calls.borrow().is_empty());
    }
    for (path, value) in [
        ("/containers/0/image", json!("repo:latest")),
        ("/restartPolicy", json!("Always")),
    ] {
        let kube = FakeKube::default();
        let mut spec = pod_spec(&image()).unwrap();
        *spec.pointer_mut(path).unwrap() = value;
        assert!(Session::create(&kube, "fs-test-unit", &spec).is_err());
        assert!(kube.calls.borrow().is_empty());
    }
    let kube = FakeKube {
        fail_namespace: true,
        ..FakeKube::default()
    };
    let error = Session::create(&kube, "fs-test-unit", &pod_spec(&image()).unwrap())
        .err()
        .unwrap();
    assert!(error.contains("FS_TEST_NAMESPACE_MODE"));
    assert_eq!(*kube.calls.borrow(), ["create namespace"]);
}

#[test]
fn init_container_images_are_guarded_before_any_client_call() {
    for invalid in [
        "repo:latest".into(),
        format!("repo@sha256:{}", "a".repeat(63)),
        format!("repo@sha256:{}", "A".repeat(64)),
    ] {
        let mut spec = pod_spec(&image()).unwrap();
        let mut init = spec["containers"][0].clone();
        init["name"] = json!("init");
        init["image"] = json!(invalid);
        spec["initContainers"] = json!([init]);
        assert!(check(&deployment(&spec), Role::GuestWorker).is_empty());
        let kube = FakeKube::default();
        assert!(Session::create_in(&kube, "fs-test-unit", &spec, &NamespaceMode::Create).is_err());
        assert!(kube.calls.borrow().is_empty());
        let cli = Cli::new(Commands::default());
        let pod = json!({"apiVersion":"v1", "kind":"Pod", "metadata": {
            "name":"fs-test-unit", "namespace":"fs-test-unit",
            "labels":{"falcone.test/run":"fs-test-unit"}}, "spec":spec});
        assert!(
            cli.create_pod(
                &Namespace::new("fs-test-unit").unwrap(),
                &pod,
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(cli.runner.calls.borrow().is_empty());
    }
}

#[test]
fn pinned_restricted_init_container_is_accepted() {
    let mut spec = pod_spec(&image()).unwrap();
    let mut init = spec["containers"][0].clone();
    init["name"] = json!("init");
    spec["initContainers"] = json!([init]);
    let cli = Cli::new(Commands::default());
    Session::create(&cli, "fs-test-unit", &spec)
        .unwrap()
        .close()
        .unwrap();
    let calls = cli.runner.calls.borrow();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[1].2.as_ref().unwrap()["spec"], spec);
}

#[test]
fn malformed_init_arrays_and_all_ephemeral_containers_are_refused() {
    for (field, value) in [
        ("initContainers", json!({})),
        ("initContainers", Value::Null),
        ("ephemeralContainers", json!([])),
        ("ephemeralContainers", Value::Null),
    ] {
        let mut spec = pod_spec(&image()).unwrap();
        spec[field] = value;
        let kube = FakeKube::default();
        assert!(
            Session::create(&kube, "fs-test-unit", &spec).is_err(),
            "{field}"
        );
        assert!(kube.calls.borrow().is_empty());
        let cli = Cli::new(Commands::default());
        let pod = json!({"apiVersion":"v1", "kind":"Pod", "metadata": {
            "name":"fs-test-unit", "namespace":"fs-test-unit",
            "labels":{"falcone.test/run":"fs-test-unit"}}, "spec":spec});
        assert!(
            cli.create_pod(
                &Namespace::new("fs-test-unit").unwrap(),
                &pod,
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(cli.runner.calls.borrow().is_empty());
    }
}

#[test]
fn completed_pod_requires_success_and_the_worker_version() {
    let kube = FakeKube::default();
    let session = Session::create(&kube, "fs-test-unit", &pod_spec(&image()).unwrap()).unwrap();
    session.verify().unwrap();
    drop(session);
    assert_eq!(
        *kube.calls.borrow(),
        [
            "create namespace",
            "create pod",
            "wait",
            "logs",
            "delete namespace"
        ]
    );
    for kube in [
        FakeKube {
            phase: Some("Failed"),
            ..FakeKube::default()
        },
        FakeKube {
            log: Some("wrong image"),
            ..FakeKube::default()
        },
    ] {
        {
            let session =
                Session::create(&kube, "fs-test-unit", &pod_spec(&image()).unwrap()).unwrap();
            assert!(session.verify().is_err());
        }
        assert_eq!(kube.calls.borrow().last(), Some(&"delete namespace"));
    }
}

#[test]
fn namespace_switch_is_explicit_and_shared_mode_never_mutates_namespaces() {
    assert_eq!(
        NamespaceMode::parse(None, None).unwrap(),
        NamespaceMode::Create
    );
    assert!(
        NamespaceMode::parse(Some("shared"), None)
            .unwrap_err()
            .contains("FS_TEST_NAMESPACE")
    );
    assert!(NamespaceMode::parse(Some("unknown"), None).is_err());
    assert!(NamespaceMode::parse(Some("shared"), Some("default")).is_err());
    let mode = NamespaceMode::parse(Some("shared"), Some("fs-test-unit")).unwrap();
    let kube = FakeKube::default();
    {
        let _session =
            Session::create_in(&kube, "fs-test-unit", &pod_spec(&image()).unwrap(), &mode).unwrap();
    }
    assert_eq!(*kube.calls.borrow(), ["create pod"]);
}

type Invocation = (String, Vec<String>, Option<Value>);

#[derive(Default)]
struct Commands {
    calls: RefCell<Vec<Invocation>>,
    deadlines: RefCell<Vec<Instant>>,
    fail: bool,
    rootless: bool,
    phase: Option<&'static str>,
}

impl Runner for Commands {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&Value>,
        deadline: Instant,
    ) -> Result<String, String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero());
        self.deadlines.borrow_mut().push(deadline);
        self.calls.borrow_mut().push((
            program.into(),
            args.iter().map(|s| (*s).into()).collect(),
            input.cloned(),
        ));
        if self.fail {
            return Err("credentials or executable unavailable".into());
        }
        if program == "podman" && args.first() == Some(&"info") {
            return Ok(self.rootless.to_string());
        }
        if let Some(index) = args.iter().position(|arg| *arg == "--digestfile") {
            std::fs::write(args[index + 1], format!("sha256:{}\n", "b".repeat(64))).unwrap();
        }
        if args.contains(&"get") {
            return Ok(self.phase.unwrap_or("Succeeded").into());
        }
        if args.contains(&"logs") {
            return Ok("falcone-serverless-worker 0.1.0\n".into());
        }
        Ok(String::new())
    }
}

#[test]
fn registry_adapter_builds_rootless_and_returns_the_pushed_digest() {
    let cli = Cli::new(Commands {
        rootless: true,
        ..Commands::default()
    });
    let image = cli
        .build_push("registry.invalid/team/worker", "fs-test-unit")
        .unwrap();
    assert_eq!(
        image,
        format!("registry.invalid/team/worker@sha256:{}", "b".repeat(64))
    );
    let calls = cli.runner.calls.borrow();
    assert_eq!(
        calls
            .iter()
            .map(|(program, args, _)| (program.as_str(), args[0].as_str()))
            .collect::<Vec<_>>(),
        [
            ("podman", "info"),
            ("podman", "login"),
            ("podman", "build"),
            ("podman", "push")
        ]
    );
    assert!(calls[1].1.contains(&"--get-login".into()));
    assert!(calls[1].1.contains(&"registry.invalid".into()));
    assert!(
        calls[2]
            .1
            .contains(&"registry.invalid/team/worker:fs-test-unit".into())
    );
    assert!(
        calls[3]
            .1
            .contains(&"docker://registry.invalid/team/worker:fs-test-unit".into())
    );
    drop(calls);
    for runner in [
        Commands::default(),
        Commands {
            fail: true,
            ..Commands::default()
        },
    ] {
        let cli = Cli::new(runner);
        assert!(
            cli.build_push("registry.invalid/worker", "fs-test-unit")
                .is_err()
        );
        assert_eq!(cli.runner.calls.borrow().len(), 1);
    }
}

#[test]
fn kubectl_adapter_targets_only_guarded_objects_and_bounds_waits() {
    let cli = Cli::new(Commands::default());
    {
        let session =
            Session::create(&cli, "fs-test-adapter", &pod_spec(&image()).unwrap()).unwrap();
        session.verify().unwrap();
    }
    let calls = cli.runner.calls.borrow();
    assert_eq!(calls.len(), 5);
    assert!(calls.iter().all(|(program, args, _)| program == "kubectl"
        && args.iter().any(|arg| arg.starts_with("--request-timeout="))));
    let namespace = calls[0].2.as_ref().unwrap();
    assert_eq!(namespace["kind"], "Namespace");
    assert_eq!(namespace["metadata"]["name"], "fs-test-adapter");
    let pod = calls[1].2.as_ref().unwrap();
    assert_eq!(pod["metadata"]["namespace"], "fs-test-adapter");
    assert_eq!(pod["metadata"]["name"], "fs-test-adapter");
    assert_eq!(
        pod["metadata"]["labels"]["falcone.test/run"],
        "fs-test-adapter"
    );
    assert!(calls[2].1.contains(&"get".into()));
    assert!(calls[3].1.contains(&"logs".into()));
    assert!(calls[4].1.contains(&"delete".into()));
    drop(calls);
    let ns = Namespace::new("fs-test-adapter").unwrap();
    let cli = Cli::new(Commands {
        phase: Some("Failed"),
        ..Commands::default()
    });
    assert_eq!(
        cli.wait_terminal(
            &ns,
            "fs-test-adapter",
            Instant::now() + Duration::from_secs(1)
        )
        .unwrap(),
        "Failed"
    );
    let cli = Cli::new(Commands::default());
    assert!(
        cli.wait_terminal(&ns, "fs-test-adapter", Instant::now())
            .is_err()
    );
    assert!(
        cli.wait_terminal(&ns, "../default", Instant::now() + Duration::from_secs(1))
            .is_err()
    );
    assert!(
        cli.logs(&ns, "../default", Instant::now() + Duration::from_secs(1))
            .is_err()
    );
    assert!(cli.runner.calls.borrow().is_empty());
}

#[test]
fn every_kube_operation_propagates_and_clamps_its_deadline() {
    let ns = Namespace::new("fs-test-unit").unwrap();
    let pod = json!({"apiVersion":"v1", "kind":"Pod", "metadata": {
        "name":"fs-test-unit", "namespace":"fs-test-unit",
        "labels":{"falcone.test/run":"fs-test-unit"}}, "spec":pod_spec(&image()).unwrap()});
    for expired in [false, true] {
        let cli = Cli::new(Commands::default());
        let deadline = Instant::now()
            + if expired {
                Duration::ZERO
            } else {
                Duration::from_secs(1)
            };
        assert_eq!(cli.create_namespace(&ns, deadline).is_err(), expired);
        assert_eq!(cli.create_pod(&ns, &pod, deadline).is_err(), expired);
        assert_eq!(
            cli.wait_terminal(&ns, "fs-test-unit", deadline).is_err(),
            expired
        );
        assert_eq!(cli.logs(&ns, "fs-test-unit", deadline).is_err(), expired);
        assert_eq!(cli.delete_namespace(&ns, deadline).is_err(), expired);
        let calls = cli.runner.calls.borrow();
        assert_eq!(calls.len(), if expired { 0 } else { 5 });
        assert!(cli.runner.deadlines.borrow().iter().all(|d| *d == deadline));
        for (_, args, _) in calls.iter() {
            let millis = args
                .iter()
                .find_map(|arg| arg.strip_prefix("--request-timeout="))
                .unwrap()
                .strip_suffix("ms")
                .unwrap()
                .parse::<u128>()
                .unwrap();
            assert!((1..=1000).contains(&millis));
        }
    }
}

#[test]
fn disabled_cluster_mode_never_invokes_tools() {
    let cli = Cli::new(Commands {
        fail: true,
        ..Commands::default()
    });
    cli.execute(&Config::Disabled, &NamespaceMode::Create)
        .unwrap();
    assert!(cli.runner.calls.borrow().is_empty());
}

#[test]
fn enabled_cluster_flow_builds_pushes_runs_and_cleans_up() {
    let cli = Cli::new(Commands {
        rootless: true,
        ..Commands::default()
    });
    cli.execute(
        &Config::Enabled {
            repo: "registry.invalid/worker".into(),
        },
        &NamespaceMode::Create,
    )
    .unwrap();
    let calls = cli.runner.calls.borrow();
    let namespace = calls
        .iter()
        .find_map(|(_, _, input)| input.as_ref().filter(|v| v["kind"] == "Namespace"))
        .unwrap();
    let name = namespace["metadata"]["name"].as_str().unwrap();
    assert!(Namespace::new(name).is_ok());
    let pod = calls
        .iter()
        .find_map(|(_, _, input)| input.as_ref().filter(|v| v["kind"] == "Pod"))
        .unwrap();
    assert_eq!(
        pod["spec"]["containers"][0]["image"],
        format!("registry.invalid/worker@sha256:{}", "b".repeat(64))
    );
    assert_eq!(pod["metadata"]["namespace"], name);
    assert_eq!(pod["metadata"]["labels"]["falcone.test/run"], name);
    assert!(calls.last().unwrap().1.contains(&"delete".into()));
}

#[test]
fn subprocess_errors_suppress_output_and_name_ambient_prerequisites() {
    let runner = Process;
    let error = runner
        .run(
            "sh",
            &["-c", "printf output-canary >&2; exit 1"],
            None,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap_err();
    assert!(!error.contains("output-canary"));
    assert!(error.contains("ambient"));
    assert_eq!(
        runner
            .run(
                "sh",
                &["-c", "printf ready"],
                None,
                Instant::now() + Duration::from_secs(2)
            )
            .unwrap(),
        "ready"
    );
    assert!(
        runner
            .run(
                "/nonexistent-fs-test-executable",
                &[],
                None,
                Instant::now() + Duration::from_secs(2)
            )
            .is_err()
    );
}

struct ProcessFixture(std::path::PathBuf);

impl ProcessFixture {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(namespace_name());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn subprocess_deadline_kills_and_reaps_the_entire_group() {
    let fixture = ProcessFixture::new();
    let pids = fixture.0.join("pids");
    let start = Instant::now();
    let result = Process.run(
        "sh",
        &[
            "-c",
            "sleep 30 & echo \"$$ $!\" > \"$1\"; wait",
            "sh",
            pids.to_str().unwrap(),
        ],
        None,
        start + Duration::from_millis(200),
    );
    assert!(result.is_err(), "hanging child must time out");
    assert!(start.elapsed() < Duration::from_secs(2));
    let pids = std::fs::read_to_string(pids).unwrap();
    for pid in pids.split_whitespace() {
        let pid = rustix::process::Pid::from_raw(pid.parse().unwrap()).unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH),
            "process {pid} remains"
        );
    }
}

#[test]
fn subprocess_drains_a_full_stdout_pipe() {
    let output = Process
        .run(
            "sh",
            &["-c", "head -c 1048576 /dev/zero"],
            None,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(output.len(), 1_048_576);
    assert!(output.bytes().all(|byte| byte == 0));
}

#[test]
fn subprocess_deadline_covers_blocked_stdin_and_inherited_stdout() {
    for script in ["sleep 30", "sleep 30 & exit 0"] {
        let input = json!("x".repeat(1_048_576));
        let start = Instant::now();
        assert!(
            Process
                .run(
                    "sh",
                    &["-c", script],
                    Some(&input),
                    start + Duration::from_millis(200)
                )
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[test]
fn expired_subprocess_deadline_never_spawns() {
    let fixture = ProcessFixture::new();
    let marker = fixture.0.join("spawned");
    assert!(
        Process
            .run(
                "sh",
                &[
                    "-c",
                    "printf spawned > \"$1\"",
                    "sh",
                    marker.to_str().unwrap()
                ],
                None,
                Instant::now()
            )
            .is_err()
    );
    assert!(!marker.exists());
}

#[test]
fn explicit_cleanup_reports_failure_without_deleting_twice() {
    for fail_delete in [false, true] {
        let kube = FakeKube {
            fail_delete,
            ..FakeKube::default()
        };
        let session = Session::create(&kube, "fs-test-unit", &pod_spec(&image()).unwrap()).unwrap();
        assert_eq!(session.close().is_err(), fail_delete);
        assert_eq!(
            kube.calls
                .borrow()
                .iter()
                .filter(|call| **call == "delete namespace")
                .count(),
            1
        );
    }
}

#[test]
fn cluster_worker_image() -> Result<(), String> {
    let cluster = std::env::var("FS_TEST_CLUSTER").ok();
    let repo = std::env::var("FS_TEST_IMAGE_REPO").ok();
    let config = Config::parse(cluster.as_deref(), repo.as_deref())?;
    if config == Config::Disabled {
        return Ok(());
    }
    let mode = std::env::var("FS_TEST_NAMESPACE_MODE").ok();
    let namespace = std::env::var("FS_TEST_NAMESPACE").ok();
    let mode = NamespaceMode::parse(mode.as_deref(), namespace.as_deref())?;
    Cli::new(Process).execute(&config, &mode)
}
