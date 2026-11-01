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
use std::time::Duration;
use support::cli::{Cli, Process, Runner, kill_tree};
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
    fail_pod: bool,
    fail_namespace: bool,
    fail_delete: bool,
    phase: Option<&'static str>,
    log: Option<&'static str>,
}

impl Kube for FakeKube {
    fn create_namespace(&self, _: &Namespace) -> Result<(), String> {
        self.calls.borrow_mut().push("create namespace");
        if self.fail_namespace {
            Err("namespace denied".into())
        } else {
            Ok(())
        }
    }
    fn create_pod(&self, _: &Namespace, pod: &Value) -> Result<(), String> {
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
    fn wait_terminal(&self, _: &Namespace, _: &str, timeout: Duration) -> Result<String, String> {
        self.calls.borrow_mut().push("wait");
        assert!(timeout > Duration::ZERO && timeout <= Duration::from_secs(180));
        Ok(self.phase.unwrap_or("Succeeded").into())
    }
    fn logs(&self, _: &Namespace, _: &str) -> Result<String, String> {
        self.calls.borrow_mut().push("logs");
        Ok(self
            .log
            .unwrap_or("falcone-serverless-worker 0.1.0\n")
            .into())
    }
    fn delete_namespace(&self, _: &Namespace) -> Result<(), String> {
        self.calls.borrow_mut().push("delete namespace");
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
fn init_containers_need_digest_pinned_images_before_any_client_call() {
    let with_init = |init_image: String| {
        let mut spec = pod_spec(&image()).unwrap();
        let mut init = spec["containers"][0].clone();
        init["name"] = json!("init");
        init["image"] = json!(init_image);
        spec["initContainers"] = json!([init]);
        spec
    };
    for unpinned in [
        "repo:latest".to_string(),
        format!("repo@sha256:{}", "a".repeat(63)),
    ] {
        let kube = FakeKube::default();
        let error = Session::create(&kube, "fs-test-unit", &with_init(unpinned.clone()))
            .err()
            .expect("refused");
        assert!(error.contains("digest-pinned"), "{unpinned}: {error}");
        assert!(kube.calls.borrow().is_empty(), "{unpinned}");
    }
    let kube = FakeKube::default();
    Session::create(&kube, "fs-test-unit", &with_init(image())).unwrap();
    assert_eq!(kube.calls.borrow().first(), Some(&"create namespace"));
    let mut spec = pod_spec(&image()).unwrap();
    spec["ephemeralContainers"] = json!([{"name": "debug", "image": image()}]);
    let kube = FakeKube::default();
    assert!(Session::create(&kube, "fs-test-unit", &spec).is_err());
    assert!(kube.calls.borrow().is_empty());
}

#[test]
fn cli_refuses_unpinned_init_containers_before_any_runner_call() {
    let cli = Cli::new(Commands::default());
    let ns = Namespace::new("fs-test-adapter").unwrap();
    let pod_with_init = |init_image: &str| {
        let mut spec = pod_spec(&image()).unwrap();
        let mut init = spec["containers"][0].clone();
        init["name"] = json!("init");
        init["image"] = json!(init_image);
        spec["initContainers"] = json!([init]);
        json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "fs-test-adapter",
                "namespace": "fs-test-adapter",
                "labels": {"falcone.test/run": "fs-test-adapter"},
            },
            "spec": spec,
        })
    };
    let error = cli
        .create_pod(&ns, &pod_with_init("repo:latest"))
        .expect_err("unpinned initContainer image is refused");
    assert!(error.contains("digest-pinned"), "{error}");
    assert!(cli.runner.calls.borrow().is_empty());
    cli.create_pod(&ns, &pod_with_init(&image())).unwrap();
    assert_eq!(cli.runner.calls.borrow().len(), 1);
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
        timeout: Duration,
    ) -> Result<String, String> {
        assert!(timeout > Duration::ZERO && timeout <= Duration::from_secs(3600));
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
        cli.wait_terminal(&ns, "fs-test-adapter", Duration::from_secs(1))
            .unwrap(),
        "Failed"
    );
    let cli = Cli::new(Commands::default());
    assert!(
        cli.wait_terminal(&ns, "fs-test-adapter", Duration::ZERO)
            .is_err()
    );
    assert!(
        cli.wait_terminal(&ns, "../default", Duration::from_secs(1))
            .is_err()
    );
    assert!(cli.logs(&ns, "../default").is_err());
    assert!(cli.runner.calls.borrow().is_empty());
}

