//! Unit tests over inline messages. The fixture-driven conformance suite is the integration
//! test `execution_api_contract`; these run in the binary's test build, where every item of
//! this module must be reachable.

use super::*;
use serde_json::{Value, json};

const ARTIFACT: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const BASE: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

fn projection(mode: &str, base: Option<Value>) -> Value {
    let mut value = json!({
        "revision": "rev-unit-0001",
        "runtime_mode": mode,
        "artifact_digest": ARTIFACT,
        "component_digest": ARTIFACT,
        "language_profile": {"name": "profile-unit", "version": "1"},
        "engine_version": "49.0.5",
        "wasi_version": "0.2.6",
        "interface_version": "0.2.0",
        "imports": [],
        "budgets": {"fuel": 1, "memory_bytes": 1, "timeout_ms": 1, "max_host_calls": 1},
        "grants": [{
            "family": "http",
            "interface": "falcone:host/http@0.2.0",
            "scope": {"hosts": ["api.example.test"]}
        }]
    });
    if let Some(base) = base {
        value["base"] = base;
    }
    value
}

fn catalog() -> Value {
    json!({"kind": "catalog", "id": "base-unit", "version": "1.0.0", "digest": BASE})
}

fn request(mode: &str, base: Option<Value>) -> Value {
    json!({
        "version": "1",
        "type": "execute",
        "activation_id": "act-unit-0001",
        "attempt_id": "att-unit-0001",
        "attempt_number": 1,
        "deadline": "2026-11-11T12:00:00Z",
        "invocation": {"invocation_key": "inv-unit-0001"},
        "credential": {
            "iss": "issuer-unit", "aud": "audience-unit", "tenant": "tenant-unit",
            "workspace": "workspace-unit", "pool_profile": "pool-unit",
            "revision": "rev-unit-0001", "activation": "act-unit-0001",
            "attempt": "att-unit-0001", "iat": 1, "exp": 2,
            "allowed_capabilities": [{
                "family": "http",
                "interface": "falcone:host/http@0.1.0",
                "scope": {"hosts": ["api.example.test"]}
            }]
        },
        "projection": projection(mode, base),
        "params": {"__ow_path": "/v1/unit"}
    })
}

fn result(base_digest: Option<Value>) -> Value {
    let mut identity = json!({
        "function_revision": "rev-unit-0001",
        "runtime_mode": "wasm",
        "artifact_digest": ARTIFACT,
        "wasmtime_version": "49.0.5",
        "language_profile": {"name": "profile-unit", "version": "1"},
        "interface_version": "0.2.0"
    });
    if let Some(digest) = base_digest {
        identity["base_digest"] = digest;
    }
    json!({
        "version": "1", "type": "result",
        "activation_id": "act-unit-0001", "attempt_id": "att-unit-0001", "attempt_number": 1,
        "status": "success", "result": {"statusCode": 200}, "logs": [], "identity": identity
    })
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

#[test]
fn a_wasm_request_is_admitted_and_answered_with_its_identity() {
    let request = parse_request(&bytes(&request("wasm", Some(catalog())))).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap()["attempt_number"], 1);
    parse_result(&bytes(&result(Some(json!(BASE)))), &request).unwrap();
    let projection = parse_projection(&bytes(&projection("wasm", Some(catalog())))).unwrap();
    parse_result(&bytes(&result(Some(json!(BASE)))), &projection).unwrap();
    for inverse in [None, Some(Value::Null)] {
        assert!(parse_result(&bytes(&result(inverse)), &projection).is_err());
    }
}

#[test]
fn the_base_digest_is_present_exactly_when_a_base_is_pinned() {
    let unpinned = parse_projection(&bytes(&projection("wasm", None))).unwrap();
    parse_result(&bytes(&result(None)), &unpinned).unwrap();
    for digest in [Some(json!(BASE)), Some(Value::Null)] {
        assert!(parse_result(&bytes(&result(digest)), &unpinned).is_err());
    }
}

#[test]
fn another_runtime_mode_is_refused_before_anything_runs() {
    let Err(Rejection::Refused(refused)) = parse_request(&bytes(&request("oci", None))) else {
        panic!("not refused");
    };
    let refusal = serde_json::to_value(&refused.refusal).unwrap();
    assert_eq!(refusal["code"], "runtime_mode_not_supported");
    assert_eq!(refusal["retryable"], false);
    let attribution = refused.attribution.unwrap();
    assert_eq!(serde_json::to_value(attribution.attempt_number).unwrap(), 1);
    assert_eq!(
        serde_json::to_value(&attribution.activation_id).unwrap(),
        "act-unit-0001"
    );
    assert_eq!(
        serde_json::to_value(&attribution.attempt_id).unwrap(),
        "att-unit-0001"
    );
    assert!(*attribution.projection == parse_projection(&bytes(&projection("oci", None))).unwrap());
    let mut unknown = request("wasm", None);
    unknown["projection"]["runtime_mode"] = json!("native");
    assert!(matches!(
        parse_request(&bytes(&unknown)),
        Err(Rejection::Refused(Refused {
            attribution: None,
            ..
        }))
    ));
}

