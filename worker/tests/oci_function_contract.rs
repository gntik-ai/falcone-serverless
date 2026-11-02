use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts/oci-function-contract")
}

#[test]
fn context_has_only_opaque_attempt_fields() {
    let schema: Value =
        serde_json::from_str(&fs::read_to_string(contracts().join("context.schema.json")).unwrap())
            .unwrap();
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap();
    let valid = json!({"handle":"opaque-attempt", "invocationId":"invocation-1", "deadline":"2026-10-31T12:00:00Z"});
    assert!(validator.is_valid(&valid));
    let properties = schema["properties"].as_object().unwrap();
    assert_eq!(
        properties.keys().map(String::as_str).collect::<Vec<_>>(),
        ["deadline", "handle", "invocationId"]
    );
    assert_eq!(schema["additionalProperties"], false);
    for forbidden in [
        "credential",
        "token",
        "secret",
        "tenant",
        "workspace",
        "extra",
    ] {
        let mut value = valid.clone();
        value[forbidden] = json!("forbidden");
        assert!(!validator.is_valid(&value), "{forbidden}");
    }
    for invalid in ["", "2026-02-30T00:00:00Z", "2026-10-31T12:00:00+01:00"] {
        let mut value = valid.clone();
        value["deadline"] = json!(invalid);
        assert!(!validator.is_valid(&value), "{invalid}");
    }
}

use std::{
    io::Read,
    net::TcpListener,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn reference(port: u16, mode: &str) -> Running {
    let child = Command::new(env!("CARGO_BIN_EXE_oci-contract-reference"))
        .env_clear()
        .env("PORT", port.to_string())
        .arg(mode)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let running = Running(child);
    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "reference startup"
        );
        thread::sleep(Duration::from_millis(10));
    }
    running
}

fn tester(port: Option<u16>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_oci-function-conformance"));
    command
        .env_clear()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(port) = port {
        command.env("PORT", port.to_string());
    }
    let mut running = Running(command.spawn().unwrap());
    let start = Instant::now();
    let status = loop {
        if let Some(status) = running.0.try_wait().unwrap() {
            break status;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "tester hung");
        thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    running
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    running
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    Output {
        status,
        stdout,
        stderr,
    }
}

fn assert_rules(output: Output, expected: &[&str]) {
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.is_empty(), "{stderr}");
    let ids: Vec<_> = stdout
        .lines()
        .map(|line| line.split_once(':').unwrap().0)
        .collect();
    assert_eq!(ids, expected, "{stdout}");
    assert_eq!(
        output.status.code(),
        Some(i32::from(!expected.is_empty())),
        "{stdout}"
    );
}

#[test]
fn compliant_http_yields_no_violations() {
    let port = free_port();
    let _candidate = reference(port, "compliant");
    assert_rules(tester(Some(port), &["http"]), &[]);
}

#[path = "../src/oci_contract.rs"]
pub mod contract;

#[test]
fn embedded_fixtures_validate_at_the_contract_boundary() {
    for fixture in contract::FIXTURES {
        let value: Value = serde_json::from_slice(fixture.bytes).unwrap();
        let schema: Value = serde_json::from_str(match fixture.schema {
            "request" => contract::REQUEST_SCHEMA,
            "context" => contract::CONTEXT_SCHEMA,
            "envelope" => contract::ENVELOPE_SCHEMA,
            other => panic!("unknown schema {other}"),
        })
        .unwrap();
        let valid = jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .unwrap()
            .is_valid(&value);
        let request = if fixture.schema == "context" {
            json!({"params":{}, "context":value})
        } else {
            value
        };
        let violations = if fixture.schema == "envelope" {
            contract::check_envelope(&request)
        } else {
            contract::check_request(&request)
        };
        if let Some(rule) = fixture.rule {
            assert!(!valid, "{}", fixture.path);
            assert_eq!(
                violations.keys().copied().collect::<Vec<_>>(),
                [rule],
                "{}",
                fixture.path
            );
        } else {
            assert!(valid, "{}", fixture.path);
            assert!(violations.is_empty(), "{}: {violations:?}", fixture.path);
        }
    }
    let request_schema: Value = serde_json::from_str(contract::REQUEST_SCHEMA).unwrap();
    let context_schema: Value = serde_json::from_str(contract::CONTEXT_SCHEMA).unwrap();
    assert_eq!(request_schema["$defs"]["context"], context_schema);
    assert!(
        jsonschema::options()
            .should_validate_formats(true)
            .build(&request_schema)
            .unwrap()
            .is_valid(&contract::request(json!({})))
    );
}

#[test]
fn params_budget_counts_compact_json_bytes() {
    assert!(contract::check_request(&contract::request(json!("x".repeat(5_999_998)))).is_empty());
    let violations = contract::check_request(&contract::request(json!("x".repeat(5_999_999))));
    assert_eq!(
        violations.keys().copied().collect::<Vec<_>>(),
        ["OCI-V1-PARAMS-LIMIT"]
    );
    let violations = contract::check_request(&contract::request(json!("é".repeat(3_000_000))));
    assert_eq!(
        violations.keys().copied().collect::<Vec<_>>(),
        ["OCI-V1-PARAMS-LIMIT"]
    );
}