#[test]
fn every_kubectl_call_gets_a_bounded_deadline_and_cleanup_its_own() {
    struct Recording(RefCell<Vec<(String, Duration)>>);
    impl Runner for Recording {
        fn run(
            &self,
            _: &str,
            args: &[&str],
            _: Option<&Value>,
            timeout: Duration,
        ) -> Result<String, String> {
            let verb = ["create", "get", "logs", "delete"]
                .into_iter()
                .find(|verb| args.contains(verb))
                .unwrap();
            self.0.borrow_mut().push((verb.into(), timeout));
            Ok("Succeeded".into())
        }
    }
    let cli = Cli::new(Recording(RefCell::default()));
    let ns = Namespace::new("fs-test-adapter").unwrap();
    cli.wait_terminal(&ns, "fs-test-adapter", Duration::from_secs(5))
        .unwrap();
    cli.logs(&ns, "fs-test-adapter").unwrap();
    cli.delete_namespace(&ns).unwrap();
    let calls = cli.runner.0.borrow();
    assert!(calls[0].1 <= Duration::from_secs(5), "{calls:?}");
    assert!(calls[1].1 > Duration::ZERO && calls[1].1 <= Duration::from_secs(60));
    // Cleanup does not inherit a spent budget: it always has a full, documented allowance.
    assert!(calls[2].1 >= Duration::from_secs(60) && calls[2].1 <= Duration::from_secs(300));
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

const LONG: Duration = Duration::from_secs(30);

#[test]
fn subprocess_errors_suppress_output_and_name_ambient_prerequisites() {
    let runner = Process;
    let error = runner
        .run(
            "sh",
            &["-c", "printf output-canary >&2; exit 1"],
            None,
            LONG,
        )
        .unwrap_err();
    assert!(!error.contains("output-canary"));
    assert!(error.contains("ambient"));
    assert_eq!(
        runner
            .run("sh", &["-c", "printf ready"], None, LONG)
            .unwrap(),
        "ready"
    );
    assert_eq!(
        runner
            .run("sh", &["-c", "cat"], Some(&json!({"k": "v"})), LONG)
            .unwrap(),
        r#"{"k":"v"}"#
    );
    assert!(
        runner
            .run("/nonexistent-fs-test-executable", &[], None, LONG)
            .is_err()
    );
}

fn scratch_file(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("fs-test-{name}-{}", std::process::id()))
}

#[test]
fn subprocess_ignoring_sigterm_is_killed_and_reaped_at_the_deadline() {
    let pidfile = scratch_file("sigterm");
    let started = std::time::Instant::now();
    let error = Process
        .run(
            "sh",
            &[
                "-c",
                "echo $$ > \"$0\"; trap '' TERM; exec sleep 30",
                pidfile.to_str().unwrap(),
            ],
            None,
            Duration::from_millis(500),
        )
        .unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid = std::fs::read_to_string(&pidfile).unwrap();
    let _ = std::fs::remove_file(&pidfile);
    // A reaped process has no /proc entry; an unreaped one would linger as a zombie.
    assert!(!std::path::Path::new(&format!("/proc/{}", pid.trim())).exists());
}

struct HangingKube {
    calls: RefCell<Vec<&'static str>>,
}

impl Kube for HangingKube {
    fn create_namespace(&self, _: &Namespace) -> Result<(), String> {
        Ok(())
    }
    fn create_pod(&self, _: &Namespace, _: &Value) -> Result<(), String> {
        Ok(())
    }
    fn wait_terminal(&self, _: &Namespace, _: &str, _: Duration) -> Result<String, String> {
        self.calls.borrow_mut().push("wait");
        Process.run(
            "sh",
            &["-c", "setsid sleep 30 & exit 0"],
            None,
            Duration::from_millis(200),
        )
    }
    fn logs(&self, _: &Namespace, _: &str) -> Result<String, String> {
        Ok(String::new())
    }
    fn delete_namespace(&self, _: &Namespace) -> Result<(), String> {
        self.calls.borrow_mut().push("delete namespace");
        Ok(())
    }
}

