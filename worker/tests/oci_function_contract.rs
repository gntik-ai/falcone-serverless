use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

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

/// The tester adopts every child of its own process, so tests that spawn processes in the
/// test process (references, testers, candidates) must not overlap.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

/// Listeners of the host that the tester cannot attribute (root services in the sandbox or
/// on a CI runner) fail the binding rule closed. That is host noise, never the
/// exclusive-binding diagnostic that names a listener of the candidate.
fn is_host_noise(line: &str) -> bool {
    line.starts_with("OCI-V1-BINDING: cannot attribute listener")
        && !line.contains("also listens on")
        && !line.contains("is not listening")
}

fn assert_rules(output: Output, expected: &[&str]) {
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
        .filter(|line| expected.contains(&"OCI-V1-BINDING") || !is_host_noise(line))
        .map(|line| line.split_once(':').unwrap().0)
        .collect();
    assert_eq!(ids, expected, "{stdout}");
}

#[test]
fn compliant_http_yields_no_violations() {
    let _serial = serial();
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
    let _serial = serial();
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
    let _serial = serial();
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

fn process_case(mode: &str, expected: &[&str]) {
    let port = free_port();
    assert_rules(
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
    let _serial = serial();
    for mode in ["compliant", "write-tmp"] {
        process_case(mode, &[]);
    }
}

#[test]
fn process_rules_and_filesystem_locations_are_isolated() {
    let _serial = serial();
    for (mode, rule) in [
        ("OCI-V1-BINDING", "OCI-V1-BINDING"),
        ("ignore-sigterm", "OCI-V1-SIGTERM"),
        ("setpgid-ignore-sigterm", "OCI-V1-SIGTERM"),
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
    let _serial = serial();
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
fn binding_snapshot_rejects_wildcard_ipv6_and_other_loopbacks() {
    let header = "  sl  local_address rem_address st\n";
    let good = format!("{header} 0: 0100007F:1F90 00000000:0000 0A\n");
    assert!(contract::check_binding_snapshot(&good, header, 8080).is_empty());
    for bad in ["00000000", "0200007F"] {
        let tcp = format!("{header} 0: {bad}:1F90 00000000:0000 0A\n");
        assert_eq!(
            contract::check_binding_snapshot(&tcp, header, 8080)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            ["OCI-V1-BINDING"]
        );
    }
    let ipv6 = format!(
        "{header} 0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A\n"
    );
    assert_eq!(
        contract::check_binding_snapshot(&good, &ipv6, 8080)
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        ["OCI-V1-BINDING"]
    );
    assert_eq!(
        contract::check_binding_snapshot(header, header, 8080)
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        ["OCI-V1-BINDING"]
    );
}

#[test]
fn process_lifecycle_signals_kills_and_reaps_without_network() {
    let _serial = serial();
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = contract::Candidate::spawn(
        8080,
        std::ffi::OsStr::new("/bin/sleep"),
        &["60".into()],
        &probe,
    )
    .unwrap();
    let pid = child.pid();
    assert!(child.terminate().unwrap());
    drop(child);
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "child was not reaped"
    );
    let child = contract::Candidate::spawn(
        8080,
        std::ffi::OsStr::new("/bin/sleep"),
        &["60".into()],
        &probe,
    )
    .unwrap();
    let pid = child.pid();
    drop(child);
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "early-error cleanup"
    );
}

#[test]
fn sigterm_ignore_is_bounded_and_killed_after_ten_seconds() {
    let _serial = serial();
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = contract::Candidate::spawn(
        8080,
        std::ffi::OsStr::new("/bin/sh"),
        &[
            "-c".into(),
            "trap '' TERM; : > \"$TMPDIR/ready\"; exec /bin/sleep 60".into(),
        ],
        &probe,
    )
    .unwrap();
    let pid = child.pid();
    let ready = Instant::now();
    while !probe.scratch().join("ready").exists() {
        assert!(ready.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(10));
    }
    let start = Instant::now();
    assert!(!child.terminate().unwrap());
    drop(child);
    assert!(start.elapsed() >= Duration::from_secs(10));
    assert!(start.elapsed() < Duration::from_secs(15));
    assert!(rustix::process::test_kill_process(pid).is_err());
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
fn cleanup_reaps_descendants_of_the_candidate() {
    let _serial = serial();
    let probe = contract::FilesystemProbe::new().unwrap();
    let child = contract::Candidate::spawn(
        8080,
        std::ffi::OsStr::new("/bin/sh"),
        &[
            "-c".into(),
            "/bin/sleep 60 & echo $! > \"$TMPDIR/child-pid\"; wait".into(),
        ],
        &probe,
    )
    .unwrap();
    let ready = Instant::now();
    let descendant = loop {
        if let Ok(text) = fs::read_to_string(probe.scratch().join("child-pid"))
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            break rustix::process::Pid::from_raw(pid).unwrap();
        }
        assert!(ready.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(10));
    };
    drop(child);
    assert!(
        rustix::process::test_kill_process(descendant).is_err(),
        "descendant was not killed/reaped"
    );
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

/// A synthetic `/proc` and process table. Scenarios are named lifecycle events attached to
/// an operation ("list /proc/100/task/100/fd"), fired right after that operation returns.
mod synthetic {
    use super::contract::ProcessTable;
    use rustix::process::Signal;
    use std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet},
        io,
        rc::Rc,
        time::Duration,
    };

    /// The tester itself: the child subreaper.
    pub const ME: i32 = 10;

    pub struct Proc {
        pub parent: i32,
        pub tasks: BTreeMap<i32, char>,
        pub sockets: BTreeSet<u64>,
        pub ignores_term: bool,
    }
    pub struct Socket {
        pub address: String,
        pub port: u16,
        pub inode: u64,
    }
    struct Event {
        operation: String,
        repeat: bool,
        fired: bool,
        action: Box<dyn FnMut(&mut World)>,
    }
    #[derive(Default)]
    pub struct World {
        pub procs: BTreeMap<i32, Proc>,
        pub sockets: Vec<Socket>,
        pub raw_tcp_rows: Vec<String>,
        pub signals: Vec<(i32, Signal)>,
        pub latency: Duration,
        /// Time one reap attempt takes.
        pub reap_latency: Duration,
        /// Exit status of a process that ended on its own.
        pub statuses: BTreeMap<i32, i32>,
        /// Pids whose fd directories refuse to be read.
        pub locked: BTreeSet<i32>,
        /// The TMPDIR the candidate was started with.
        pub scratch: Option<std::path::PathBuf>,
        /// Tasks that are listed but gone by the time they are read.
        pub ghosts: BTreeSet<(i32, i32)>,
        /// Pids whose children lists refuse to be read.
        pub unreadable_children: BTreeSet<i32>,
        handles: BTreeMap<u64, i32>,
        next_handle: u64,
        next_pid: i32,
        next_inode: u64,
        events: Vec<Event>,
    }

    impl World {
        pub fn add(&mut self, parent: i32, tids: &[i32]) -> i32 {
            let pid = tids[0];
            self.next_pid = self.next_pid.max(pid + 1);
            self.procs.insert(
                pid,
                Proc {
                    parent,
                    tasks: tids.iter().map(|tid| (*tid, 'S')).collect(),
                    sockets: BTreeSet::new(),
                    ignores_term: false,
                },
            );
            pid
        }
        pub fn fork(&mut self, parent: i32) -> i32 {
            let pid = self.next_pid.max(100);
            self.add(parent, &[pid])
        }
        pub fn listen(&mut self, pid: i32, address: &str, port: u16) -> u64 {
            self.next_inode += 1;
            let inode = 5000 + self.next_inode;
            self.sockets.push(Socket {
                address: address.to_owned(),
                port,
                inode,
            });
            if let Some(proc) = self.procs.get_mut(&pid) {
                proc.sockets.insert(inode);
            }
            inode
        }
        /// The process dies: a zombie until reaped, its children go to the subreaper.
        pub fn exit(&mut self, pid: i32) {
            let Some(proc) = self.procs.get_mut(&pid) else {
                return;
            };
            for state in proc.tasks.values_mut() {
                *state = 'Z';
            }
            let closed = std::mem::take(&mut proc.sockets);
            self.sockets.retain(|s| !closed.contains(&s.inode));
            for child in self.procs.values_mut().filter(|p| p.parent == pid) {
                child.parent = ME;
            }
        }
        pub fn set_state(&mut self, pid: i32, tid: i32, state: char) {
            self.procs.get_mut(&pid).unwrap().tasks.insert(tid, state);
        }
        /// True while the tester still holds an open handle to `pid`.
        pub fn holds(&self, pid: i32) -> bool {
            self.handles.values().any(|p| *p == pid)
        }
        pub fn killed(&self, pid: i32) -> bool {
            self.signals
                .iter()
                .any(|(p, s)| *p == pid && *s == Signal::KILL)
        }
        pub fn on(
            &mut self,
            operation: &str,
            repeat: bool,
            action: impl FnMut(&mut World) + 'static,
        ) {
            self.events.push(Event {
                operation: operation.to_owned(),
                repeat,
                fired: false,
                action: Box::new(action),
            });
        }
        fn fire(&mut self, operation: &str) {
            let mut events = std::mem::take(&mut self.events);
            for event in events
                .iter_mut()
                .filter(|e| e.operation == operation && (e.repeat || !e.fired))
            {
                event.fired = true;
                (event.action)(self);
            }
            events.append(&mut self.events);
            self.events = events;
            // Slow procfs reads; signals and reaps cost nothing.
            let procfs = ["read ", "list ", "link "];
            if !self.latency.is_zero() && procfs.iter().any(|p| operation.starts_with(p)) {
                std::thread::sleep(self.latency);
            }
        }
        fn table(&self, ipv6: bool) -> String {
            let mut text = String::from("  sl  local_address rem_address   st tx_queue rx_queue\n");
            for socket in self
                .sockets
                .iter()
                .filter(|s| s.address.contains(':') == ipv6)
            {
                let ip: std::net::IpAddr = socket.address.parse().unwrap();
                let address: String = match ip {
                    std::net::IpAddr::V4(ip) => format!("{:08X}", u32::from_ne_bytes(ip.octets())),
                    std::net::IpAddr::V6(ip) => ip
                        .octets()
                        .chunks(4)
                        .map(|w| format!("{:08X}", u32::from_ne_bytes(w.try_into().unwrap())))
                        .collect(),
                };
                text += &format!(
                    " 0: {address}:{:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 {} 1 0\n",
                    socket.port, socket.inode
                );
            }
            if !ipv6 {
                for row in &self.raw_tcp_rows {
                    text += row;
                    text.push('\n');
                }
            }
            text
        }
        fn children(&self, pid: i32) -> String {
            self.procs
                .iter()
                .filter(|(_, p)| p.parent == pid)
                .map(|(child, _)| child.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        }
        fn dead(&self, pid: i32) -> bool {
            self.procs[&pid].tasks.values().all(|s| *s == 'Z')
        }
    }

    fn not_found() -> io::Error {
        io::Error::from(io::ErrorKind::NotFound)
    }
    fn denied() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    #[derive(Clone, Default)]
    pub struct Fake(pub Rc<RefCell<World>>);
    impl Fake {
        pub fn new() -> Self {
            let fake = Self::default();
            fake.with(|w| {
                w.add(1, &[ME]);
            });
            fake
        }
        pub fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
            f(&mut self.0.borrow_mut())
        }
        fn operate<T>(&self, operation: String, run: impl FnOnce(&mut World) -> T) -> T {
            let mut world = self.0.borrow_mut();
            let result = run(&mut world);
            world.fire(&operation);
            result
        }
    }

    impl ProcessTable for Fake {
        fn me(&self) -> i32 {
            ME
        }
        fn read(&self, path: &str) -> io::Result<String> {
            self.operate(format!("read {path}"), |w| {
                let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
                match parts.as_slice() {
                    ["proc", "net", "tcp"] => Ok(w.table(false)),
                    ["proc", "net", "tcp6"] => Ok(w.table(true)),
                    ["proc", pid, "task", tid, file] => {
                        let (pid, tid): (i32, i32) = (pid.parse().unwrap(), tid.parse().unwrap());
                        let proc = w.procs.get(&pid).ok_or_else(not_found)?;
                        if w.ghosts.contains(&(pid, tid)) {
                            return Err(not_found());
                        }
                        let state = proc.tasks.get(&tid).ok_or_else(not_found)?;
                        match *file {
                            "status" => Ok(format!("Name:\tx\nState:\t{state} (state)\n")),
                            "children" if w.unreadable_children.contains(&pid) => Err(denied()),
                            "children" if tid == pid => Ok(w.children(pid)),
                            "children" => Ok(String::new()),
                            _ => Err(not_found()),
                        }
                    }
                    _ => Err(not_found()),
                }
            })
        }
        fn list(&self, path: &str) -> io::Result<Vec<String>> {
            self.operate(format!("list {path}"), |w| {
                let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
                let fds = |w: &World, pid: i32| {
                    if w.locked.contains(&pid) {
                        return Err(denied());
                    }
                    let proc = w.procs.get(&pid).ok_or_else(not_found)?;
                    Ok((0..proc.sockets.len()).map(|n| n.to_string()).collect())
                };
                match parts.as_slice() {
                    ["proc"] => Ok(w
                        .procs
                        .keys()
                        .map(i32::to_string)
                        .chain(["net".to_owned(), "self".to_owned()])
                        .collect()),
                    ["proc", pid, "task"] => {
                        let proc = w.procs.get(&pid.parse().unwrap()).ok_or_else(not_found)?;
                        let mut tids: Vec<_> = proc.tasks.keys().map(i32::to_string).collect();
                        tids.extend(
                            w.ghosts
                                .iter()
                                .filter(|(p, _)| p.to_string() == *pid)
                                .map(|(_, t)| t.to_string()),
                        );
                        Ok(tids)
                    }
                    ["proc", pid, "task", _, "fd"] | ["proc", pid, "fd"] => {
                        let pid = pid.parse().unwrap();
                        if w.procs
                            .get(&pid)
                            .is_some_and(|p| p.tasks.values().all(|s| *s == 'Z'))
                        {
                            return Ok(Vec::new());
                        }
                        fds(w, pid)
                    }
                    _ => Err(not_found()),
                }
            })
        }
        fn link(&self, path: &str) -> io::Result<String> {
            self.operate(format!("link {path}"), |w| {
                let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
                let (pid, fd) = match parts.as_slice() {
                    ["proc", pid, "task", _, "fd", fd] | ["proc", pid, "fd", fd] => (pid, fd),
                    _ => return Err(not_found()),
                };
                let proc = w.procs.get(&pid.parse().unwrap()).ok_or_else(not_found)?;
                let inode = proc
                    .sockets
                    .iter()
                    .nth(fd.parse().unwrap())
                    .ok_or_else(not_found)?;
                Ok(format!("socket:[{inode}]"))
            })
        }
        fn spawn(
            &self,
            command: &std::process::Command,
            _deadline: std::time::Instant,
        ) -> io::Result<(i32, u64)> {
            let scratch = command
                .get_envs()
                .find(|(key, _)| *key == "TMPDIR")
                .and_then(|(_, value)| value.map(std::path::PathBuf::from));
            self.operate("spawn".into(), |w| {
                // The candidate writes to its scratch directory, as it is allowed to.
                for n in 0..3 {
                    if let Some(dir) = &scratch {
                        let _ = std::fs::write(dir.join(format!("candidate-{n}")), b"x");
                    }
                }
                w.scratch = scratch;
                let pid = w.fork(ME);
                w.handles.insert(w.next_handle, pid);
                w.next_handle += 1;
                Ok((pid, w.next_handle - 1))
            })
        }
        fn open(&self, pid: i32) -> io::Result<u64> {
            self.operate(format!("open {pid}"), |w| {
                if !w.procs.contains_key(&pid) {
                    return Err(not_found());
                }
                w.handles.insert(w.next_handle, pid);
                w.next_handle += 1;
                Ok(w.next_handle - 1)
            })
        }
        fn release(&self, handle: u64) {
            self.0.borrow_mut().handles.remove(&handle);
        }
        fn signal(&self, handle: u64, signal: Signal) -> io::Result<()> {
            self.operate(format!("signal {handle}"), |w| {
                let pid = w.handles[&handle];
                w.signals.push((pid, signal));
                let kills = signal == Signal::KILL
                    || (signal == Signal::TERM
                        && w.procs.get(&pid).is_some_and(|p| !p.ignores_term));
                if kills && w.procs.contains_key(&pid) && !w.dead(pid) {
                    w.exit(pid);
                }
                Ok(())
            })
        }
        fn reap(&self, handle: u64) -> io::Result<bool> {
            self.operate(format!("reap {handle}"), |w| {
                std::thread::sleep(w.reap_latency);
                let pid = w.handles[&handle];
                let Some(proc) = w.procs.get(&pid) else {
                    w.handles.remove(&handle);
                    return Ok(true);
                };
                if !w.dead(pid) {
                    return Ok(false);
                }
                // Only the subreaper reaps; any other zombie is left to its parent.
                if proc.parent == ME {
                    w.procs.remove(&pid);
                }
                w.handles.remove(&handle);
                Ok(true)
            })
        }
        fn exit_status(&self, handle: u64) -> io::Result<Option<i32>> {
            let world = self.0.borrow();
            let pid = world.handles[&handle];
            Ok(world
                .procs
                .get(&pid)
                .filter(|_| world.dead(pid))
                .and_then(|_| world.statuses.get(&pid).copied()))
        }
    }
}

use synthetic::{Fake, ME};

const PORT: u16 = 8080;

fn deadline_in(seconds: u64) -> Instant {
    Instant::now() + Duration::from_secs(seconds)
}

fn binding(fake: &Fake) -> contract::Diagnostics {
    contract::inspect_binding(fake, PORT, deadline_in(30)).unwrap()
}

fn binding_message(fake: &Fake) -> Option<String> {
    binding(fake).get("OCI-V1-BINDING").cloned()
}

/// A candidate (pid 100) holding 127.0.0.1:$PORT, the tester's only child.
fn compliant_tree() -> Fake {
    let fake = Fake::new();
    fake.with(|w| {
        let root = w.add(ME, &[100]);
        w.listen(root, "127.0.0.1", PORT);
    });
    fake
}

#[test]
fn loopback_port_listener_owned_by_the_tree_is_compliant() {
    let fake = compliant_tree();
    assert!(binding(&fake).is_empty());
}

#[test]
fn listener_held_by_a_process_outside_the_tree_is_not_the_candidates() {
    let fake = compliant_tree();
    fake.with(|w| {
        let service = w.add(1, &[500]);
        w.listen(service, "0.0.0.0", 22);
        w.listen(service, "::", 22);
    });
    assert!(binding(&fake).is_empty());
}

#[test]
fn dual_listener_names_the_extra_wildcard_listener() {
    let fake = compliant_tree();
    fake.with(|w| {
        w.listen(100, "0.0.0.0", 40000);
    });
    let message = binding_message(&fake).expect("OCI-V1-BINDING");
    assert!(
        message.contains("also listens on 0.0.0.0:40000"),
        "{message}"
    );
    assert!(!message.contains("127.0.0.1:8080,"), "{message}");
}

#[test]
fn descendants_and_ipv6_and_other_loopbacks_are_whole_tree_violations() {
    for (address, expected) in [
        ("::", "[::]:41000"),
        ("127.0.0.2", "127.0.0.2:41000"),
        ("0.0.0.0", "0.0.0.0:41000"),
    ] {
        let fake = compliant_tree();
        fake.with(|w| {
            let child = w.add(100, &[101]);
            let grandchild = w.add(child, &[102]);
            w.listen(grandchild, address, 41000);
        });
        let message = binding_message(&fake).expect("OCI-V1-BINDING");
        assert!(
            message.contains(&format!("also listens on {expected}")),
            "{message}"
        );
    }
}

#[test]
fn live_thread_of_a_zombie_leader_holding_a_wildcard_listener_fails() {
    let fake = compliant_tree();
    fake.with(|w| {
        let leader = w.add(100, &[200, 201]);
        w.listen(leader, "0.0.0.0", 42000);
        w.set_state(leader, 200, 'Z');
    });
    let message = binding_message(&fake).expect("OCI-V1-BINDING");
    assert!(
        message.contains("also listens on 0.0.0.0:42000"),
        "{message}"
    );
}

#[test]
fn listener_nobody_is_seen_holding_fails_closed() {
    let fake = compliant_tree();
    fake.with(|w| {
        // Ownerless: in the table, in no process's descriptors.
        w.listen(9999, "0.0.0.0", 43000);
        // An outside holder whose descriptors cannot be read proves nothing.
        let service = w.add(1, &[500]);
        w.listen(service, "0.0.0.0", 22);
        w.locked.insert(500);
    });
    let message = binding_message(&fake).expect("OCI-V1-BINDING");
    assert!(message.contains("cannot attribute listener"), "{message}");
    assert!(message.contains("0.0.0.0:43000") && message.contains("0.0.0.0:22"));
    assert!(!message.contains("also listens on"), "{message}");
}

#[test]
fn candidate_without_the_loopback_port_listener_fails() {
    let fake = Fake::new();
    fake.with(|w| {
        w.add(ME, &[100]);
    });
    let message = binding_message(&fake).expect("OCI-V1-BINDING");
    assert!(
        message.contains("not listening on 127.0.0.1:$PORT"),
        "{message}"
    );
}

fn assert_unstable(fake: &Fake) {
    let message = binding_message(fake).expect("OCI-V1-BINDING");
    assert!(message.starts_with("inspection unstable"), "{message}");
}

#[test]
fn unreadable_fd_directory_fails_closed() {
    let fake = compliant_tree();
    fake.with(|w| {
        w.locked.insert(100);
    });
    assert_unstable(&fake);
}

#[test]
fn malformed_socket_and_task_state_fail_closed() {
    let fake = compliant_tree();
    fake.with(|w| {
        // A socket state outside the kernel's set.
        w.raw_tcp_rows
            .push(" 1: 0100007F:1F91 00000000:0000 FF 0 0 0 0 0 0 7777".into());
    });
    assert_unstable(&fake);

    let fake = compliant_tree();
    fake.with(|w| {
        w.set_state(100, 100, 'Q');
    });
    assert_unstable(&fake);

    let fake = compliant_tree();
    fake.with(|w| {
        w.raw_tcp_rows.push(" 1: nonsense 00000000:0000 0A".into());
    });
    assert_unstable(&fake);
}

#[test]
fn vanished_task_fails_closed() {
    let fake = compliant_tree();
    fake.with(|w| {
        w.ghosts.insert((100, 101));
    });
    assert_unstable(&fake);
}

#[test]
fn process_forked_during_the_fd_read_is_rescanned_and_its_listener_named() {
    let fake = compliant_tree();
    fake.with(|w| {
        w.on("list /proc/100/task/100/fd", false, |w| {
            let child = w.fork(100);
            w.listen(child, "0.0.0.0", 44000);
        });
    });
    let message = binding_message(&fake).expect("OCI-V1-BINDING");
    assert!(
        message.contains("also listens on 0.0.0.0:44000"),
        "{message}"
    );
}

#[test]
fn wildcard_listener_opened_while_resampling_is_not_missed() {
    let fake = compliant_tree();
    fake.with(|w| {
        w.on("list /proc/100/task/100/fd", false, |w| {
            w.listen(100, "0.0.0.0", 45000);
        });
    });
    let message = binding_message(&fake).expect("OCI-V1-BINDING");
    assert!(
        message.contains("also listens on 0.0.0.0:45000"),
        "{message}"
    );
}

#[test]
fn member_changing_state_after_the_last_fd_sample_fails_closed() {
    let fake = compliant_tree();
    fake.with(|w| {
        // Stopped and resumed again, between the fd sample and the final state read, every time.
        let mut stopped = false;
        w.on("list /proc/100/task/100/fd", true, move |w| {
            stopped = !stopped;
            w.set_state(100, 100, if stopped { 'T' } else { 'S' });
        });
    });
    assert_unstable(&fake);
}

#[test]
fn a_read_crossing_the_deadline_is_the_deadline_error_never_a_verdict() {
    let fake = compliant_tree();
    fake.with(|w| {
        w.on("read /proc/100/task/100/status", false, |_| {
            thread::sleep(Duration::from_millis(300))
        });
    });
    let error = contract::inspect_binding(&fake, PORT, Instant::now() + Duration::from_millis(150))
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
}

fn candidate(
    fake: &Fake,
    probe: &contract::FilesystemProbe,
    deadline: Instant,
) -> std::io::Result<contract::Candidate> {
    contract::Candidate::spawn_until(
        Box::new(fake.clone()),
        PORT,
        std::ffi::OsStr::new("/bin/true"),
        &[],
        probe,
        deadline,
    )
}

#[test]
fn deadline_expiring_during_acquisition_still_kills_and_reaps_the_root() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    let error = candidate(&fake, &probe, Instant::now())
        .err()
        .expect("deadline error");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    fake.with(|w| {
        assert!(w.killed(100), "root was not sent SIGKILL");
        assert!(!w.procs.contains_key(&100), "root was not reaped");
        assert_eq!(w.signals.len(), 1);
    });
}

