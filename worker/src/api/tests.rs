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

// --- execution limits: every budget field ---------------------------------------------------

fn with_budgets(budgets: Value) -> ExecuteRequest {
    let mut message = request("wasm", None);
    message["projection"]["budgets"] = budgets;
    parse_request(&bytes(&message)).unwrap()
}

fn worker(max_memory_bytes: u64, max_wall_clock_ms: u64) -> Ceilings {
    Ceilings {
        max_memory_bytes,
        max_wall_clock_ms,
    }
}

const MIB: u64 = 1024 * 1024;

#[test]
fn execution_limits_carry_every_budget_of_the_revision() {
    use crate::limits::Field;
    let budgets =
        json!({"fuel": 123_456, "memory_bytes": 3 * MIB, "timeout_ms": 2500, "max_host_calls": 7});
    let limits = with_budgets(budgets)
        .execution_limits(worker(512 * MIB, 60_000))
        .unwrap();
    assert_eq!(limits.fuel, 123_456);
    assert_eq!(limits.max_host_calls, 7);
    assert_eq!(limits.timeout, std::time::Duration::from_millis(2500));
    assert_eq!(limits.limits.value(Field::GuestMemoryBytes), 3 * MIB);
}

#[test]
fn the_timeout_is_the_shorter_of_the_projection_and_the_worker() {
    let budgets =
        json!({"fuel": 1, "memory_bytes": MIB, "timeout_ms": 90_000, "max_host_calls": 1});
    let limits = with_budgets(budgets)
        .execution_limits(worker(MIB, 1500))
        .unwrap();
    assert_eq!(limits.timeout, std::time::Duration::from_millis(1500));
}

#[test]
fn the_memory_limit_is_the_minimum_of_the_budget_the_worker_and_the_compatibility_ceiling() {
    use crate::limits::Field;
    let memory = |budget: u64, worker_max: u64| {
        let budgets =
            json!({"fuel": 1, "memory_bytes": budget, "timeout_ms": 1, "max_host_calls": 1});
        with_budgets(budgets).execution_limits(worker(worker_max, 1000))
    };
    // The budget is smallest.
    assert_eq!(
        memory(10 * MIB, 50 * MIB)
            .unwrap()
            .limits
            .value(Field::GuestMemoryBytes),
        10 * MIB
    );
    // The worker's setting equals the budget; the compatibility ceiling (256 MiB) is largest.
    assert_eq!(
        memory(50 * MIB, 50 * MIB)
            .unwrap()
            .limits
            .value(Field::GuestMemoryBytes),
        50 * MIB
    );
    // The compatibility ceiling binds when the worker allows more.
    assert_eq!(
        memory(256 * MIB, 512 * MIB)
            .unwrap()
            .limits
            .value(Field::GuestMemoryBytes),
        256 * MIB
    );
    // A budget the worker cannot honour is refused, never replaced: above the worker's
    // setting, and above the compatibility ceiling.
    for (budget, worker_max) in [(51 * MIB, 50 * MIB), (257 * MIB, 512 * MIB)] {
        let refusal = memory(budget, worker_max).unwrap_err();
        assert_eq!(
            format!("{refusal:?}"),
            "Refusal(InvalidRequest, BudgetAboveWorkerCeiling)"
        );
        assert_eq!(refusal.status(), 400);
    }
}

#[test]
fn a_projection_for_another_engine_is_refused_so_the_identity_names_the_engine_that_ran() {
    let mut message = request("wasm", None);
    message["projection"]["engine_version"] = json!("48.0.0");
    let refusal = parse_request(&bytes(&message))
        .unwrap()
        .execution_limits(worker(512 * MIB, 60_000))
        .unwrap_err();
    assert_eq!(
        format!("{refusal:?}"),
        "Refusal(InvalidRequest, EngineVersionMismatch)"
    );
}

#[test]
fn the_plan_carries_the_declared_imports_the_scopes_both_lists_grant_and_the_deadline() {
    let mut message = request("wasm", None);
    message["projection"]["imports"] = json!(["falcone:host/logging@0.2.0"]);
    message["projection"]["grants"][0]["scope"]["hosts"] =
        json!(["a.example.test", "b.example.test"]);
    message["credential"]["allowed_capabilities"][0]["scope"]["hosts"] =
        json!(["b.example.test", "c.example.test"]);
    let plan = parse_request(&bytes(&message)).unwrap().plan();
    assert_eq!(plan.imports, ["falcone:host/logging@0.2.0"]);
    // Only the scope both name.
    assert_eq!(plan.context.grants.hosts, ["b.example.test"]);
    assert_eq!(plan.activation_id, "act-unit-0001");
    // 2026-11-11T12:00:00Z
    assert_eq!(plan.request_deadline_millis, 1_794_398_400_000);
    // The claim strings are the redaction secrets, including every scope name.
    assert!(plan.secrets.iter().any(|secret| secret == "tenant-unit"));
    assert!(plan.secrets.iter().any(|secret| secret == "c.example.test"));
    assert!(plan.credential.contains("tenant-unit"));
}

#[test]
fn a_result_envelope_always_validates_against_the_request_it_answers() {
    let request = parse_request(&bytes(&request("wasm", Some(catalog())))).unwrap();
    let (status, success) =
        request.success(br#"{"statusCode":201,"body":{"a":1}}"#, vec!["line".into()]);
    assert_eq!(status, 200);
    parse_result(&success, &request).unwrap();
    // A bare JSON value is the body of a 200; anything that is not JSON is a failure.
    let (_, bare) = request.success(b"[1,2]", Vec::new());
    assert_eq!(
        serde_json::from_slice::<Value>(&bare).unwrap()["result"],
        json!({"statusCode": 200, "body": [1, 2]})
    );
    let (status, invalid) = request.success(b"not json", Vec::new());
    assert_eq!(status, 500);
    parse_result(&invalid, &request).unwrap();
    for (code, reason, expected) in [
        (Code::Timeout, Reason::DeadlineExceeded, 504),
        (Code::Cancelled, Reason::CancelRequested, 499),
        (Code::Busy, Reason::AtCapacity, 503),
        (Code::ExecutionFailed, Reason::UncertainSideEffects, 500),
    ] {
        let (status, failure) = request.failure(Refusal::new(code, reason), Vec::new());
        assert_eq!(status, expected);
        parse_result(&failure, &request).unwrap();
    }
}
