use super::evidence::{Reason, Report, Verdict};
use super::was01_report::{
    Output, ProcessKubectl, ReportGate, Runner, gate, plan, plan_safe, probe_permissions,
    probe_version, run,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[test]
fn plan_accepts_only_the_explicit_allow_list() {
    let namespace =
        super::namespace::Namespace::new(&super::namespace::namespace_name("was01")).unwrap();
    assert!(namespace.name().starts_with("fs-test-was01-"));
    let commands = plan(&namespace);
    assert_eq!(commands.len(), 15);
    assert!(plan_safe(&namespace, &commands));
    for bad in [
        "get services -n default",
        "get services -n in-falcone-staging",
        "get services -A",
        "get services",
        "get nodes -n SELF",
        "get clusterversion -n SELF",
        "create clusterrole -n SELF",
        "create crd -n SELF",
        "create runtimeclass -n SELF",
        "create scc -n SELF",
        "get services -n SELF runtimeClassName=x",
        "get services --namespace other",
        "get services --namespace=other",
        "get services -n=other",
        "get services -n SELF -n SELF",
        "get --raw /api",
        "get --raw /version -n SELF",
        "auth can-i get nodes -n SELF",
        "delete namespace other --wait",
        "apply -n SELF -f x",
        "auth can-i get services -n SELF --as admin",
        "get services --all-namespaces",
    ] {
        let command = bad
            .replace("SELF", namespace.name())
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        assert!(!plan_safe(&namespace, &[command]), "{bad}");
    }
}

#[test]
fn rejected_plans_record_failed_evidence_before_returning_an_error() {
    let namespace = super::Namespace::new("fs-test-was01-rejected").unwrap();
    let commands = vec![vec!["get".into(), "nodes".into()]];
    let mut report = Report::new();
    assert_eq!(
        super::was01_report::check_plan(&mut report, &namespace, &commands),
        Err("plan-rejected")
    );
    let item = report.item("plan-namespaced-only").unwrap();
    assert_eq!(
        (item.verdict, item.reason),
        (Verdict::Failed, Reason::RejectedPlan)
    );
    assert_eq!(Report::parse(&report.render()).unwrap(), report);
    assert!(super::was01_report::check_plan(&mut report, &namespace, &plan(&namespace)).is_ok());
    assert_eq!(
        report.item("plan-namespaced-only").unwrap().verdict,
        Verdict::Passed
    );
}

#[test]
fn items_require_evidence_and_known_ids() {
    let mut report = Report::new();
    let expected: std::collections::BTreeSet<_> = [
        "kubernetes-version",
        "plan-namespaced-only",
        "openshift-4.20",
        "pin-engine",
        "pin-architecture",
        "pin-wasi",
        "pin-compiler",
        "guest-install",
        "guest-execute",
        "guest-interrupt",
        "can-get-deployments.apps",
        "can-create-deployments.apps",
        "can-update-deployments.apps",
        "can-delete-deployments.apps",
        "can-get-services",
        "can-create-services",
        "can-update-services",
        "can-delete-services",
        "can-get-leases.coordination.k8s.io",
        "can-create-leases.coordination.k8s.io",
        "can-update-leases.coordination.k8s.io",
        "can-delete-leases.coordination.k8s.io",
    ]
    .into();
    assert_eq!(
        report.ids().collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    for (ids, reason) in [
        (
            vec!["pin-engine", "pin-architecture", "pin-wasi", "pin-compiler"],
            Reason::NoPinSelected,
        ),
        (
            vec!["guest-install", "guest-execute", "guest-interrupt"],
            Reason::NotCollected,
        ),
        (vec!["openshift-4.20"], Reason::OpenshiftNotAvailable),
    ] {
        for id in ids {
            let item = report.item(id).unwrap();
            assert_eq!((item.verdict, item.reason), (Verdict::Unverified, reason));
        }
    }
    assert_eq!(
        report.item("kubernetes-version").unwrap().verdict,
        Verdict::Unverified
    );
    assert!(
        report
            .record(
                "kubernetes-version",
                Verdict::Passed,
                Reason::Recorded,
                None
            )
            .is_err()
    );
    assert!(
        report
            .record("unknown", Verdict::Unverified, Reason::NotProbed, None)
            .is_err()
    );
    report
        .record(
            "kubernetes-version",
            Verdict::Passed,
            Reason::Recorded,
            Some("v1.37.2"),
        )
        .unwrap();
    assert_eq!(
        report
            .item("kubernetes-version")
            .unwrap()
            .evidence
            .as_deref(),
        Some("v1.37.2")
    );
}

#[test]
fn ac3_failures_remain_visible_without_a_recorded_version() {
    let mut report = Report::new();
    report
        .record(
            "can-get-services",
            Verdict::Failed,
            Reason::ProbeError,
            None,
        )
        .unwrap();
    for target in ["k8s-1.37", "k8s-1.36"] {
        assert_eq!(report.cell(target).unwrap(), Verdict::Failed);
    }
    assert_eq!(report.cell("openshift-4.20").unwrap(), Verdict::Unverified);
    assert_eq!(Report::parse(&report.render()).unwrap(), report);
    report
        .record(
            "kubernetes-version",
            Verdict::Passed,
            Reason::Recorded,
            Some("v1.35.0"),
        )
        .unwrap();
    for target in super::evidence::TARGETS {
        assert_eq!(report.cell(target).unwrap(), Verdict::Unverified);
    }
}

#[test]
fn versions_round_trip_and_cells_aggregate_only_matching_evidence() {
    // The template must parse with a valid version before testing invalid replacements.
    for version in ["v1.35.0", "v1.37.2", "v1.36.4+k3s1"] {
        assert!(Report::parse(&version_report_text(version)).is_ok());
    }
    for (version, target) in [("v1.37.2", "k8s-1.37"), ("v1.36.4+k3s1", "k8s-1.36")] {
        let mut report = Report::new();
        report
            .record(
                "kubernetes-version",
                Verdict::Passed,
                Reason::Recorded,
                Some(version),
            )
            .unwrap();
        for id in Report::ac3_ids()
            .into_iter()
            .filter(|id| id != "kubernetes-version")
        {
            report
                .record(&id, Verdict::Passed, Reason::Allowed, Some("yes"))
                .unwrap();
        }
        assert_eq!(report.cell(target).unwrap(), Verdict::Passed);
        assert_eq!(Report::parse(&report.render()).unwrap(), report);
        report
            .record(
                "can-get-services",
                Verdict::Unverified,
                Reason::Denied,
                None,
            )
            .unwrap();
        assert_eq!(report.cell(target).unwrap(), Verdict::Unverified);
        report
            .record(
                "can-get-services",
                Verdict::Failed,
                Reason::ProbeError,
                None,
            )
            .unwrap();
        assert_eq!(report.cell(target).unwrap(), Verdict::Failed);
    }
    for version in [
        "1.37.2",
        "v1.37",
        " v1.37.2",
        "v1.37.2\n",
        "v1.37.2/token",
        "v1.37.2\t",
        "v1.37.2+",
        "v1.37.2-",
        "v1.37.2..0",
    ] {
        let mut report = Report::new();
        assert!(
            report
                .record(
                    "kubernetes-version",
                    Verdict::Passed,
                    Reason::Recorded,
                    Some(version)
                )
                .is_err(),
            "{version:?}"
        );
        assert_eq!(report.item("kubernetes-version").unwrap().evidence, None);
        assert!(
            Report::parse(&version_report_text(version)).is_err(),
            "{version:?}"
        );
    }
    assert!(
        Report::new()
            .record(
                "kubernetes-version",
                Verdict::Passed,
                Reason::Recorded,
                Some(&format!("v1.37.2+{}", "a".repeat(200)))
            )
            .is_err()
    );
    assert!(Report::parse("unknown\tunverified\tnot-probed\t-\n").is_err());
    assert!(Report::parse("broken line\n").is_err());
    assert!(Report::parse("").is_err());
    assert!(Report::new().cell("unknown").is_err());
}

// Keep cells and summary consistent with a recorded version. Only the evidence field
// changes, so rejecting a malformed version cannot be caused by a stale summary.
fn version_report_text(version: &str) -> String {
    let mut report = Report::new();
    report
        .record(
            "kubernetes-version",
            Verdict::Passed,
            Reason::Recorded,
            Some("v1.35.0"),
        )
        .unwrap();
    report.render().replace(
        "kubernetes-version\tpassed\trecorded\tv1.35.0\n",
        &format!("kubernetes-version\tpassed\trecorded\t{version}\n"),
    )
}

#[test]
fn valid_unmatched_versions_remain_recorded_without_qualifying_any_cell() {
    for version in ["v1.35.0", "v1.370.0", "v2.37.0"] {
        let mut report = Report::new();
        report
            .record(
                "kubernetes-version",
                Verdict::Passed,
                Reason::Recorded,
                Some(version),
            )
            .unwrap();
        assert_eq!(
            report
                .item("kubernetes-version")
                .unwrap()
                .evidence
                .as_deref(),
            Some(version)
        );
        for target in super::evidence::TARGETS {
            assert_eq!(report.cell(target).unwrap(), Verdict::Unverified);
        }
        assert_eq!(Report::parse(&report.render()).unwrap(), report);
        assert!(!report.render().contains("unsupported-version"));
    }
}

struct PrivateDir(PathBuf);
impl PrivateDir {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let name = super::namespace::namespace_name("was01");
        let path = std::env::temp_dir().join(name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
    fn config(&self) -> super::Kubeconfigs {
        let path = self.0.join("config");
        std::fs::write(&path, crate::kubeconfig_text("https://127.0.0.1:1", "u")).unwrap();
        super::Kubeconfigs::parse(path.to_str()).unwrap()
    }
    fn script(&self, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        // Publish a fresh executable only after closing its writer. Never rewrite an
        // executable that another invocation may still have open.
        let path = self.0.join(super::namespace::namespace_name("kubectl"));
        let staging = path.with_extension("writing");
        // Assert explicit kubeconfig selection on every real process invocation.
        std::fs::write(
            &staging,
            format!("#!/bin/sh\ncase \"$1\" in --kubeconfig=*) ;; *) exit 99 ;; esac\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::rename(staging, &path).unwrap();
        path
    }
}
impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn process(dir: &PrivateDir, script: &Path, timeout: Duration) -> ProcessKubectl {
    ProcessKubectl::new(script, &dir.config(), timeout).unwrap()
}

#[test]
fn process_retains_the_explicit_kubeconfig_for_its_lifetime() {
    let dir = PrivateDir::new();
    let script = dir.script("test -f \"${1#--kubeconfig=}\" || exit 98; echo yes");
    // process() drops its temporary Kubeconfigs before the command is run.
    let runner = process(&dir, &script, Duration::from_secs(3));
    let namespace = super::Namespace::new("fs-test-was01-config-lifetime").unwrap();
    let mut report = Report::new();
    probe_permissions(&mut report, &runner, &namespace).unwrap();
    for id in Report::ac3_ids()
        .into_iter()
        .filter(|id| id != "kubernetes-version")
    {
        assert_eq!(report.item(&id).unwrap().reason, Reason::Allowed);
    }
}

#[test]
fn process_forbidden_is_classified_from_stderr_without_exposing_it() {
    let dir = PrivateDir::new();
    let script =
        dir.script("echo 'Error from server (Forbidden): token=SECRET_TOKEN_abc' >&2; exit 1");
    let runner = process(&dir, &script, Duration::from_secs(3));
    let namespace = super::Namespace::new("fs-test-was01-process").unwrap();
    let output = runner.run(&plan(&namespace)[1]).unwrap();
    assert_eq!(output.stderr, Some(Reason::Forbidden));
    assert!(!format!("{output:?}").contains("SECRET_TOKEN_abc"));
    let mut report = Report::new();
    probe_version(&mut report, &runner).unwrap();
    probe_permissions(&mut report, &runner, &namespace).unwrap();
    for id in Report::ac3_ids() {
        let item = report.item(&id).unwrap();
        assert_eq!(
            (item.verdict, item.reason),
            (Verdict::Unverified, Reason::Forbidden)
        );
    }
    for text in [report.render(), format!("{report:?}")] {
        assert!(!text.contains("SECRET_TOKEN_abc"));
    }
}

#[test]
fn process_controls_never_turn_diagnostics_into_permission_evidence() {
    let namespace = super::Namespace::new("fs-test-was01-controls").unwrap();
    for (body, expected) in [
        (
            "echo 'Error from server (Forbidden): x'; exit 1",
            Reason::ProbeError,
        ),
        ("echo 'unrelated failure' >&2; exit 1", Reason::ProbeError),
        (
            "echo 'prefix Error from server (Forbidden): x' >&2; exit 1",
            Reason::ProbeError,
        ),
        (
            "echo 'the word Forbidden is unrelated' >&2; exit 1",
            Reason::ProbeError,
        ),
        ("echo yes; exit 1", Reason::ProbeError),
        ("echo no; exit 1", Reason::Denied),
        ("echo 'no - RBAC: x'; exit 1", Reason::Denied),
        ("echo nope; exit 1", Reason::ProbeError),
        ("echo no-op; exit 1", Reason::ProbeError),
        ("echo yes", Reason::Allowed),
        (
            "echo yes; echo 'Error from server (Forbidden): x' >&2",
            Reason::Forbidden,
        ),
    ] {
        let dir = PrivateDir::new();
        let script = dir.script(body);
        let runner = process(&dir, &script, Duration::from_secs(3));
        let mut report = Report::new();
        probe_permissions(&mut report, &runner, &namespace).unwrap();
        for id in Report::ac3_ids()
            .into_iter()
            .filter(|id| id != "kubernetes-version")
        {
            let item = report.item(&id).unwrap();
            assert_eq!(item.reason, expected, "{body}");
            assert_eq!(
                item.verdict,
                if expected == Reason::Allowed {
                    Verdict::Passed
                } else {
                    Verdict::Unverified
                }
            );
        }
        if expected == Reason::ProbeError {
            probe_version(&mut report, &runner).unwrap();
            assert_eq!(
                report.item("kubernetes-version").unwrap().reason,
                expected,
                "{body}"
            );
        }
        assert!(!report.render().contains("unrelated"));
        assert!(!report.render().contains("RBAC: x"));
    }
}

#[test]
fn process_cleanup_and_probes_respect_the_configured_call_timeout() {
    let dir = PrivateDir::new();
    let script = dir.script("sleep 1; exit 0");
    let runner = process(&dir, &script, Duration::from_millis(200));
    let namespace = super::Namespace::new("fs-test-was01-cleanup").unwrap();
    let commands = plan(&namespace);
    let start = Instant::now();
    assert_eq!(
        runner.run(commands.last().unwrap()).unwrap_err(),
        Reason::ProbeError
    );
    assert!(start.elapsed() >= Duration::from_millis(200));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(runner.run(&commands[1]).unwrap_err(), Reason::ProbeError);
}

#[test]
fn process_drains_overflow_and_kills_a_hung_child_at_the_deadline() {
    let dir = PrivateDir::new();
    let script = dir.script("head -c 1048576 /dev/zero | tr '\\000' x >&2; echo '\nError from server (Forbidden): token=SECRET_TOKEN_abc' >&2; exit 1");
    let runner = process(&dir, &script, Duration::from_secs(3));
    let mut report = Report::new();
    let started = Instant::now();
    probe_version(&mut report, &runner).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "finite stderr flood must drain before the 3-second deadline"
    );
    assert_eq!(
        report.item("kubernetes-version").unwrap().reason,
        Reason::ProbeError
    );
    assert!(!report.render().contains("SECRET_TOKEN_abc"));
    let pid_file = dir.0.join("pid");
    let script = dir.script(&format!(
        "echo $$ > '{}'; exec sleep 30",
        pid_file.display()
    ));
    let runner = process(&dir, &script, Duration::from_secs(3));
    let start = Instant::now();
    let (pid, error) = std::thread::scope(|scope| {
        let child = scope.spawn(|| runner.run(&["get".into(), "--raw".into(), "/version".into()]));
        let pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                && pid.trim().parse::<i32>().is_ok()
            {
                break pid;
            }
            assert!(!child.is_finished(), "hung child must signal readiness");
            std::thread::sleep(Duration::from_millis(10));
        };
        (pid, child.join().unwrap().unwrap_err())
    });
    assert_eq!(error, Reason::ProbeError);
    assert!(start.elapsed() >= Duration::from_secs(3));
    assert!(start.elapsed() < Duration::from_secs(10));
    let pid = rustix::process::Pid::from_raw(pid.trim().parse().unwrap()).unwrap();
    // /proc can belong to the host PID namespace; signal zero uses our child's namespace.
    assert_eq!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH),
        "hung child survived"
    );
}

struct FakeRunner {
    calls: RefCell<Vec<Vec<String>>>,
    scenario: &'static str,
}
impl FakeRunner {
    fn new(scenario: &'static str) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            scenario,
        }
    }
}
impl Runner for FakeRunner {
    fn run(&self, args: &[String]) -> Result<Output, Reason> {
        self.calls.borrow_mut().push(args.to_vec());
        let first = args.first().map(String::as_str);
        if first == Some("create") && ["create-failure", "already-exists"].contains(&self.scenario)
        {
            return Ok(Output {
                exit_code: Some(1),
                stdout: if self.scenario == "already-exists" {
                    "AlreadyExists token=SECRET_TOKEN_abc".into()
                } else {
                    String::new()
                },
                stderr: Some(Reason::ProbeError),
            });
        }
        if first == Some("delete")
            && ["delete-error", "panic-delete-error"].contains(&self.scenario)
        {
            return Err(Reason::ProbeError);
        }
        if first == Some("get") {
            if ["panic", "panic-delete-error"].contains(&self.scenario) {
                panic!("injected step panic");
            }
            if self.scenario == "late-error" {
                return Err(Reason::ProbeError);
            }
            if self.scenario == "forbidden" {
                return Ok(Output {
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: Some(Reason::Forbidden),
                });
            }
            let version = match self.scenario {
                "k3s" => "v1.36.4+k3s1",
                "unmatched" => "v1.35.0",
                "invalid-version" => "1.37.2",
                _ => "v1.37.2",
            };
            return Ok(Output {
                exit_code: Some(0),
                stdout: serde_json::json!({"gitVersion": version}).to_string(),
                stderr: None,
            });
        }
        if first == Some("auth") {
            match self.scenario {
                "permission-error" => return Err(Reason::ProbeError),
                "forbidden" => {
                    return Ok(Output {
                        exit_code: Some(1),
                        stdout: String::new(),
                        stderr: Some(Reason::Forbidden),
                    });
                }
                "denied" => {
                    return Ok(Output {
                        exit_code: Some(1),
                        stdout: "no".into(),
                        stderr: None,
                    });
                }
                _ => (),
            }
        }
        if self.scenario == "all-errors" {
            return Err(Reason::ProbeError);
        }
        Ok(Output {
            exit_code: Some(0),
            stdout: "yes".into(),
            stderr: None,
        })
    }
}

