//! Service contract tests; shared runtime cases also run in restricted cluster pods.
use api::Rejection;
use falcone_serverless_worker::api;
use serde_json::json;

#[test]
fn contract_runtime_and_image_refusal_precedes_malformed_inputs() {
    for projection in [
        json!({"runtime_mode":"unknown"}),
        json!({"runtime_mode":false}),
        json!({"runtime_mode":"wasm", "image":"registry.invalid/image"}),
    ] {
        let bytes = serde_json::to_vec(&json!({"projection":projection})).unwrap();
        let Err(Rejection::Refused(refused)) = api::parse_request(&bytes) else {
            panic!("runtime refusal must precede shape checks");
        };
        let value = serde_json::to_value(refused.refusal).unwrap();
        assert_eq!(value["status"], 409);
        assert_eq!(value["code"], "runtime_mode_not_supported");
    }
}

#[path = "../src/qualification/execution_api.rs"]
pub mod cases;

#[test]
fn tcp_execute_reports_the_projection_identity_and_legacy_result() {
    cases::identity();
}

#[test]
fn tcp_listeners_separate_execute_from_control() {
    cases::listener_routes();
}

#[test]
fn tcp_execute_and_control_capacity_recover_independently() {
    cases::saturation();
}

#[test]
fn tcp_ceilings_deadlines_and_slot_recovery() {
    cases::ceilings_and_transfer_deadlines();
}

#[test]
fn tcp_service_uses_the_shortest_execution_timeout() {
    cases::timeout_selection();
}

#[test]
fn tcp_response_deadline_releases_slow_reader_permits() {
    cases::slow_readers();
}

#[test]
fn tcp_refusals_never_start_a_guest() {
    cases::refusals();
}

#[test]
fn tcp_service_enforces_declared_imports() {
    cases::declared_imports();
}

#[test]
fn tcp_guest_cannot_obtain_the_host_credential() {
    cases::credential_isolation();
}

#[test]
fn tcp_redaction_budget_releases_execution_capacity() {
    cases::redaction_budget();
}

#[test]
fn contract_serve_requires_explicit_v2_configuration() {
    let path = std::env::temp_dir().join(format!("run01-cli-{}.toml", std::process::id()));
    std::fs::write(&path, "pool_id='p'\nfalcone_api_endpoint='https://api.invalid'\nfalcone_dispatcher_endpoint='https://dispatcher.invalid'\n").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("serve")
        .arg(&path)
        .output()
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "service_requires_v2"
    );
}

#[test]
fn contract_imports_must_be_declared_and_globally_allowed() {
    for (import, declared, accepted) in [
        ("falcone:host/invocation-context@0.2.0", vec![], false),
        (
            "falcone:host/invocation-context@0.2.0",
            vec!["falcone:host/logging@0.2.0"],
            false,
        ),
        (
            "wasi:cli/environment@0.2.6",
            vec!["wasi:cli/environment@0.2.6"],
            false,
        ),
        (
            "falcone:host/invocation-context@0.2.0",
            vec!["falcone:host/invocation-context@0.2.0"],
            true,
        ),
    ] {
        let source = include_str!("../src/qualification/execution_api_fixtures/success.wat");
        let component = wat::parse_str(
            source.replace(";; IMPORTS", &format!("(import \"{import}\" (instance))")),
        )
        .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "fixtures/contract/request/valid/wasm-no-base.json"
        ))
        .unwrap();
        value["projection"]["imports"] = json!(declared);
        let request = api::parse_request(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            falcone_serverless_worker::server::validate_component(&component, &request),
            if accepted {
                Ok(())
            } else {
                Err("undeclared_import")
            }
        );
    }
}

#[test]
fn contract_runtime_fixture_binaries_match_their_sources_and_validate() {
    for name in [
        "success",
        "blocking",
        "logs",
        "context",
        "credential",
        "storage",
    ] {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/qualification/execution_api_fixtures");
        let bytes = std::fs::read(root.join(format!("{name}.wasm"))).unwrap();
        assert_eq!(
            wat::parse_file(root.join(format!("{name}.wat"))).unwrap(),
            bytes
        );
        falcone_serverless_worker::artifact::validate::validate(&bytes, 65536).unwrap();
    }
}

#[test]
fn contract_fixture_preserves_response_data_while_receiving_legacy_params() {
    let path =
        std::env::temp_dir().join(format!("run01-fixture-params-{}.json", std::process::id()));
    let mut request: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/contract/request/valid/wasm-no-base.json"
    ))
    .unwrap();
    request["params"]["input"] = json!("x".repeat(1024));
    std::fs::write(&path, serde_json::to_vec(&request["params"]).unwrap()).unwrap();
    let component = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/qualification/execution_api_fixtures/success.wasm");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("execute")
        .arg(component)
        .arg(&path)
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(output.status.success(), "guest execution failed");
    assert_eq!(output.stdout, br#"{"statusCode":200,"body":"ok"}"#);
}