#[test]
fn cleanup_whose_scan_runs_out_of_settle_time_has_already_killed_and_reaped_every_handle() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    fake.with(|w| {
        w.on("spawn", false, |w| {
            for _ in 0..40 {
                w.fork(100);
            }
        });
    });
    let mut child = candidate(&fake, &probe, deadline_in(60)).unwrap();
    let retained: Vec<i32> = fake.with(|w| w.procs.keys().copied().filter(|p| *p >= 100).collect());
    assert_eq!(retained.len(), 41);
    // Every procfs read now takes long enough that no scan can finish in the settle time.
    fake.with(|w| w.latency = Duration::from_millis(400));
    let start = Instant::now();
    let error = child.cleanup().unwrap_err();
    let spent = start.elapsed();
    assert!(error.to_string().contains("cleanup"), "{error}");
    assert!(spent >= contract::KILL_SETTLE, "{spent:?}");
    assert!(
        spent < contract::KILL_SETTLE + Duration::from_secs(1),
        "{spent:?}"
    );
    fake.with(|w| {
        assert!(
            retained.iter().all(|pid| w.killed(*pid)),
            "a retained handle was not sent SIGKILL"
        );
        // Reaped before the slow scan began, not left for a scan that never finished.
        assert!(
            w.procs.keys().all(|pid| *pid < 100),
            "killed but not reaped: {:?}",
            w.procs.keys()
        );
    });
    // Drop neither signals nor waits again.
    let signals = fake.with(|w| w.signals.len());
    let start = Instant::now();
    drop(child);
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(fake.with(|w| w.signals.len()), signals);
}