#[test]
fn malformed_messages_name_a_rule_and_never_an_input() {
    let mut bad = request("wasm", None);
    bad["credential"]["revision"] = json!("rev-secret-0002");
    let error = parse_request(&bytes(&bad)).unwrap_err();
    assert!(!format!("{error:?}{error}").contains("secret"));
    assert!(matches!(
        parse_request(b"{"),
        Err(Rejection::Malformed(Malformed::Syntax))
    ));
    assert!(matches!(
        parse_request(b"{}"),
        Err(Rejection::Malformed(Malformed::Shape))
    ));
}

#[test]
fn update_and_cancel_messages_parse() {
    let update = json!({
        "version": "1", "type": "update", "activation_id": "act-unit-0001",
        "attempt_id": "att-unit-0001", "sequence": 1,
        "update": {"kind": "progress", "completed": 2, "total": 1}
    });
    assert_eq!(
        parse_update(&bytes(&update)),
        Err(Malformed::Constraint("update.progress"))
    );
    let cancel = json!({
        "version": "1", "type": "cancel", "activation_id": "act-unit-0001",
        "attempt_id": "att-unit-0001", "cancel": {"state": "uncertain"}
    });
    assert!(parse_cancel(&bytes(&cancel)).is_ok());
}

#[test]
fn a_parsed_message_prints_no_value() {
    let request = parse_request(&bytes(&request("wasm", Some(catalog())))).unwrap();
    let printed = format!("{request:?} {request}");
    for value in [
        "act-unit-0001",
        "rev-unit-0001",
        "2026-11-11",
        ARTIFACT,
        BASE,
    ] {
        assert!(!printed.contains(value));
    }
}

// --- execution limits -----------------------------------------------------------------------

const DEADLINE_MS: u64 = 1_794_398_400_000;

fn worker() -> WorkerLimits {
    WorkerLimits {
        max_memory_bytes: 512 * 1024 * 1024,
        compatibility_memory_bytes: 256 * 1024 * 1024,
        max_wall_clock: std::time::Duration::from_millis(300_000),
        engine_version: "49.0.5",
    }
}

/// A request whose budgets are `(fuel, memory_bytes, timeout_ms, max_host_calls)`.
fn budgeted(fuel: u64, memory: u64, timeout: u64, calls: u64) -> ExecuteRequest {
    let mut document = request("wasm", None);
    document["projection"]["budgets"] = json!({
        "fuel": fuel, "memory_bytes": memory, "timeout_ms": timeout, "max_host_calls": calls
    });
    parse_request(&bytes(&document)).unwrap()
}

fn refusal_of(error: Refusal) -> (u16, Value) {
    let value = serde_json::to_value(&error).unwrap();
    (error.status(), value["detail"]["reason"].clone())
}

#[test]
fn the_deadline_is_read_as_unix_milliseconds() {
    for (stamp, millis) in [
        ("1970-01-01T00:00:00Z", 0),
        ("2000-02-29T23:59:59Z", 951_868_799_000),
        ("2026-11-11T12:00:00Z", 1_794_398_400_000),
        ("2026-11-11T12:00:30.250Z", 1_794_398_430_250),
        ("2026-11-11T12:00:30.2509Z", 1_794_398_430_250),
        ("2038-01-19T03:14:08Z", 2_147_483_648_000),
    ] {
        let mut document = request("wasm", None);
        document["deadline"] = json!(stamp);
        let parsed = parse_request(&bytes(&document)).unwrap();
        assert_eq!(parsed.deadline_unix_millis(), millis, "{stamp}");
    }
}

#[test]
fn every_budget_field_reaches_the_limits() {
    let limits = budgeted(12_345, 1024 * 1024, 90_000, 77)
        .execution_limits(&worker(), DEADLINE_MS - 3_600_000)
        .unwrap();
    assert_eq!(limits.fuel, 12_345);
    assert_eq!(limits.max_host_calls, 77);
    assert_eq!(limits.memory_bytes, 1024 * 1024);
    assert_eq!(limits.timeout, std::time::Duration::from_millis(90_000));
}

#[test]
fn the_guest_memory_limit_is_the_minimum_of_the_three_sources() {
    let ceiling = 256 * 1024 * 1024;
    // The budget is the smallest.
    let limits = budgeted(1, 4 * 1024 * 1024, 1, 1)
        .execution_limits(&worker(), 0)
        .unwrap();
    assert_eq!(limits.memory_bytes, 4 * 1024 * 1024);
    // The worker's own ceiling is the smallest.
    let mut small_worker = worker();
    small_worker.max_memory_bytes = 8 * 1024 * 1024;
    let limits = budgeted(1, 8 * 1024 * 1024, 1, 1)
        .execution_limits(&small_worker, 0)
        .unwrap();
    assert_eq!(limits.memory_bytes, 8 * 1024 * 1024);
    // The compatibility ceiling is the smallest, and a budget at it is honoured.
    let limits = budgeted(1, ceiling, 1, 1)
        .execution_limits(&worker(), 0)
        .unwrap();
    assert_eq!(limits.memory_bytes, ceiling);
}