#[test]
fn gate_requires_both_switches_and_rejects_shared_or_invalid_config_before_calls() {
    let dir = PrivateDir::new();
    dir.config();
    let file = dir.0.join("config");
    for was01 in [None, Some("0"), Some("true"), Some("1")] {
        for cluster in [None, Some("0"), Some("true"), Some("1")] {
            let enabled = was01 == Some("1") && cluster == Some("1");
            let result = gate(was01, cluster, None, file.to_str()).unwrap();
            assert_eq!(matches!(result, ReportGate::Cluster(_)), enabled);
        }
    }
    for (mode, config) in [
        (Some("shared"), file.to_str()),
        (Some("invalid"), file.to_str()),
        (None, Some(":")),
        (None, None),
    ] {
        let fake = FakeRunner::new("happy");
        let result =
            gate(Some("1"), Some("1"), mode, config).and_then(|gate| run(&fake, &gate, &dir.0));
        assert!(result.is_err());
        assert!(fake.calls.borrow().is_empty());
    }
}

#[test]
fn offline_report_has_no_calls_and_keeps_every_missing_item_in_the_summary() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let fake = FakeRunner::new("happy");
    let path = run(&fake, &ReportGate::Offline, dir).unwrap();
    assert!(path.starts_with(dir));
    assert!(fake.calls.borrow().is_empty());
    let report = Report::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
    for id in Report::ac3_ids() {
        let item = report.item(&id).unwrap();
        assert_eq!(
            (item.verdict, item.reason),
            (Verdict::Unverified, Reason::NotProbed)
        );
        assert!(report.summary().contains(&id));
    }
    for (id, reason) in super::evidence::FIXED {
        let item = report.item(id).unwrap();
        assert_eq!((item.verdict, item.reason), (Verdict::Unverified, reason));
        assert!(report.summary().contains(id));
    }
    assert!(report.summary().starts_with("never-go"));
}