#[test]
fn descendant_that_forks_and_exits_between_refresh_and_lookup_is_killed_and_reaped_first() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = candidate(&fake, &probe, deadline_in(60)).unwrap();
    let grandchild = fake.with(|w| {
        let descendant = w.fork(100);
        // The refresh lists it; before the lookup opens it, it forks a child and exits.
        w.on(&format!("open {descendant}"), false, move |w| {
            let adopted = w.fork(descendant);
            w.set_state(adopted, adopted, 'S');
            w.exit(descendant);
        });
        descendant + 1
    });
    child.cleanup().unwrap();
    fake.with(|w| {
        assert!(w.killed(grandchild), "adopted child was not killed");
        assert!(
            w.procs.keys().all(|pid| *pid < 100),
            "left behind: {:?}",
            w.procs.keys()
        );
    });
}

#[test]
fn terminate_that_reaps_the_child_leaves_drop_with_nothing_to_signal() {
    use contract::ProcessTable;
    use std::{cell::RefCell, rc::Rc};
    struct Recording(
        contract::RealTable,
        Rc<RefCell<Vec<rustix::process::Signal>>>,
    );
    impl ProcessTable for Recording {
        fn me(&self) -> i32 {
            self.0.me()
        }
        fn read(&self, p: &str) -> std::io::Result<String> {
            self.0.read(p)
        }
        fn list(&self, p: &str) -> std::io::Result<Vec<String>> {
            self.0.list(p)
        }
        fn link(&self, p: &str) -> std::io::Result<String> {
            self.0.link(p)
        }
        fn spawn(&self, c: &std::process::Command, d: Instant) -> std::io::Result<(i32, u64)> {
            self.0.spawn(c, d)
        }
        fn open(&self, pid: i32) -> std::io::Result<u64> {
            self.0.open(pid)
        }
        fn release(&self, h: u64) {
            self.0.release(h)
        }
        fn signal(&self, h: u64, s: rustix::process::Signal) -> std::io::Result<()> {
            self.1.borrow_mut().push(s);
            self.0.signal(h, s)
        }
        fn reap(&self, h: u64) -> std::io::Result<bool> {
            self.0.reap(h)
        }
        fn exit_status(&self, h: u64) -> std::io::Result<Option<i32>> {
            self.0.exit_status(h)
        }
    }
    let _serial = serial();
    let log = Rc::new(RefCell::new(Vec::new()));
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = contract::Candidate::spawn_until(
        Box::new(Recording(contract::RealTable::default(), log.clone())),
        PORT,
        std::ffi::OsStr::new("/bin/sleep"),
        &["60".into()],
        &probe,
        deadline_in(60),
    )
    .unwrap();
    assert!(child.terminate().unwrap());
    assert_eq!(*log.borrow(), [rustix::process::Signal::TERM]);
    drop(child);
    assert_eq!(
        *log.borrow(),
        [rustix::process::Signal::TERM],
        "Drop signalled a reaped child"
    );
}