#[test]
fn contract_credential_probe_detects_a_canary_in_the_last_memory_word() {
    let root = std::env::temp_dir().join(format!("run01-canary-probe-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let source = include_str!("../src/qualification/execution_api_fixtures/credential.wat")
        .replace(
            "(memory (export \"memory\") 64)",
            "(memory (export \"memory\") 64) (data (i32.const 4194296) \"credOnly\")",
        );
    let component = root.join("negative-control.wasm");
    let params = root.join("params.json");
    std::fs::write(&component, wat::parse_str(source).unwrap()).unwrap();
    std::fs::write(&params, b"{}").unwrap();
    let clean_component = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/qualification/execution_api_fixtures/credential.wasm");
    let clean = std::process::Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("execute")
        .arg(clean_component)
        .arg(&params)
        .output()
        .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("execute")
        .arg(component)
        .arg(params)
        .output()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        clean.status.success(),
        "memory probe must accept the clean control"
    );
    assert_eq!(clean.stdout, br#"{"statusCode":200,"body":"ok"}"#);
    assert!(
        !output.status.success(),
        "memory probe must detect the negative control"
    );
    assert_eq!(output.stderr, b"handler_trapped\n");
}

#[test]
fn contract_credential_redaction_covers_overlaps_prefixes_and_short_secrets() {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/contract/request/valid/wasm-no-base.json"
    ))
    .unwrap();
    value["credential"]["iss"] = json!("abc");
    value["credential"]["aud"] = json!("bcdef");
    value["credential"]["tenant"] = json!("ab");
    value["credential"]["workspace"] = json!("^");
    let request = api::parse_request(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        request.redact_logs(&["abcdef x abc ab ^".into()]),
        vec!["<redacted> x <redacted> <redacted> <redacted>"]
    );
    value["credential"]["iss"] = json!("Z".repeat(9000));
    let request = api::parse_request(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        request.redact_logs(&["Z".repeat(10000)]),
        vec!["<redacted>"]
    );
    // The transport's bounded log collector may already have cut the line.
    assert_eq!(request.redact_logs(&["Z".repeat(8192)]), vec!["<redacted>"]);
    value["credential"]["iss"] = json!("quote\"and\\slash");
    let request = api::parse_request(&serde_json::to_vec(&value).unwrap()).unwrap();
    let line = json!({"message":"quote\"and\\slash"}).to_string();
    assert_eq!(
        request.redact_logs(&[line]),
        vec!["{\"message\":\"<redacted>\"}"]
    );
    value["credential"]["iss"] = json!("é秘密");
    value["credential"]["aud"] = json!("秘密尾");
    let request = api::parse_request(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(request.redact_logs(&["é秘密尾".into()]), vec!["<redacted>"]);
}

#[test]
fn contract_maximum_request_redaction_fails_closed_within_two_seconds() {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/contract/request/valid/wasm-no-base.json"
    ))
    .unwrap();
    value["credential"]["allowed_capabilities"][0]["scope"]["hosts"] = json!([]);
    let baseline = serde_json::to_vec(&value).unwrap().len();
    let count = (8 * 1024 * 1024 - baseline + 1) / 12;
    value["credential"]["allowed_capabilities"][0]["scope"]["hosts"] =
        json!((0..count).map(|n| format!("s{n:08x}")).collect::<Vec<_>>());
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() <= 8 * 1024 * 1024);
    assert!(8 * 1024 * 1024 - bytes.len() < 12);
    let request = api::parse_request(&bytes).unwrap();
    let logs = vec!["z".repeat(8192); 1000];
    let started = std::time::Instant::now();
    assert_eq!(request.redact_logs(&logs), vec!["<redacted>"]);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn contract_timeout_selection_and_timeout_envelope_are_contract_values() {
    use falcone_serverless_worker::limits::Field;
    for (request_ms, projection_ms, worker_ms, expected) in [
        (100, 300, 500, 100),
        (300, 100, 500, 100),
        (300, 500, 100, 100),
    ] {
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "fixtures/contract/request/valid/wasm-no-base.json"
        ))
        .unwrap();
        value["timeout_ms"] = json!(request_ms);
        value["projection"]["budgets"]["timeout_ms"] = json!(projection_ms);
        let request = api::parse_request(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            request
                .execution_limits(worker_ms)
                .value(Field::GuestDeadlineMs),
            expected
        );
        let envelope = request
            .refusal_envelope(&api::Refusal::timeout(), vec![])
            .unwrap();
        assert_eq!(envelope["result"]["status"], 504);
        assert_eq!(envelope["result"]["code"], "timeout");
        api::parse_result(&serde_json::to_vec(&envelope).unwrap(), &request).unwrap();
    }
}
