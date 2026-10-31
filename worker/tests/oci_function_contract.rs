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