#[test]
fn child_in_another_process_group_ignoring_sigterm_is_killed_through_its_pidfd() {
    let _serial = serial();
    let port = free_port();
    let probe = contract::FilesystemProbe::new().unwrap();
    let reference = std::ffi::OsStr::new(env!("CARGO_BIN_EXE_oci-contract-reference"));
    let start = Instant::now();
    let mut child = contract::Candidate::spawn_until(
        Box::new(contract::RealTable::default()),
        port,
        reference,
        &["setpgid-ignore-sigterm".into()],
        &probe,
        deadline_in(3),
    )
    .unwrap();
    // Serving means setpgid and the SIGTERM trap are in place.
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "reference startup"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let pid = child.pid();
    assert_ne!(
        rustix::process::getpgid(Some(pid)).unwrap(),
        pid,
        "still in its own group"
    );
    let error = child.terminate().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    drop(child);
    assert!(
        start.elapsed() < Duration::from_secs(3) + contract::KILL_SETTLE + Duration::from_secs(1)
    );
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "child was not reaped"
    );
}

#[test]
fn check_process_ends_within_deadline_plus_kill_settle_even_when_the_candidate_resists() {
    let _serial = serial();
    let port = free_port();
    let start = Instant::now();
    let result = contract::check_process_until(
        port,
        std::ffi::OsStr::new(env!("CARGO_BIN_EXE_oci-contract-reference")),
        &["setpgid-ignore-sigterm".into()],
        deadline_in(4),
    );
    assert_eq!(
        result.expect_err("deadline error").kind(),
        std::io::ErrorKind::TimedOut
    );
    assert!(
        start.elapsed() < Duration::from_secs(4) + contract::KILL_SETTLE + Duration::from_secs(1)
    );
}