#[test]
fn result_budget_counts_compact_json_bytes() {
    let mut envelope = json!({"status":"success", "result":"x".repeat(5_999_998), "logs":""});
    assert!(contract::check_envelope(&envelope).is_empty());
    envelope["result"] = json!("x".repeat(5_999_999));
    assert_eq!(
        contract::check_envelope(&envelope)
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        ["OCI-V1-RESULT-LIMIT"]
    );
}

#[test]
fn log_budget_requires_marker_and_counts_utf8_bytes() {
    let mut envelope = json!({"status":"success", "result":null, "logs":"x".repeat(255_999)});
    assert!(contract::check_envelope(&envelope).is_empty());
    envelope["logs"] = json!("x".repeat(256_000));
    assert_eq!(
        contract::check_envelope(&envelope)
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        ["OCI-V1-LOGS-TRUNCATION"]
    );
    envelope["logs"] = json!(format!("{}[TRUNCATED]", "x".repeat(255_989)));
    assert!(contract::check_envelope(&envelope).is_empty());
    for logs in ["x".repeat(256_001), "é".repeat(128_001)] {
        envelope["logs"] = json!(logs);
        assert_eq!(
            contract::check_envelope(&envelope)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            ["OCI-V1-LOGS-LIMIT"]
        );
    }
}

#[test]
fn http_framing_is_bounded_and_unambiguous() {
    use std::io::Cursor;
    let response = contract::read_response(&mut Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: application/json; charset=utf-8\r\n\r\n{}"));
    assert_eq!(response.unwrap().body, b"{}");
    for bytes in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 9000000\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\n\r\n{}",
        b"HTTP/1.1 nonsense\r\nContent-Length: 0\r\n\r\n",
    ] {
        assert!(contract::read_response(&mut Cursor::new(bytes)).is_err());
    }
    assert!(contract::read_response(&mut Cursor::new(vec![b'x'; 16_385])).is_err());
}

#[test]
fn unset_port_has_exactly_the_port_rule_in_both_modes() {
    for args in [
        vec!["http"],
        vec![
            "process",
            "--",
            env!("CARGO_BIN_EXE_oci-contract-reference"),
            "compliant",
        ],
    ] {
        assert_rules(tester(None, &args), &["OCI-V1-PORT"]);
    }
}

#[test]
fn each_http_rule_is_isolated() {
    for rule in [
        "OCI-V1-HEALTHZ",
        "OCI-V1-INVOKE",
        "OCI-V1-REQUEST",
        "OCI-V1-ENVELOPE",
        "OCI-V1-STATUS",
        "OCI-V1-PARAMS-LIMIT",
        "OCI-V1-RESULT-LIMIT",
        "OCI-V1-LOGS-LIMIT",
        "OCI-V1-LOGS-TRUNCATION",
    ] {
        let port = free_port();
        let _candidate = reference(port, rule);
        assert_rules(tester(Some(port), &["http"]), &[rule]);
    }
}

#[test]
fn filesystem_probe_allows_scratch_and_catches_sentinel_changes() {
    for change in [
        "none",
        "tmp",
        "cwd",
        "home",
        "modify",
        "remove",
        "symlink",
        "nested",
        "permissions",
    ] {
        let probe = contract::FilesystemProbe::new().unwrap();
        let root = probe.root().to_path_buf();
        match change {
            "none" => {}
            "tmp" => fs::write(probe.scratch().join("allowed"), b"scratch").unwrap(),
            "cwd" | "home" => fs::write(probe.sentinel().join("new"), b"violation").unwrap(),
            "modify" => fs::write(probe.sentinel().join("unchanged"), b"changed").unwrap(),
            "remove" => fs::remove_file(probe.sentinel().join("unchanged")).unwrap(),
            "symlink" => {
                fs::remove_file(probe.sentinel().join("unchanged")).unwrap();
                std::os::unix::fs::symlink(probe.scratch(), probe.sentinel().join("unchanged"))
                    .unwrap();
            }
            "nested" => fs::create_dir(probe.sentinel().join("nested")).unwrap(),
            "permissions" => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    probe.sentinel().join("unchanged"),
                    fs::Permissions::from_mode(0o400),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            probe.unchanged().unwrap(),
            matches!(change, "none" | "tmp"),
            "{change}"
        );
        drop(probe);
        assert!(!root.exists());
    }
}

fn assert_process_rules(output: Output, expected: &[&str]) {
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(
        output.status.code(),
        Some(i32::from(!stdout.is_empty())),
        "{stdout}"
    );
    let ids: Vec<_> = stdout
        .lines()
        .filter_map(|line| {
            let (rule, detail) = line.split_once(':').unwrap();
            if rule == "OCI-V1-BINDING" && !expected.contains(&rule) {
                // Host noise may make complete ownership evidence unavailable. It
                // must never conceal a positively observed exclusive-binding failure.
                assert!(!detail.contains("not listening exclusively"), "{line}");
                assert!(detail.contains("inspection unstable"), "{line}");
                None
            } else {
                Some(rule)
            }
        })
        .collect();
    assert_eq!(ids, expected, "{stdout}");
}