#[test]
fn namespace_ownership_and_cleanup_are_fail_closed_including_on_panic() {
    let dir = PrivateDir::new();
    let gate = ReportGate::Cluster(dir.config());
    for scenario in [
        "happy",
        "create-failure",
        "already-exists",
        "late-error",
        "delete-error",
        "panic",
    ] {
        let fake = FakeRunner::new(scenario);
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&fake, &gate, &dir.0)));
        let calls = fake.calls.borrow();
        let name = &calls[0][2];
        let deletes: Vec<_> = calls.iter().filter(|args| args[0] == "delete").collect();
        if ["create-failure", "already-exists"].contains(&scenario) {
            let error = outcome.unwrap().unwrap_err();
            assert!(error.contains("FS_TEST_NAMESPACE_MODE"));
            assert!(!error.contains("SECRET_TOKEN_abc"));
            assert!(deletes.is_empty());
        } else {
            assert_eq!(deletes.len(), 1);
            assert_eq!(
                calls.last().unwrap(),
                &["delete", "namespace", name, "--wait"]
            );
            if scenario == "panic" {
                assert!(outcome.is_err());
            } else {
                assert_eq!(outcome.unwrap().is_ok(), scenario == "happy");
            }
        }
    }
}

#[test]
fn cluster_runs_execute_the_checked_plan_and_persist_the_observed_evidence() {
    let dir = PrivateDir::new();
    let gate = ReportGate::Cluster(dir.config());
    for (scenario, version, version_reason, permission_reason, target, cell, succeeds) in [
        (
            "happy",
            Some("v1.37.2"),
            Reason::Recorded,
            Reason::Allowed,
            "k8s-1.37",
            Verdict::Passed,
            true,
        ),
        (
            "k3s",
            Some("v1.36.4+k3s1"),
            Reason::Recorded,
            Reason::Allowed,
            "k8s-1.36",
            Verdict::Passed,
            true,
        ),
        (
            "unmatched",
            Some("v1.35.0"),
            Reason::Recorded,
            Reason::Allowed,
            "k8s-1.37",
            Verdict::Unverified,
            true,
        ),
        (
            "denied",
            Some("v1.37.2"),
            Reason::Recorded,
            Reason::Denied,
            "k8s-1.37",
            Verdict::Unverified,
            true,
        ),
        (
            "forbidden",
            None,
            Reason::Forbidden,
            Reason::Forbidden,
            "k8s-1.37",
            Verdict::Unverified,
            false,
        ),
        (
            "late-error",
            None,
            Reason::ProbeError,
            Reason::Allowed,
            "k8s-1.37",
            Verdict::Unverified,
            false,
        ),
        (
            "invalid-version",
            None,
            Reason::InvalidVersion,
            Reason::Allowed,
            "k8s-1.37",
            Verdict::Unverified,
            false,
        ),
        (
            "permission-error",
            Some("v1.37.2"),
            Reason::Recorded,
            Reason::ProbeError,
            "k8s-1.37",
            Verdict::Unverified,
            false,
        ),
    ] {
        let fake = FakeRunner::new(scenario);
        let result = run(&fake, &gate, &dir.0);
        assert_eq!(result.is_ok(), succeeds, "{scenario}: {result:?}");
        let calls = fake.calls.borrow();
        let namespace = super::Namespace::new(&calls[0][2]).unwrap();
        assert_eq!(*calls, plan(&namespace), "{scenario}");
        let path = dir.0.join(format!("{}.report", namespace.name()));
        if let Ok(saved) = result {
            assert_eq!(saved, path);
        }
        let text = std::fs::read_to_string(path).unwrap();
        let report = Report::parse(&text).unwrap();
        let item = report.item("kubernetes-version").unwrap();
        assert_eq!(item.evidence.as_deref(), version, "{scenario}");
        assert_eq!(item.reason, version_reason, "{scenario}");
        assert_eq!(
            item.verdict,
            if version.is_some() {
                Verdict::Passed
            } else {
                Verdict::Unverified
            }
        );
        for id in Report::ac3_ids()
            .into_iter()
            .filter(|id| id != "kubernetes-version")
        {
            let item = report.item(&id).unwrap();
            assert_eq!(item.reason, permission_reason, "{scenario}: {id}");
            let allowed = permission_reason == Reason::Allowed;
            assert_eq!(
                item.verdict,
                if allowed {
                    Verdict::Passed
                } else {
                    Verdict::Unverified
                }
            );
            assert_eq!(item.evidence.as_deref(), allowed.then_some("yes"));
        }
        assert_eq!(
            report.item("plan-namespaced-only").unwrap().verdict,
            Verdict::Passed
        );
        for other in super::evidence::TARGETS {
            assert_eq!(
                report.cell(other).unwrap(),
                if other == target {
                    cell
                } else {
                    Verdict::Unverified
                },
                "{scenario}: {other}"
            );
        }
        for (id, reason) in super::evidence::FIXED {
            let item = report.item(id).unwrap();
            assert_eq!((item.verdict, item.reason), (Verdict::Unverified, reason));
            assert!(report.summary().contains(id));
        }
        assert!(report.summary().starts_with("never-go"));
        assert!(!text.contains("SECRET_TOKEN_abc"));
    }
}