#[test]
fn scratch_removal_runs_out_of_time_once_and_drop_does_not_restart_it() {
    let mut probe = contract::FilesystemProbe::new().unwrap();
    for n in 0..20 {
        fs::write(probe.scratch().join(format!("entry-{n}")), b"x").unwrap();
    }
    let root = probe.root().to_path_buf();
    let error = probe.remove_tree(Instant::now()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    drop(probe);
    assert!(
        root.join("scratch/entry-0").exists(),
        "Drop restarted a removal that had already run out of time"
    );
    fs::remove_dir_all(&root).unwrap();

    // A probe whose deadline has passed never starts an unbounded traversal either.
    let probe = contract::FilesystemProbe::new_until(Instant::now()).unwrap();
    fs::write(probe.scratch().join("entry"), b"x").unwrap();
    let root = probe.root().to_path_buf();
    drop(probe);
    assert!(root.join("scratch/entry").exists());
    fs::remove_dir_all(&root).unwrap();

    let mut probe = contract::FilesystemProbe::new().unwrap();
    fs::write(probe.scratch().join("entry"), b"x").unwrap();
    let root = probe.root().to_path_buf();
    probe.remove_tree(deadline_in(30)).unwrap();
    assert!(!root.exists());

    // Within its deadline Drop still cleans up after an earlier failure.
    let probe = contract::FilesystemProbe::new_until(deadline_in(30)).unwrap();
    fs::write(probe.scratch().join("entry"), b"x").unwrap();
    let root = probe.root().to_path_buf();
    drop(probe);
    assert!(!root.exists());
}

#[test]
fn cleanup_crossing_the_deadline_is_the_timeout_within_the_wall_time_bound() {
    let fake = Fake::new();
    fake.with(|w| {
        // The candidate has a descendant; reaping it is slow enough to exhaust both the
        // kill settle time and the check deadline.
        w.on("spawn", false, |w| {
            w.fork(100);
        });
        w.on("reap 1", false, |_| {
            thread::sleep(Duration::from_millis(1500))
        });
    });
    let start = Instant::now();
    let result = contract::check_process_with(
        Box::new(fake.clone()),
        free_port(),
        std::ffi::OsStr::new("/bin/true"),
        &[],
        deadline_in(4),
    );
    let spent = start.elapsed();
    assert_eq!(
        result.expect_err("deadline error").kind(),
        std::io::ErrorKind::TimedOut,
        "a cleanup failure after the deadline must be the deadline error"
    );
    assert!(spent >= Duration::from_secs(4), "{spent:?}");
    assert!(
        spent < Duration::from_secs(4) + contract::KILL_SETTLE + Duration::from_secs(1),
        "{spent:?}"
    );
    // The scratch removal ran out of time with the check; Drop did not start it again.
    let scratch = fake.with(|w| w.scratch.clone()).expect("candidate scratch");
    assert!(
        scratch.join("candidate-0").exists(),
        "Drop restarted scratch removal after the deadline"
    );
    fs::remove_dir_all(scratch.parent().unwrap()).unwrap();
}

#[test]
fn handle_opened_after_the_settle_time_is_held_unsignalled_and_reported() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = candidate(&fake, &probe, deadline_in(60)).unwrap();
    let descendant = fake.with(|w| {
        let descendant = w.fork(100);
        // Acquiring the handle takes longer than the settle time allows. Ownership cannot be
        // confirmed in time, so the pid may have been reused: no signal, and the check fails.
        w.on(&format!("open {descendant}"), false, |_| {
            thread::sleep(contract::KILL_SETTLE + Duration::from_millis(200))
        });
        descendant
    });
    let start = Instant::now();
    let error = child.cleanup().unwrap_err();
    assert!(
        error.to_string().contains("could not be confirmed"),
        "{error}"
    );
    assert!(
        error.to_string().contains(&descendant.to_string()),
        "{error}"
    );
    assert!(start.elapsed() < contract::KILL_SETTLE + Duration::from_secs(1));
    fake.with(|w| {
        assert!(!w.killed(descendant), "an unconfirmed handle was signalled");
        assert!(
            !w.holds(descendant),
            "the unconfirmed handle was not closed"
        );
    });
}