#[test]
fn a_budget_above_a_worker_ceiling_is_refused_never_replaced() {
    let mut small_worker = worker();
    small_worker.max_memory_bytes = 8 * 1024 * 1024;
    for (request, worker) in [
        // Above the worker's own maximum.
        (budgeted(1, 8 * 1024 * 1024 + 1, 1, 1), small_worker),
        // Above the compatibility ceiling, though below the worker's maximum.
        (budgeted(1, 256 * 1024 * 1024 + 1, 1, 1), worker()),
    ] {
        let refused = request.execution_limits(&worker, 0).err().unwrap();
        assert_eq!(
            refusal_of(refused),
            (400, json!("budget_above_worker_ceiling"))
        );
    }
}

#[test]
fn another_engine_version_is_refused() {
    let mut other = worker();
    other.engine_version = "50.0.0";
    let refused = budgeted(1, 1, 1, 1)
        .execution_limits(&other, 0)
        .err()
        .unwrap();
    assert_eq!(refusal_of(refused), (400, json!("engine_version_mismatch")));
}

#[test]
fn the_shortest_of_the_three_timeouts_wins() {
    let millis = std::time::Duration::from_millis;
    // The request's deadline is 20 s away; the revision allows 90 s; the worker 300 s.
    let now = DEADLINE_MS - 20_000;
    let request = budgeted(1, 1, 90_000, 1);
    assert_eq!(
        request.execution_limits(&worker(), now).unwrap().timeout,
        millis(20_000)
    );
    // The revision's limit is the shortest.
    let request = budgeted(1, 1, 5_000, 1);
    assert_eq!(
        request.execution_limits(&worker(), now).unwrap().timeout,
        millis(5_000)
    );
    // The worker's wall clock is the shortest.
    let mut short_worker = worker();
    short_worker.max_wall_clock = millis(1_500);
    let request = budgeted(1, 1, 90_000, 1);
    assert_eq!(
        request
            .execution_limits(&short_worker, now)
            .unwrap()
            .timeout,
        millis(1_500)
    );
    // A deadline that has passed leaves no time at all.
    assert_eq!(
        request
            .execution_limits(&worker(), DEADLINE_MS + 1)
            .unwrap()
            .timeout,
        millis(0)
    );
}

#[test]
fn a_result_is_built_for_its_request_and_validates_against_it() {
    let parsed = parse_request(&bytes(&request("wasm", Some(catalog())))).unwrap();
    let handler = HandlerResult::from_guest(br#"{"statusCode":200,"body":"ok"}"#).unwrap();
    let success = ExecutionResult::answer(&parsed, Ok(handler), vec!["line".into()]).unwrap();
    parse_result(&serde_json::to_vec(&success).unwrap(), &parsed).unwrap();
    for code_and_reason in [
        (Code::Timeout, Reason::DeadlineExceeded),
        (Code::Busy, Reason::AtCapacity),
        (Code::ExecutionFailed, Reason::FuelExhausted),
        (Code::UndeclaredImport, Reason::ImportNotDeclared),
    ] {
        let failure = ExecutionResult::answer(
            &parsed,
            Err(Refusal::new(code_and_reason.0, code_and_reason.1)),
            Vec::new(),
        )
        .unwrap();
        parse_result(&serde_json::to_vec(&failure).unwrap(), &parsed).unwrap();
    }
    // Lines are cut to the schema's limit, and no more than 1000 are kept.
    let long = ExecutionResult::answer(
        &parsed,
        Err(Refusal::new(Code::Timeout, Reason::DeadlineExceeded)),
        std::iter::repeat_n("é".repeat(9000), 1200).collect(),
    )
    .unwrap();
    let encoded = serde_json::to_vec(&long).unwrap();
    parse_result(&encoded, &parsed).unwrap();
    let logs = serde_json::from_slice::<Value>(&encoded).unwrap()["logs"].clone();
    assert_eq!(logs.as_array().unwrap().len(), 1000);
    assert_eq!(logs[0].as_str().unwrap().chars().count(), 8192);
}

#[test]
fn a_guest_output_must_be_a_legacy_handler_result() {
    for bad in [
        &b"not json"[..],
        br#"{"statusCode":99}"#,
        br#"{"statusCode":600}"#,
        br#"{"statusCode":304,"body":"x"}"#,
        br#"{"statusCode":200,"extra":1}"#,
        br#"{"body":"x"}"#,
    ] {
        assert!(HandlerResult::from_guest(bad).is_none());
    }
    assert!(HandlerResult::from_guest(br#"{"statusCode":304}"#).is_some());
}

#[test]
fn the_credential_strings_are_distinct_and_never_printed() {
    let parsed = parse_request(&bytes(&request("wasm", None))).unwrap();
    let secrets = parsed.credential_secrets();
    assert!(secrets.contains(&"tenant-unit".to_owned()));
    assert!(secrets.contains(&"api.example.test".to_owned()));
    let mut sorted = secrets.clone();
    sorted.dedup();
    assert_eq!(secrets, sorted);
    assert!(!format!("{parsed:?}{parsed}").contains("tenant-unit"));
}