#[test]
fn run_failures_identify_the_namespace_and_saved_report_without_diagnostics() {
    let dir = PrivateDir::new();
    let gate = ReportGate::Cluster(dir.config());
    for scenario in [
        "create-failure",
        "already-exists",
        "late-error",
        "delete-error",
    ] {
        let fake = FakeRunner::new(scenario);
        let error = run(&fake, &gate, &dir.0).unwrap_err();
        let name = fake.calls.borrow()[0][2].clone();
        let path = dir.0.join(format!("{name}.report"));
        assert!(error.contains(&name), "{scenario}: {error}");
        assert!(
            error.contains(path.to_str().unwrap()),
            "{scenario}: {error}"
        );
        assert!(path.is_file());
        assert!(!error.contains("SECRET_TOKEN_abc"));
    }
}

#[test]
fn panic_cleanup_failure_logs_the_namespace_and_report() {
    const CHILD_DIRECTORY: &str = "WAS01_TEST_PANIC_CLEANUP_DIRECTORY";
    if let Some(directory) = std::env::var_os(CHILD_DIRECTORY) {
        let dir = PrivateDir::new();
        let gate = ReportGate::Cluster(dir.config());
        let fake = FakeRunner::new("panic-delete-error");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(&fake, &gate, Path::new(&directory))
        }));
        assert!(outcome.is_err());
        assert_eq!(fake.calls.borrow().last().unwrap()[0], "delete");
        return;
    }
    // Capture the actual panic-path diagnostic at the process boundary.
    let dir = PrivateDir::new();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "support::report_tests::panic_cleanup_failure_logs_the_namespace_and_report",
            "--nocapture",
        ])
        .env(CHILD_DIRECTORY, &dir.0)
        .env_remove("FS_TEST_CLUSTER")
        .env_remove("FS_WAS01_CLUSTER")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let path = std::fs::read_dir(&dir.0)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let namespace = path.file_stem().unwrap().to_str().unwrap();
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("cleanup failed; manual operator cleanup required"));
    assert!(diagnostic.contains(namespace));
    assert!(diagnostic.contains(path.to_str().unwrap()));
    assert!(!diagnostic.contains("SECRET_TOKEN_abc"));
}