#[test]
fn confirmation_crossing_the_deadline_holds_the_handle_and_fails_closed() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = candidate(&fake, &probe, deadline_in(60)).unwrap();
    let descendant = fake.with(|w| {
        let descendant = w.fork(100);
        // The root died, so the descendant belongs to the tester now; its confirmation read of
        // the tester's tasks is the one that crosses the settle time.
        w.on(&format!("open {descendant}"), false, move |w| {
            w.on("list /proc/10/task", false, |_| {
                thread::sleep(contract::KILL_SETTLE + Duration::from_millis(200))
            });
        });
        descendant
    });
    let error = child.cleanup().unwrap_err();
    assert!(
        error.to_string().contains("could not be confirmed"),
        "{error}"
    );
    fake.with(|w| assert!(!w.killed(descendant), "an unconfirmed handle was signalled"));
}

#[test]
fn confirmation_work_is_bounded_cumulatively() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut child = candidate(&fake, &probe, deadline).unwrap();
    let descendant = fake.with(|w| {
        // The root has many threads, so confirming a descendant against it reads many
        // children lists; no single read is slow, their sum is.
        let root = *w.procs.keys().filter(|p| **p >= 100).min().unwrap();
        for tid in 300..500 {
            w.set_state(root, tid, 'S');
        }
        let descendant = w.fork(root);
        // When its handle is opened, the root dies but is not reaped: it stays listed with
        // its 201 tasks, the descendant moves to the tester, and each read now takes 10 ms.
        w.on(&format!("open {descendant}"), false, move |w| {
            w.exit(root);
            w.latency = Duration::from_millis(10);
        });
        descendant
    });
    let start = Instant::now();
    let error = child.binding(PORT).unwrap_err();
    let spent = start.elapsed();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
    // Reading every list would take over 2 s; the check deadline stops it.
    assert!(
        spent < Duration::from_millis(500) + Duration::from_secs(1),
        "{spent:?}"
    );
    fake.with(|w| {
        assert!(!w.killed(descendant), "an unconfirmed handle was signalled");
        assert!(
            w.holds(descendant),
            "an opened handle was released for want of proof"
        );
    });
    fake.with(|w| w.latency = Duration::ZERO);
    drop(child);
}