fn process_case(mode: &str, expected: &[&str]) {
    let port = free_port();
    assert_process_rules(
        tester(
            Some(port),
            &[
                "process",
                "--",
                env!("CARGO_BIN_EXE_oci-contract-reference"),
                mode,
            ],
        ),
        expected,
    );
}

#[test]
fn compliant_process_and_scratch_writes_pass() {
    for mode in ["compliant", "write-tmp"] {
        process_case(mode, &[]);
    }
}

#[test]
fn process_rules_and_filesystem_locations_are_isolated() {
    for (mode, rule) in [
        ("OCI-V1-BINDING", "OCI-V1-BINDING"),
        ("ignore-sigterm", "OCI-V1-SIGTERM"),
        ("write-cwd", "OCI-V1-FILESYSTEM"),
        ("write-home", "OCI-V1-FILESYSTEM"),
        ("modify-sentinel", "OCI-V1-FILESYSTEM"),
        ("remove-sentinel", "OCI-V1-FILESYSTEM"),
        ("health-hang", "OCI-V1-HEALTHZ"),
    ] {
        process_case(mode, &[rule]);
    }
}

#[test]
fn legacy_replayers_and_boundary_envelopes_pass() {
    for mode in [
        "replay-success",
        "replay-error",
        "replay-null",
        "result-boundary",
        "logs-boundary",
    ] {
        let port = free_port();
        let _candidate = reference(port, mode);
        assert_rules(tester(Some(port), &["http"]), &[]);
    }
}

#[test]
fn utc_deadlines_match_the_published_schema() {
    let schema: Value = serde_json::from_str(contract::REQUEST_SCHEMA).unwrap();
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap();
    for (deadline, valid) in [
        ("2000-02-29T00:00:00Z", true),
        ("1900-02-29T00:00:00Z", false),
        ("2026-12-31T23:59:60Z", true),
        ("2026-12-31T12:00:60Z", false),
        ("2026-10-31T12:00:00.123456789Z", true),
        ("2026-10-31T12:00:00.1234567890Z", false),
        ("2026-10-31T12:00:00+00:00", false),
    ] {
        let mut value = contract::request(json!({}));
        value["context"]["deadline"] = json!(deadline);
        assert_eq!(validator.is_valid(&value), valid, "schema: {deadline}");
        assert_eq!(
            contract::check_request(&value).is_empty(),
            valid,
            "tester: {deadline}"
        );
    }
}

#[test]
#[should_panic(expected = "unknown OCI contract rule: OCI-V1-NEW")]
fn diagnostics_reject_rules_outside_the_documented_registry() {
    let mut diagnostics = contract::Diagnostics::new();
    diagnostics.insert("OCI-V1-NEW", "undocumented tester check");
}

#[test]
fn doc_rules_and_tester_checks_have_exact_parity() {
    use std::collections::BTreeSet;
    let documented: BTreeSet<_> = contract::DOC
        .lines()
        .filter_map(|line| line.strip_prefix("| OCI-V1-"))
        .map(|line| format!("OCI-V1-{}", line.split('|').next().unwrap().trim()))
        .collect();
    let checked: BTreeSet<_> = contract::RULE_IDS.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(documented, checked);
    assert_eq!(checked.len(), contract::RULE_IDS.len());
    // Inspect the actual insertion sites, excluding fixture metadata and the
    // registry itself, so removing a check cannot leave a stale registry entry.
    let emitted: BTreeSet<_> = include_str!("../src/oci_contract.rs")
        .split(".insert(")
        .skip(1)
        .filter_map(|call| call.trim_start().strip_prefix('"'))
        .filter_map(|literal| literal.split('"').next())
        .filter(|id| id.starts_with("OCI-V1-"))
        .map(str::to_owned)
        .collect();
    assert_eq!(documented, emitted);
}

fn fixture_pin_matches(path: &str, bytes: &[u8]) -> bool {
    use sha2::{Digest, Sha256};
    let prefix = format!("| `{path}` | `");
    let expected = contract::DOC
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap()
        .split('`')
        .next()
        .unwrap();
    format!("{:x}", Sha256::digest(bytes)) == expected
}

#[test]
fn provisional_legacy_hashes_are_pinned_and_mismatch_is_rejected() {
    for fixture in contract::FIXTURES
        .iter()
        .filter(|f| f.path.starts_with("fixtures/legacy/"))
    {
        assert!(
            fixture_pin_matches(fixture.path, fixture.bytes),
            "{}",
            fixture.path
        );
        let mut modified = fixture.bytes.to_vec();
        modified.push(b' ');
        assert!(
            !fixture_pin_matches(fixture.path, &modified),
            "{}",
            fixture.path
        );
    }
    let mut on_disk = Vec::new();
    for directory in ["valid", "invalid", "legacy"] {
        for file in fs::read_dir(contracts().join("fixtures").join(directory)).unwrap() {
            let file = file.unwrap();
            on_disk.push(format!(
                "fixtures/{directory}/{}",
                file.file_name().to_str().unwrap()
            ));
        }
    }
    on_disk.sort();
    let embedded: Vec<_> = contract::FIXTURES
        .iter()
        .map(|f| f.path.to_owned())
        .collect();
    assert_eq!(on_disk, embedded, "every fixture must be embedded");
}