#[test]
fn runner_errors_are_unverified_for_every_permission() {
    let fake = FakeRunner::new("all-errors");
    let mut report = Report::new();
    probe_permissions(
        &mut report,
        &fake,
        &super::Namespace::new("fs-test-was01-errors").unwrap(),
    )
    .unwrap();
    for id in Report::ac3_ids()
        .into_iter()
        .filter(|id| id != "kubernetes-version")
    {
        let item = report.item(&id).unwrap();
        assert_eq!(
            (item.verdict, item.reason),
            (Verdict::Unverified, Reason::ProbeError)
        );
    }
}

#[test]
fn documentation_names_the_entire_inventory_and_dependency_guard_is_pinned_to_base() {
    let readme = include_str!("../../../docs/qualification/was-01/README.md");
    for id in Report::new().ids() {
        assert!(readme.contains(id), "missing {id}");
    }
    for code in Reason::ALL {
        assert!(readme.contains(code.code()), "missing {}", code.code());
    }
    for target in super::evidence::TARGETS {
        assert!(readme.contains(target));
    }
    for name in [
        "FS_WAS01_CLUSTER",
        "FS_TEST_CLUSTER",
        "FS_TEST_NAMESPACE_MODE",
        "FS_TEST_NAMESPACE",
        "KUBECONFIG",
        "CARGO_TARGET_TMPDIR",
    ] {
        assert!(readme.contains(name), "missing {name}");
    }
    let root = include_str!("../../../Cargo.toml");
    let worker = include_str!("../../Cargo.toml");
    let lock = include_str!("../../../Cargo.lock");
    use super::was01_report::dependency_guard;
    assert!(dependency_guard(root, worker, lock));
    assert!(!dependency_guard(
        root,
        &worker.replace("[dependencies]", "[dependencies]\ninjected = \"1\""),
        lock
    ));
    assert!(!dependency_guard(
        &format!("{root}\n[dependencies]\ninjected = \"1\"\n"),
        worker,
        lock
    ));
    assert!(!dependency_guard(
        root,
        worker,
        &format!("{lock}\n[[package]]\nname = \"injected\"\nversion = \"1.0.0\"\n")
    ));
    assert!(!dependency_guard(
        root,
        worker,
        &format!("{lock}\n[[package]]\n")
    ));
    assert!(!dependency_guard("invalid", worker, lock));
}