#[test]
fn uncertain_ownership_holds_the_handle_unsignalled_until_it_is_confirmed() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = candidate(&fake, &probe, deadline_in(60)).unwrap();
    let (root, descendant) = fake.with(|w| {
        let root = *w.procs.keys().filter(|p| **p >= 100).min().unwrap();
        let descendant = w.fork(root);
        // Once the handle is open, the parent's children list cannot be read: ownership is
        // unknown, not foreign.
        w.on(&format!("open {descendant}"), false, move |w| {
            w.unreadable_children.insert(root);
        });
        (root, descendant)
    });
    let diagnostics = child.binding(PORT).unwrap();
    let message = diagnostics
        .get("OCI-V1-BINDING")
        .cloned()
        .unwrap_or_default();
    assert!(message.contains("inspection unstable"), "{message}");
    fake.with(|w| {
        assert!(
            w.holds(descendant),
            "an opened handle was released for want of proof"
        );
        assert!(
            !w.killed(descendant),
            "an unconfirmed handle was signalled while inspecting"
        );
        // The list becomes readable again: the next confirmation succeeds.
        w.unreadable_children.remove(&root);
    });
    child.cleanup().unwrap();
    fake.with(|w| {
        assert!(w.killed(descendant), "a confirmed handle was not killed");
        assert!(w.procs.keys().all(|pid| *pid < 100), "{:?}", w.procs.keys());
    });
}

#[test]
fn reaping_is_bounded_cumulatively_when_every_attempt_is_slow() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    fake.with(|w| {
        w.on("spawn", false, |w| {
            for _ in 0..40 {
                w.fork(100);
            }
        });
    });
    let mut child = candidate(&fake, &probe, deadline_in(60)).unwrap();
    let retained: Vec<i32> = fake.with(|w| w.procs.keys().copied().filter(|p| *p >= 100).collect());
    fake.with(|w| w.reap_latency = Duration::from_millis(300));
    let start = Instant::now();
    let error = child.cleanup().unwrap_err();
    let spent = start.elapsed();
    assert!(error.to_string().contains("cleanup"), "{error}");
    // Forty-one attempts would take 12 s: the settle time and the one attempt in flight when
    // it ran out bound it instead, with no further reap budget after it.
    assert!(
        spent < contract::KILL_SETTLE + Duration::from_millis(300) + Duration::from_secs(1),
        "{spent:?}"
    );
    fake.with(|w| {
        assert!(
            retained.iter().all(|pid| w.killed(*pid)),
            "a retained handle was not sent SIGKILL"
        );
    });
    let signals = fake.with(|w| w.signals.len());
    drop(child);
    assert_eq!(fake.with(|w| w.signals.len()), signals);
}

#[test]
fn candidate_that_cannot_be_executed_is_an_infrastructure_error_not_a_health_failure() {
    let fake = Fake::new();
    fake.with(|w| {
        w.on("spawn", false, |w| {
            w.statuses.insert(100, 127);
            w.exit(100);
        });
    });
    let error = contract::check_process_with(
        Box::new(fake.clone()),
        free_port(),
        std::ffi::OsStr::new("/bin/true"),
        &[],
        deadline_in(60),
    )
    .expect_err("infrastructure error");
    assert_ne!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
    assert!(
        error.to_string().contains("could not be executed"),
        "{error}"
    );
    assert!(error.to_string().contains("127"), "{error}");
}

#[test]
fn candidate_that_runs_and_fails_health_is_still_a_health_violation() {
    let fake = Fake::new();
    fake.with(|w| {
        w.on("spawn", false, |w| {
            w.statuses.insert(100, 1);
            w.exit(100);
        });
    });
    let diagnostics = contract::check_process_with(
        Box::new(fake.clone()),
        free_port(),
        std::ffi::OsStr::new("/bin/true"),
        &[],
        deadline_in(60),
    )
    .unwrap();
    assert!(
        diagnostics.contains_key("OCI-V1-HEALTHZ"),
        "{:?}",
        *diagnostics
    );
}

#[test]
fn programs_that_cannot_be_executed_exit_with_infrastructure_status_two() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = serial();
    let dir = tempfile_dir("no-exec");
    let script = dir.join("script");
    fs::write(&script, "#!/nonexistent/interpreter\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let directory = dir.join("directory");
    fs::create_dir(&directory).unwrap();
    for program in [&script, &directory] {
        let output = tester(
            Some(free_port()),
            &["process", "--", program.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(2), "{}", program.display());
        assert!(
            String::from_utf8_lossy(&output.stdout).is_empty(),
            "a start failure reported as a rule violation: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

/// Ends a process the test itself spawned, through a pidfd of its own.
fn end_and_reap(pid: i32) {
    use rustix::process::{
        Pid, PidfdFlags, Signal, WaitOptions, pidfd_open, pidfd_send_signal, waitpid,
    };
    let pid = Pid::from_raw(pid).expect("pid");
    let fd = pidfd_open(pid, PidfdFlags::empty()).expect("pidfd");
    pidfd_send_signal(&fd, Signal::KILL).expect("kill");
    waitpid(Some(pid), WaitOptions::empty()).expect("reap");
}

fn tempfile_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oci-gate-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A candidate that, if it ever ran, would fork a long-lived child and leave a marker.
fn forking_candidate(marker: &Path) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!("sleep 60 & echo ran > {}; wait", marker.display()));
    command
}

fn refused() -> std::io::Error {
    std::io::Error::from_raw_os_error(rustix::io::Errno::MFILE.raw_os_error())
}

#[test]
fn failing_to_retain_the_spawned_child_never_runs_the_candidate() {
    let _serial = serial();
    let dir = tempfile_dir("gate-closed");
    let marker = dir.join("ran");
    let command = forking_candidate(&marker);
    let spawned = std::cell::Cell::new(0);
    let start = Instant::now();
    let error = contract::spawn_retained(
        &command,
        |pid| {
            spawned.set(pid);
            Err(std::io::Error::other("pidfd_open refused"))
        },
        deadline_in(60),
    )
    .unwrap_err();
    assert!(error.to_string().contains("pidfd_open refused"), "{error}");
    assert!(error.to_string().contains("never started"), "{error}");
    assert!(start.elapsed() < contract::KILL_SETTLE + Duration::from_secs(1));
    // Not even a zombie is left behind, and the candidate had no time to fork or write.
    let pid = spawned.get();
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the gate outlived the failed handle acquisition"
    );
    thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "the candidate ran without a handle");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_transient_refusal_to_retain_the_spawned_child_is_retried_until_the_deadline() {
    let _serial = serial();
    let mut command = Command::new("/bin/sleep");
    command.arg("60");
    let mut refusals = 2;
    let (pid, handle) = contract::spawn_retained(
        &command,
        |_| {
            if refusals > 0 {
                refusals -= 1;
                Err(refused())
            } else {
                Ok(7)
            }
        },
        deadline_in(60),
    )
    .unwrap();
    assert_eq!(handle, 7);
    end_and_reap(pid);

    // Past the deadline the handle is asked for once, and the refusal ends the attempt.
    let mut asked = 0;
    let error = contract::spawn_retained(
        &command,
        |_| {
            asked += 1;
            Err(refused())
        },
        Instant::now(),
    )
    .unwrap_err();
    assert_eq!(asked, 1, "{error}");
}

#[test]
fn released_candidate_runs_with_only_the_given_environment_and_no_stdin() {
    let _serial = serial();
    let dir = tempfile_dir("gate-open");
    let out = dir.join("out");
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!(
            "env | sort > {0}.tmp; cat >> {0}.tmp; mv {0}.tmp {0}",
            out.display()
        ))
        .env_clear()
        .env("PORT", "8080")
        .env("HOME", "/h o")
        .current_dir(&dir);
    let (pid, _) = contract::spawn_retained(&command, |_| Ok(1), deadline_in(60)).unwrap();
    let start = Instant::now();
    while !out.exists() && start.elapsed() < Duration::from_secs(10) {
        thread::sleep(Duration::from_millis(10));
    }
    let text = fs::read_to_string(&out).unwrap();
    // `env` is the only extra the shell itself adds (PWD); nothing leaks from the tester.
    let names: Vec<&str> = text.lines().map(|l| l.split('=').next().unwrap()).collect();
    assert!(
        names
            .iter()
            .all(|n| ["HOME", "PORT", "PWD", "_"].contains(n)),
        "{text}"
    );
    assert!(text.contains("HOME=/h o"), "{text}");
    assert!(text.contains("PORT=8080"), "{text}");
    // stdin is /dev/null: `cat` returned at once instead of waiting for the tester.
    end_and_reap(pid);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn descendant_whose_pid_is_reused_before_its_handle_is_opened_is_never_signalled() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = candidate(&fake, &probe, deadline_in(60)).unwrap();
    let outsider = fake.with(|w| {
        let descendant = w.fork(100);
        // Listed by the walk; before the handle is opened it exits, its parent reaps it and
        // an unrelated process (not ours) is started under the same pid.
        w.on(
            &format!("read /proc/{descendant}/task/{descendant}/children"),
            false,
            move |w| {
                w.procs.remove(&descendant);
                w.add(1, &[descendant]);
            },
        );
        descendant
    });
    child.cleanup().unwrap();
    fake.with(|w| {
        assert!(!w.killed(outsider), "an unrelated process was signalled");
        assert!(
            w.procs.contains_key(&outsider),
            "unrelated process was reaped"
        );
        assert!(
            w.signals.iter().all(|(pid, _)| *pid == 100),
            "{:?}",
            w.signals
        );
    });
}