#[test]
fn valid_fixture_files_are_direct_contract_instances() {
    for kind in ["request", "context", "envelope"] {
        let fixture: Value = serde_json::from_slice(
            &fs::read(contracts().join(format!("fixtures/valid/{kind}.json"))).unwrap(),
        )
        .unwrap();
        let schema: Value = serde_json::from_slice(
            &fs::read(contracts().join(format!("{kind}.schema.json"))).unwrap(),
        )
        .unwrap();
        assert!(
            jsonschema::options()
                .should_validate_formats(true)
                .build(&schema)
                .unwrap()
                .is_valid(&fixture),
            "{kind}"
        );
    }
}

#[test]
fn source_only_image_context_has_identical_contract_assets() {
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/oci_contract_assets");
    for file in [
        "README.md",
        "context.schema.json",
        "request.schema.json",
        "envelope.schema.json",
    ] {
        assert_eq!(
            fs::read(assets.join(file)).unwrap(),
            fs::read(contracts().join(file)).unwrap(),
            "{file}"
        );
    }
    for fixture in contract::FIXTURES {
        assert_eq!(
            fs::read(assets.join(fixture.path)).unwrap(),
            fs::read(contracts().join(fixture.path)).unwrap(),
            "{}",
            fixture.path
        );
        assert_eq!(
            fixture.bytes,
            fs::read(contracts().join(fixture.path)).unwrap(),
            "{}",
            fixture.path
        );
    }
}