#[test]
fn process_records_valid_versions_and_rejects_malformed_probe_and_report_values() {
    for version in ["v1.37.2", "v1.36.4+k3s1", "v1.35.0", "v1.370.0", "v2.37.0"] {
        let dir = PrivateDir::new();
        let body = format!(
            "printf '%s\\n' '{}'",
            serde_json::json!({"gitVersion":version})
        );
        let script = dir.script(&body);
        let runner = process(&dir, &script, Duration::from_secs(3));
        let mut report = Report::new();
        probe_version(&mut report, &runner).unwrap();
        let item = report.item("kubernetes-version").unwrap();
        assert_eq!(
            (item.verdict, item.reason, item.evidence.as_deref()),
            (Verdict::Passed, Reason::Recorded, Some(version))
        );
        assert_eq!(Report::parse(&report.render()).unwrap(), report);
        if ["v1.35.0", "v1.370.0", "v2.37.0"].contains(&version) {
            for target in super::evidence::TARGETS {
                assert_eq!(report.cell(target).unwrap(), Verdict::Unverified);
            }
        }
    }
    for value in [serde_json::json!({}), serde_json::json!({"gitVersion":37})]
        .into_iter()
        .chain(
            [
                "1.37.2".to_owned(),
                "v1.37".into(),
                " v1.37.2".into(),
                "v1.37.2\n".into(),
                "v1.37.2/token".into(),
                format!("v1.37.2+{}", "a".repeat(200)),
            ]
            .into_iter()
            .map(|version| serde_json::json!({"gitVersion":version})),
        )
    {
        let dir = PrivateDir::new();
        let script = dir.script(&format!("printf '%s\\n' '{value}'"));
        let runner = process(&dir, &script, Duration::from_secs(3));
        let mut report = Report::new();
        probe_version(&mut report, &runner).unwrap();
        let item = report.item("kubernetes-version").unwrap();
        assert_eq!(
            (item.verdict, item.reason, item.evidence.as_deref()),
            (Verdict::Unverified, Reason::InvalidVersion, None)
        );
        if let Some(version) = value.get("gitVersion").and_then(serde_json::Value::as_str) {
            assert!(Report::parse(&version_report_text("v1.35.0")).is_ok());
            assert!(Report::parse(&version_report_text(version)).is_err());
        }
    }
}