#[test]
fn detached_descendant_holding_the_pipes_cannot_stall_run_or_cleanup() {
    let kube = HangingKube {
        calls: RefCell::default(),
    };
    let started = std::time::Instant::now();
    {
        let session = Session::create(&kube, "fs-test-unit", &pod_spec(&image()).unwrap()).unwrap();
        assert!(session.verify().is_err());
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(*kube.calls.borrow(), ["wait", "delete namespace"]);
}

#[test]
fn detached_descendant_is_killed_where_it_can_be_found() {
    let pidfile = scratch_file("detached");
    let script = "setsid sh -c 'echo $$ > \"$0\"; exec sleep 30' \"$0\" & exit 0";
    let result = Process.run(
        "sh",
        &["-c", script, pidfile.to_str().unwrap()],
        None,
        Duration::from_millis(500),
    );
    assert!(result.unwrap_err().contains("timed out"));
    let pid = std::fs::read_to_string(&pidfile).unwrap();
    let _ = std::fs::remove_file(&pidfile);
    let proc = format!("/proc/{}/stat", pid.trim());
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    // Killed; the zombie (if any) is reaped by the init process, so allow it a moment.
    while std::fs::read_to_string(&proc).is_ok_and(|stat| !stat.contains(") Z ")) {
        assert!(std::time::Instant::now() < deadline, "descendant survived");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Kills a helper the code under test may have failed to kill, even when an assertion fails.
struct Helper(std::path::PathBuf);

impl Helper {
    fn pid(&self) -> Option<String> {
        std::fs::read_to_string(&self.0)
            .ok()
            .map(|pid| pid.trim().to_string())
            .filter(|pid| !pid.is_empty())
    }

    fn alive(&self) -> bool {
        self.pid().is_some_and(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
        })
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if let Some(pid) = self.pid() {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid])
                .stderr(std::process::Stdio::null())
                .status();
        }
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn environment_clearing_descendant_that_left_the_group_cannot_stall_run_or_cleanup() {
    // Neither the process group nor the run marker reaches this helper, so discovery fails and
    // the pipe readers must be abandoned within the documented grace.
    let helper = Helper(scratch_file("escaped"));
    let script = "env -i setsid sh -c 'echo $$ > \"$0\"; exec sleep 30' \"$0\" & exit 0";
    let started = std::time::Instant::now();
    let error = Process
        .run(
            "sh",
            &["-c", script, helper.0.to_str().unwrap()],
            None,
            Duration::from_millis(200),
        )
        .unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    // Cleanup after the abandoned readers still runs.
    let kube = FakeKube::default();
    {
        let session = Session::create(&kube, "fs-test-unit", &pod_spec(&image()).unwrap()).unwrap();
        drop(session);
    }
    assert!(kube.calls.borrow().contains(&"delete namespace"));
}

#[test]
fn environment_clearing_descendant_left_in_the_group_is_killed_after_the_parent_exits() {
    let helper = Helper(scratch_file("grouped"));
    let script = "env -i sh -c 'echo $$ > \"$0\"; exec sleep 30' \"$0\" & exit 0";
    let started = std::time::Instant::now();
    let error = Process
        .run(
            "sh",
            &["-c", script, helper.0.to_str().unwrap()],
            None,
            Duration::from_millis(200),
        )
        .unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(3));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    // The zombie (if any) is reaped by the init process, so allow it a moment.
    while helper.alive() {
        assert!(std::time::Instant::now() < deadline, "descendant survived");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn child_is_reaped_even_when_the_cleanup_budget_is_already_spent() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    // Discovery gets no time at all; the killed child must still be reaped (SIGKILL is
    // asynchronous, so the reaping allowance after the signal covers the delivery).
    kill_tree(
        &mut child,
        "fs-test-spent-budget",
        std::time::Instant::now(),
    );
    // A zombie keeps its /proc entry until somebody reaps it.
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn hung_foreground_command_times_out_within_the_deadline_and_grace() {
    let started = std::time::Instant::now();
    let error = Process
        .run("sleep", &["30"], None, Duration::from_millis(200))
        .unwrap_err();
    assert!(error.contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(2));
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