#[test]
fn binding_inspection_retains_handles_and_never_adopts_a_reused_pid() {
    let fake = Fake::new();
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = candidate(&fake, &probe, deadline_in(60)).unwrap();
    let outsider = fake.with(|w| {
        w.listen(100, "127.0.0.1", PORT);
        let descendant = w.fork(100);
        w.on(
            &format!("read /proc/{descendant}/task/{descendant}/children"),
            false,
            move |w| {
                w.procs.remove(&descendant);
                w.add(1, &[descendant]);
            },
        );
        descendant
    });
    let diagnostics = child.binding(PORT).unwrap();
    assert!(diagnostics.is_empty(), "{:?}", *diagnostics);
    child.cleanup().unwrap();
    fake.with(|w| assert!(!w.killed(outsider), "an unrelated process was signalled"));
}

fn post_deadline(port: u16, deadline: &str) -> u16 {
    use std::io::Write;
    let body = serde_json::to_vec(&contract::request_with_deadline(json!({}), deadline)).unwrap();
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "POST /invoke HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    contract::read_response(&mut stream).unwrap().status
}

#[test]
fn reference_and_tester_apply_the_contract_deadline_syntax() {
    let _serial = serial();
    let schema: Value = serde_json::from_str(contract::REQUEST_SCHEMA).unwrap();
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap();
    let port = free_port();
    let _candidate = reference(port, "compliant");
    let cases = [
        ("2026-10-31T12:00:00Z", true),
        ("2026-10-31T12:00:00.5Z", true),
        ("2026-10-31T12:00:00.123456789Z", true),
        ("2000-02-29T00:00:00Z", true),
        ("2026-12-31T23:59:60Z", true),
        ("2026-10-31T12:00:00+01:00", false),
        ("2026-10-31T12:00:00", false),
        ("2026-10-31T12:00:00.Z", false),
        ("2026-10-31T12:00:00.12345678901Z", false),
        ("2026-13-31T12:00:00Z", false),
        ("2026-02-30T00:00:00Z", false),
        ("1900-02-29T00:00:00Z", false),
        ("", false),
    ];
    for (deadline, valid) in cases {
        let request = contract::request_with_deadline(json!({}), deadline);
        assert_eq!(validator.is_valid(&request), valid, "schema: {deadline}");
        assert_eq!(
            contract::utc_deadline(deadline),
            valid,
            "tester: {deadline}"
        );
        assert_eq!(
            post_deadline(port, deadline),
            if valid { 200 } else { 400 },
            "reference: {deadline}"
        );
    }
    // The deadlines the tester sends are exactly the ones the schema calls valid or not.
    for deadline in contract::VALID_DEADLINES {
        assert!(
            validator.is_valid(&contract::request_with_deadline(json!({}), deadline)),
            "{deadline}"
        );
    }
    for deadline in contract::MALFORMED_DEADLINES {
        assert!(
            !validator.is_valid(&contract::request_with_deadline(json!({}), deadline)),
            "{deadline}"
        );
    }
    assert!(
        contract::VALID_DEADLINES.iter().any(|d| d.contains('.')),
        "fractional UTC is exercised"
    );
}

#[test]
fn reference_that_knows_one_literal_deadline_fails_the_request_rule() {
    let _serial = serial();
    let port = free_port();
    let _candidate = reference(port, "literal-deadline");
    assert_rules(tester(Some(port), &["http"]), &["OCI-V1-REQUEST"]);
}

#[test]
fn dual_listener_process_fails_binding_naming_the_extra_listener() {
    let _serial = serial();
    let output = tester(
        Some(free_port()),
        &[
            "process",
            "--",
            env!("CARGO_BIN_EXE_oci-contract-reference"),
            "dual-listener",
        ],
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    assert!(lines[0].starts_with("OCI-V1-BINDING: "), "{stdout}");
    assert!(lines[0].contains("also listens on 0.0.0.0:"), "{stdout}");
}