#[test]
fn report_parser_rejects_reasons_inconsistent_with_the_item_or_verdict() {
    for (id, verdict, valid, invalid) in [
        (
            "can-get-services",
            Verdict::Failed,
            Reason::ProbeError,
            "not-probed",
        ),
        (
            "can-get-services",
            Verdict::Unverified,
            Reason::NotProbed,
            "plan-rejected",
        ),
        (
            "plan-namespaced-only",
            Verdict::Failed,
            Reason::RejectedPlan,
            "invalid-version",
        ),
        (
            "kubernetes-version",
            Verdict::Unverified,
            Reason::InvalidVersion,
            "denied",
        ),
    ] {
        let mut report = Report::new();
        report.record(id, verdict, valid, None).unwrap();
        let text = report.render();
        assert_eq!(Report::parse(&text).unwrap(), report);
        let corrupted = text.replace(
            &format!("{id}\t{}\t{}\t-\n", verdict.code(), valid.code()),
            &format!("{id}\t{}\t{invalid}\t-\n", verdict.code()),
        );
        assert_ne!(corrupted, text);
        assert!(Report::parse(&corrupted).is_err(), "{id}: {invalid}");
    }
}

#[test]
fn report_parser_rejects_unknown_duplicate_missing_and_inconsistent_items() {
    let report = Report::new();
    let text = report.render();
    for corrupted in [
        text.replace("can-get-services", "unknown-item"),
        text.replace("can-get-services\tunverified\tnot-probed\t-\n", ""),
        text.replace("not-probed", "arbitrary-reason"),
        text.replace("unverified", "passed"),
        text.replace(
            "can-get-services\tunverified\tnot-probed\t-\n",
            concat!(
                "can-get-services\tunverified\tnot-probed\t-\n",
                "can-get-services\tunverified\tnot-probed\t-\n",
            ),
        ),
        text.replace("cell\tk8s-1.37\tunverified", "cell\tk8s-1.37\tpassed"),
        text.replace("never-go", "go"),
    ] {
        assert!(Report::parse(&corrupted).is_err());
    }
    let mut report = report;
    assert!(
        report
            .record(
                "pin-engine",
                Verdict::Passed,
                Reason::Recorded,
                Some("a candidate")
            )
            .is_err()
    );
    assert!(
        report
            .record(
                "can-get-services",
                Verdict::Passed,
                Reason::Allowed,
                Some("no")
            )
            .is_err()
    );
    assert!(
        report
            .record(
                "can-get-services",
                Verdict::Unverified,
                Reason::Denied,
                Some("token=SECRET_TOKEN_abc")
            )
            .is_err()
    );
    assert!(!report.render().contains("SECRET_TOKEN_abc"));
}