mod process_seam {
    use super::contract::{BindingVerdict, ProcessEvent, ProcessTable, ProcessTree};
    use rustix::process::Signal;
    use std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet},
        io,
        rc::Rc,
        time::{Duration, Instant},
    };

    #[derive(Clone)]
    struct Task {
        state: char,
        children: Vec<u32>,
        sockets: BTreeSet<u64>,
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Scenario {
        Stable,
        TcpCountersAdvance,
        ForkDuringFdRead,
        SocketTransfer,
        WildcardDuringResample,
        MalformedSocketState,
        MalformedTaskState,
        MalformedTaskEntry,
        MalformedTcpEntry,
        ReadCrossingDeadline,
        StopWaitConsumesInspectionBudget,
        ReparentingMidWalk,
        ForkThenExit,
        UnreadableFd,
        VanishedTid,
        ZombieLeaderLiveThread,
        StateAfterLastFdSample,
        PidfdUnavailable,
        ReusedChildBeforeOpen,
        CleanupForkThenExit,
        CleanupExhaustion,
        IgnoreTermOnly,
    }
    struct SyntheticProc {
        tasks: BTreeMap<u32, BTreeMap<u32, Task>>,
        tcp: String,
        tcp6: String,
        now: Instant,
        scenario: Scenario,
        delivered: bool,
        signals: Rc<RefCell<Vec<(u32, Signal)>>>,
        alive: Rc<RefCell<BTreeSet<u32>>>,
    }
    const HEADER: &str = "sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n";
    fn row(local: &str, state: &str, inode: u64) -> String {
        format!(
            "0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000 1000 0 {inode}\n"
        )
    }
    impl SyntheticProc {
        fn compliant() -> Self {
            let task = |children, sockets| Task {
                state: 'T',
                children,
                sockets,
            };
            Self {
                tasks: [
                    (1, [(1, task(vec![10], BTreeSet::new()))].into()),
                    (10, [(10, task(vec![], [42].into()))].into()),
                ]
                .into(),
                tcp: format!("{HEADER}{}", row("0100007F:1F90", "0A", 42)),
                tcp6: HEADER.into(),
                now: Instant::now(),
                scenario: Scenario::Stable,
                delivered: false,
                signals: Rc::default(),
                alive: Rc::new(RefCell::new([10].into())),
            }
        }
        fn tree(self) -> ProcessTree<Self> {
            let deadline = self.now + Duration::from_secs(60);
            ProcessTree::new(self, 10, 1, BTreeMap::new(), deadline)
        }
    }
    impl ProcessTable for SyntheticProc {
        type Handle = u32;
        fn read(&mut self, path: &str) -> io::Result<String> {
            if path == "/proc/net/tcp" {
                if self.scenario == Scenario::MalformedTcpEntry {
                    return Ok(self.tcp.replace(" 1000 ", " bad-uid "));
                }
                if self.scenario == Scenario::ReadCrossingDeadline {
                    self.now += Duration::from_secs(61);
                }
                if self.scenario == Scenario::MalformedSocketState {
                    return Ok(format!("{HEADER}{}", row("0100007F:1F90", "FE", 42)));
                }
                return Ok(self.tcp.clone());
            }
            if path == "/proc/net/tcp6" {
                return Ok(self.tcp6.clone());
            }
            let parts: Vec<_> = path.split('/').collect();
            let pid = parts[2].parse::<u32>().unwrap();
            let tid = parts[4].parse::<u32>().unwrap();
            let task = self
                .tasks
                .get(&pid)
                .and_then(|tasks| tasks.get(&tid))
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            match parts[5] {
                "children" => Ok(task
                    .children
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")),
                "stat" if self.scenario == Scenario::MalformedTaskEntry && pid == 10 => Ok(
                    format!("{tid} (synthetic) T bad-parent {}", vec!["0"; 18].join(" ")),
                ),
                "stat" => Ok(format!(
                    "{tid} (synthetic) {} {}",
                    if pid == 10 && self.scenario == Scenario::MalformedTaskState {
                        '?'
                    } else {
                        task.state
                    },
                    vec!["0"; 19].join(" ")
                )),
                _ => panic!("unexpected path: {path}"),
            }
        }
        fn list(&mut self, path: &str, _: Instant) -> io::Result<Vec<u32>> {
            if path == "/proc" {
                return Ok(self.tasks.keys().copied().collect());
            }
            let pid = path.split('/').nth(2).unwrap().parse::<u32>().unwrap();
            if pid == 10 && self.scenario == Scenario::VanishedTid {
                return Ok(vec![10, 11]);
            }
            self.tasks
                .get(&pid)
                .map(|tasks| tasks.keys().copied().collect())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }
        fn sockets(&mut self, pid: u32, tid: u32, _: Instant) -> io::Result<BTreeSet<u64>> {
            if pid == 10 && self.scenario == Scenario::UnreadableFd {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let sockets = self
                .tasks
                .get(&pid)
                .and_then(|tasks| tasks.get(&tid))
                .map(|task| task.sockets.clone())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            if pid == 10 && !self.delivered {
                match self.scenario {
                    Scenario::ForkDuringFdRead => {
                        self.delivered = true;
                        self.tasks
                            .get_mut(&10)
                            .unwrap()
                            .get_mut(&10)
                            .unwrap()
                            .children
                            .push(20);
                        self.tasks.insert(
                            20,
                            [(
                                20,
                                Task {
                                    state: 'T',
                                    children: vec![],
                                    sockets: BTreeSet::new(),
                                },
                            )]
                            .into(),
                        );
                    }
                    Scenario::ReparentingMidWalk | Scenario::ForkThenExit => {
                        self.delivered = true;
                        self.tasks
                            .get_mut(&1)
                            .unwrap()
                            .get_mut(&1)
                            .unwrap()
                            .children
                            .push(20);
                        self.tasks.insert(
                            20,
                            [(
                                20,
                                Task {
                                    state: 'T',
                                    children: vec![],
                                    sockets: [43].into(),
                                },
                            )]
                            .into(),
                        );
                        self.tcp.push_str(&row("00000000:2329", "0A", 43));
                        if self.scenario == Scenario::ForkThenExit {
                            self.tasks.remove(&10);
                            self.tasks
                                .get_mut(&1)
                                .unwrap()
                                .get_mut(&1)
                                .unwrap()
                                .children
                                .retain(|id| *id != 10);
                        }
                    }
                    _ => {}
                }
            }
            Ok(sockets)
        }
        fn open(&mut self, pid: u32, _: Instant) -> io::Result<u32> {
            if pid == 20 && self.scenario == Scenario::ReusedChildBeforeOpen && !self.delivered {
                self.delivered = true;
                // The stopped parent ignores SIGCHLD: its exited child has been
                // auto-reaped and the kernel reused the PID outside the tree.
                self.tasks
                    .get_mut(&10)
                    .unwrap()
                    .get_mut(&10)
                    .unwrap()
                    .children
                    .clear();
                self.tasks
                    .get_mut(&30)
                    .unwrap()
                    .get_mut(&30)
                    .unwrap()
                    .children
                    .push(20);
            }
            if self.scenario == Scenario::PidfdUnavailable || !self.tasks.contains_key(&pid) {
                Err(io::ErrorKind::NotFound.into())
            } else {
                Ok(pid)
            }
        }
        fn is_current(&mut self, pid: &u32, _: Instant) -> io::Result<bool> {
            Ok(self.tasks.contains_key(pid))
        }
        fn signal(&mut self, pid: &u32, signal: Signal) -> io::Result<()> {
            self.signals.borrow_mut().push((*pid, signal));
            if let Some(tasks) = self.tasks.get_mut(pid) {
                for task in tasks.values_mut() {
                    match signal {
                        Signal::STOP
                            if self.scenario == Scenario::StopWaitConsumesInspectionBudget => {}
                        Signal::STOP if task.state != 'Z' => task.state = 'T',
                        Signal::CONT if task.state == 'T' => task.state = 'S',
                        Signal::TERM
                            if matches!(
                                self.scenario,
                                Scenario::IgnoreTermOnly
                                    | Scenario::StopWaitConsumesInspectionBudget
                            ) => {}
                        Signal::TERM | Signal::KILL
                            if self.scenario != Scenario::CleanupExhaustion =>
                        {
                            task.state = 'Z'
                        }
                        _ => {}
                    }
                }
            }
            Ok(())
        }
        fn reap(&mut self, pid: &u32, _: Instant) -> io::Result<bool> {
            if self
                .tasks
                .get(pid)
                .is_some_and(|tasks| tasks.values().all(|t| t.state == 'Z'))
            {
                let tasks = self.tasks.remove(pid).unwrap();
                let children: Vec<_> = tasks.values().flat_map(|t| t.children.clone()).collect();
                for tasks in self.tasks.values_mut() {
                    for task in tasks.values_mut() {
                        task.children.retain(|id| id != pid);
                    }
                }
                self.tasks
                    .get_mut(&1)
                    .unwrap()
                    .get_mut(&1)
                    .unwrap()
                    .children
                    .extend(children);
                self.alive.borrow_mut().remove(pid);
                Ok(true)
            } else {
                Ok(false)
            }
        }
        fn now(&self) -> Instant {
            self.now
        }
        fn sleep(&mut self, duration: Duration) {
            self.now += duration;
        }
        fn event(&mut self, event: ProcessEvent) {
            if self.delivered {
                return;
            }
            match (self.scenario, event) {
                (Scenario::TcpCountersAdvance, ProcessEvent::FdResampleComplete) => {
                    self.delivered = true;
                    // Timers and queue lengths are not socket identity. HTTP probes
                    // leave TIME_WAIT rows whose countdown changes during inspection.
                    self.tcp = self
                        .tcp
                        .replace("03:000006ED", "03:000006E5")
                        .replace("00000000:00000000", "00000001:00000002");
                }
                (Scenario::SocketTransfer, ProcessEvent::FdSampleComplete) => {
                    self.delivered = true;
                    self.tasks
                        .get_mut(&10)
                        .unwrap()
                        .get_mut(&10)
                        .unwrap()
                        .sockets
                        .clear();
                    self.tasks
                        .get_mut(&1)
                        .unwrap()
                        .get_mut(&1)
                        .unwrap()
                        .sockets
                        .insert(42);
                }
                (Scenario::WildcardDuringResample, ProcessEvent::FdResampleComplete) => {
                    self.delivered = true;
                    self.tcp.push_str(&row("00000000:2329", "0A", 43));
                    self.tasks
                        .get_mut(&10)
                        .unwrap()
                        .get_mut(&10)
                        .unwrap()
                        .sockets
                        .insert(43);
                }
                (Scenario::CleanupForkThenExit, ProcessEvent::BeforeReap(20)) => {
                    self.delivered = true;
                    self.tasks.get_mut(&20).unwrap().get_mut(&20).unwrap().state = 'Z';
                    self.tasks.insert(
                        21,
                        [(
                            21,
                            Task {
                                state: 'S',
                                children: vec![],
                                sockets: BTreeSet::new(),
                            },
                        )]
                        .into(),
                    );
                    self.tasks
                        .get_mut(&1)
                        .unwrap()
                        .get_mut(&1)
                        .unwrap()
                        .children
                        .push(21);
                    self.alive.borrow_mut().insert(21);
                }
                (Scenario::StateAfterLastFdSample, ProcessEvent::FdResampleComplete) => {
                    self.delivered = true;
                    // An in-tree exit becomes visible after the last fd evidence.
                    self.tasks.get_mut(&10).unwrap().get_mut(&10).unwrap().state = 'Z';
                }
                _ => {}
            }
        }
    }

    #[test]
    fn sole_tree_owned_loopback_listener_is_compliant() {
        assert_eq!(
            SyntheticProc::compliant()
                .tree()
                .inspect_binding(8080)
                .unwrap(),
            BindingVerdict::Compliant
        );
    }

    #[test]
    fn compliant_listener_survives_tcp_timer_and_queue_churn() {
        let mut proc = SyntheticProc::compliant();
        proc.scenario = Scenario::TcpCountersAdvance;
        proc.tcp
            .push_str(&row("0100007F:C001", "06", 0).replace(" 00:00000000 ", " 03:000006ED "));
        assert_eq!(
            proc.tree().inspect_binding(8080).unwrap(),
            BindingVerdict::Compliant
        );
    }

    #[test]
    fn listener_without_any_fd_owner_never_passes() {
        let mut proc = SyntheticProc::compliant();
        proc.tasks
            .get_mut(&10)
            .unwrap()
            .get_mut(&10)
            .unwrap()
            .sockets
            .clear();
        assert_ne!(
            proc.tree().inspect_binding(8080).unwrap(),
            BindingVerdict::Compliant
        );
    }
    #[test]
    fn named_lifecycle_anomalies_never_yield_compliant() {
        for scenario in [
            Scenario::ForkDuringFdRead,
            Scenario::SocketTransfer,
            Scenario::WildcardDuringResample,
            Scenario::MalformedSocketState,
            Scenario::MalformedTaskState,
            Scenario::MalformedTaskEntry,
            Scenario::MalformedTcpEntry,
            Scenario::ReadCrossingDeadline,
            Scenario::ReparentingMidWalk,
            Scenario::ForkThenExit,
            Scenario::UnreadableFd,
            Scenario::VanishedTid,
            Scenario::ZombieLeaderLiveThread,
            Scenario::StateAfterLastFdSample,
            Scenario::PidfdUnavailable,
        ] {
            let mut proc = SyntheticProc::compliant();
            proc.scenario = scenario;
            if scenario == Scenario::ZombieLeaderLiveThread {
                proc.tasks.get_mut(&10).unwrap().get_mut(&10).unwrap().state = 'Z';
                proc.tasks.get_mut(&10).unwrap().insert(
                    11,
                    Task {
                        state: 'T',
                        children: vec![],
                        sockets: [43].into(),
                    },
                );
                proc.tcp.push_str(&row("00000000:2329", "0A", 43));
            }
            let verdict = proc.tree().inspect_binding(8080);
            if scenario == Scenario::ReadCrossingDeadline {
                assert_eq!(verdict.unwrap_err().kind(), io::ErrorKind::TimedOut);
            } else {
                let verdict = verdict.unwrap();
                assert_ne!(verdict, BindingVerdict::Compliant, "{scenario:?}");
                if let BindingVerdict::Unstable(detail) = verdict {
                    assert!(detail.contains("inspection unstable"), "{detail}");
                }
            }
        }
    }

    #[test]
    fn tree_owned_wildcards_ipv6_and_other_loopbacks_violate_exclusive_binding() {
        for (local, expected) in [
            ("00000000:2329", "0.0.0.0:9001"),
            ("0200007F:2329", "127.0.0.2:9001"),
            ("00000000000000000000000000000000:2329", "[::]:9001"),
        ] {
            let mut proc = SyntheticProc::compliant();
            let entry = row(local, "0A", 43);
            if local.len() > 13 {
                proc.tcp6.push_str(&entry.replacen(
                    "00000000:0000",
                    "00000000000000000000000000000000:0000",
                    1,
                ));
            } else {
                proc.tcp.push_str(&entry);
            }
            proc.tasks
                .get_mut(&10)
                .unwrap()
                .get_mut(&10)
                .unwrap()
                .sockets
                .insert(43);
            let BindingVerdict::Violation(detail) = proc.tree().inspect_binding(8080).unwrap()
            else {
                panic!("expected exclusive-binding violation for {expected}");
            };
            assert!(detail.contains(expected), "{detail}");
        }
        let mut proc = SyntheticProc::compliant();
        proc.tcp = HEADER.into();
        assert!(matches!(
            proc.tree().inspect_binding(8080).unwrap(),
            BindingVerdict::Violation(_)
        ));
    }

    #[test]
    fn extra_tree_listener_names_the_wildcard_and_outside_listener_is_excluded() {
        let mut proc = SyntheticProc::compliant();
        proc.tcp.push_str(&row("00000000:2329", "0A", 43));
        proc.tasks
            .get_mut(&10)
            .unwrap()
            .get_mut(&10)
            .unwrap()
            .sockets
            .insert(43);
        let BindingVerdict::Violation(detail) = proc.tree().inspect_binding(8080).unwrap() else {
            panic!("exclusive binding diagnostic required");
        };
        assert!(detail.contains("0.0.0.0:9001"), "{detail}");
        let mut proc = SyntheticProc::compliant();
        proc.tcp.push_str(&row("00000000:2329", "0A", 43));
        proc.tasks
            .get_mut(&1)
            .unwrap()
            .get_mut(&1)
            .unwrap()
            .sockets
            .insert(43);
        assert_eq!(
            proc.tree().inspect_binding(8080).unwrap(),
            BindingVerdict::Compliant
        );
    }

    #[test]
    fn reused_child_pid_outside_the_stopped_parent_is_never_signalled() {
        let mut proc = SyntheticProc::compliant();
        proc.scenario = Scenario::ReusedChildBeforeOpen;
        proc.tasks
            .get_mut(&10)
            .unwrap()
            .get_mut(&10)
            .unwrap()
            .children
            .push(20);
        for pid in [20, 30] {
            proc.tasks.insert(
                pid,
                [(
                    pid,
                    Task {
                        state: 'S',
                        children: vec![],
                        sockets: BTreeSet::new(),
                    },
                )]
                .into(),
            );
        }
        proc.alive.borrow_mut().insert(20);
        let alive = proc.alive.clone();
        let signals = proc.signals.clone();
        let mut tree = proc.tree();
        assert!(matches!(
            tree.inspect_binding(8080).unwrap(),
            BindingVerdict::Unstable(_)
        ));
        tree.terminate().unwrap();
        drop(tree);
        assert_eq!(
            *alive.borrow(),
            [20].into(),
            "the unrelated process was killed or reaped"
        );
        assert!(
            signals.borrow().iter().all(|(pid, _)| *pid != 20),
            "the unrelated process received a signal"
        );
    }

    #[test]
    fn cleanup_discovers_child_adopted_between_refresh_and_reap() {
        let mut proc = SyntheticProc::compliant();
        proc.scenario = Scenario::CleanupForkThenExit;
        proc.tasks
            .get_mut(&10)
            .unwrap()
            .get_mut(&10)
            .unwrap()
            .children
            .push(20);
        proc.tasks.insert(
            20,
            [(
                20,
                Task {
                    state: 'S',
                    children: vec![],
                    sockets: BTreeSet::new(),
                },
            )]
            .into(),
        );
        proc.alive.borrow_mut().insert(20);
        let alive = proc.alive.clone();
        let mut tree = proc.tree();
        assert!(tree.terminate().unwrap());
        assert!(
            alive.borrow().is_empty(),
            "cleanup returned with a surviving adopted child"
        );
    }

    #[test]
    fn deadline_crossing_still_kills_retained_processes_and_drop_does_not_retry() {
        let mut proc = SyntheticProc::compliant();
        proc.scenario = Scenario::ReadCrossingDeadline;
        let signals = proc.signals.clone();
        let mut tree = proc.tree();
        assert_eq!(
            tree.inspect_binding(8080).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            tree.terminate().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(
            signals.borrow().contains(&(10, Signal::KILL)),
            "the deadline left the stopped candidate alive"
        );
        signals.borrow_mut().clear();
        drop(tree);
        assert!(
            signals.borrow().is_empty(),
            "Drop restarted exhausted cleanup"
        );
    }

    #[test]
    fn stop_wait_leaves_time_to_kill_and_reap_within_the_absolute_deadline() {
        let mut proc = SyntheticProc::compliant();
        proc.scenario = Scenario::StopWaitConsumesInspectionBudget;
        proc.tasks.get_mut(&10).unwrap().get_mut(&10).unwrap().state = 'D';
        let alive = proc.alive.clone();
        let mut tree = proc.tree();
        assert_eq!(
            tree.inspect_binding(8080).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(!tree.terminate().unwrap());
        assert!(
            alive.borrow().is_empty(),
            "candidate was not killed and reaped"
        );
    }

    #[test]
    fn short_absolute_deadline_still_reserves_the_kill_and_reap_bound() {
        let mut proc = SyntheticProc::compliant();
        proc.scenario = Scenario::IgnoreTermOnly;
        let deadline = proc.now + Duration::from_secs(2);
        let alive = proc.alive.clone();
        let mut tree = ProcessTree::new(proc, 10, 1, BTreeMap::new(), deadline);
        assert!(!tree.terminate().unwrap());
        assert!(alive.borrow().is_empty());
    }

    #[test]
    fn drop_after_reap_or_cleanup_exhaustion_sends_no_signal() {
        for exhausted in [false, true] {
            let mut proc = SyntheticProc::compliant();
            if exhausted {
                proc.scenario = Scenario::CleanupExhaustion;
            }
            let signals = proc.signals.clone();
            let mut tree = proc.tree();
            if exhausted {
                assert!(tree.terminate().is_err());
            } else {
                assert!(tree.terminate().unwrap());
            }
            signals.borrow_mut().clear();
            drop(tree);
            assert!(
                signals.borrow().is_empty(),
                "Drop signalled after completed cleanup"
            );
        }
    }
}

#[test]
fn scratch_cleanup_crossing_deadline_returns_deadline_error() {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut probe = contract::FilesystemProbe::new_until(deadline).unwrap();
    let error = probe
        .cleanup_with(|root, until| {
            fs::remove_dir_all(root)?;
            thread::sleep(
                until.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
            );
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
}

#[test]
fn tester_rejects_a_reference_that_only_accepts_one_deadline_literal() {
    let port = free_port();
    let _candidate = reference(port, "literal-deadline");
    assert_rules(tester(Some(port), &["http"]), &["OCI-V1-REQUEST"]);
}

#[test]
fn dual_listener_process_diagnostic_names_the_wildcard() {
    let output = tester(
        Some(free_port()),
        &[
            "process",
            "--",
            env!("CARGO_BIN_EXE_oci-contract-reference"),
            "dual-listener",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("OCI-V1-BINDING: candidate is not listening exclusively on 127.0.0.1:$PORT: listener 0.0.0.0:"), "{stdout}");
    assert_rules(output, &["OCI-V1-BINDING"]);
}

#[test]
fn reference_deadlines_are_checked_over_http() {
    use std::io::Write;
    let port = free_port();
    let _candidate = reference(port, "compliant");
    for (deadline, status) in [
        ("2026-10-31T12:00:00Z", 200),
        ("2026-10-31T12:00:00.5Z", 200),
        ("2026-10-31T12:00:00.123456789Z", 200),
        ("yesterday", 400),
        ("2026-10-31T12:00:00+00:00", 400),
        ("2026-10-31T12:00:00.1234567890Z", 400),
        ("2026-10-31T12:00:00z", 400),
        ("2026-02-30T12:00:00Z", 400),
    ] {
        let mut value = contract::request(json!({}));
        value["context"]["deadline"] = json!(deadline);
        let body = serde_json::to_vec(&value).unwrap();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(stream, "POST /invoke HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
        assert_eq!(
            contract::read_response(&mut stream).unwrap().status,
            status,
            "{deadline}"
        );
    }
}
