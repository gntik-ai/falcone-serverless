//! Unit tests over inline messages. The fixture-driven conformance suite is the integration
//! test `execution_api_contract`; these run in the library's test build.

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

// --- the execution service's view of a request -----------------------------------------------

mod service {
    use super::*;
    use crate::api::{ExecuteRequest, HandlerResult, WorkerCeilings};
    use crate::limits::{Field, Limits};

    /// `deadline` of the requests below: 2026-11-11T12:00:30.500Z.
    const DEADLINE_MS: u64 = 1_794_398_430_500;

    const MIB: u64 = 1024 * 1024;

    fn admitted(budgets: Value) -> ExecuteRequest {
        let mut value = request("wasm", None);
        value["deadline"] = json!("2026-11-11T12:00:30.500Z");
        value["projection"]["budgets"] = budgets;
        parse_request(&bytes(&value)).unwrap()
    }

    fn budgets() -> Value {
        json!({"fuel": 1234, "memory_bytes": 2 * MIB, "timeout_ms": 8000, "max_host_calls": 7})
    }

    fn worker(max_memory_bytes: u64, max_wall_clock_ms: u64) -> WorkerCeilings {
        WorkerCeilings {
            max_memory_bytes,
            max_wall_clock_ms,
        }
    }

    /// Twenty seconds before the request's deadline.
    const NOW_MS: u64 = DEADLINE_MS - 20_000;

    fn limits(request: &ExecuteRequest, worker: WorkerCeilings, now: u64) -> Limits {
        request.execution_limits(worker, now).unwrap()
    }

    #[test]
    fn execution_limits_apply_every_budget_and_keep_the_other_fields() {
        let limits = limits(&admitted(budgets()), worker(512 * MIB, 300_000), NOW_MS);
        let expected = [
            (Field::CompilerCpuSeconds, 120),
            (Field::CompilerMemoryBytes, 4 * 1024 * MIB),
            (Field::GuestMemoryBytes, 2 * MIB),
            (Field::GuestToHostBytes, MIB),
            (Field::HostToGuestBytes, MIB),
            (Field::GuestDeadlineMs, 8000),
            (Field::HostCallDeadlineMs, 8000),
            (Field::TerminationBudgetMs, 500),
        ];
        for (field, value) in expected {
            assert_eq!(limits.value(field), value, "{}", field.name());
        }
        assert_eq!(expected.len(), Field::ALL.len());
        assert_eq!(limits.fuel(), 1234);
        assert_eq!(limits.max_host_calls(), 7);
    }

    #[test]
    fn guest_memory_is_the_least_of_the_budget_the_worker_maximum_and_the_ceiling() {
        let memory = |budget: u64, maximum: u64| {
            let mut budgets = budgets();
            budgets["memory_bytes"] = json!(budget);
            limits(&admitted(budgets), worker(maximum, 300_000), NOW_MS)
                .value(Field::GuestMemoryBytes)
        };
        assert_eq!(memory(MIB, 512 * MIB), MIB);
        assert_eq!(memory(150 * MIB, 200 * MIB), 150 * MIB);
        assert_eq!(memory(200 * MIB, 200 * MIB), 200 * MIB);
        // Above the compatibility ceiling of 256 MiB the ceiling wins, never the larger budget.
        assert_eq!(memory(1024 * MIB, 2048 * MIB), 256 * MIB);
        assert_eq!(memory(300 * MIB, 2048 * MIB), 256 * MIB);
    }

    #[test]
    fn a_memory_budget_above_the_worker_maximum_is_refused_not_replaced() {
        let mut over = budgets();
        over["memory_bytes"] = json!(200 * MIB + 1);
        let refusal = admitted(over)
            .execution_limits(worker(200 * MIB, 300_000), NOW_MS)
            .err()
            .unwrap();
        let refusal = serde_json::to_value(&refusal).unwrap();
        assert_eq!(refusal["status"], 400);
        assert_eq!(refusal["code"], "invalid_request");
        assert_eq!(refusal["detail"]["reason"], "budget_above_worker_ceiling");
        assert_eq!(refusal["retryable"], false);
    }

    #[test]
    fn the_shortest_of_the_three_time_limits_wins() {
        let deadline = |request: u64, projection: u64, wall_clock: u64| {
            let mut budgets = budgets();
            budgets["timeout_ms"] = json!(projection);
            limits(
                &admitted(budgets),
                worker(512 * MIB, wall_clock),
                DEADLINE_MS - request,
            )
        };
        // The request's own deadline, the projection, the worker: each can be the shortest.
        for (request, projection, wall_clock, expected) in [
            (3_000, 8_000, 300_000, 3_000),
            (20_000, 8_000, 300_000, 8_000),
            (20_000, 8_000, 5_000, 5_000),
            (20_000, 60_000, 90_000, 20_000),
            (4_000, 4_000, 4_000, 4_000),
        ] {
            let limits = deadline(request, projection, wall_clock);
            assert_eq!(limits.value(Field::GuestDeadlineMs), expected);
            // A host call never outlives the execution, nor the 30 s host-call cap.
            assert_eq!(
                limits.value(Field::HostCallDeadlineMs),
                expected.min(30_000)
            );
        }
        // Timeouts beyond the 30 s compatibility cap are honoured, the host-call cap is not.
        let long = deadline(120_000, 100_000, 300_000);
        assert_eq!(long.value(Field::GuestDeadlineMs), 100_000);
        assert_eq!(long.value(Field::HostCallDeadlineMs), 30_000);
        // A deadline already past leaves nothing to run in.
        let late = limits(
            &admitted(budgets()),
            worker(512 * MIB, 300_000),
            DEADLINE_MS + 1,
        );
        assert_eq!(late.value(Field::GuestDeadlineMs), 0);
    }

    #[test]
    fn fuel_and_host_calls_come_from_the_budgets_not_the_compatibility_profile() {
        let compatibility = Limits::compatibility();
        assert_eq!(compatibility.fuel(), u64::MAX);
        assert_eq!(compatibility.max_host_calls(), u64::MAX);
        let mut custom = budgets();
        custom["fuel"] = json!(9_007_199_254_740_991u64);
        custom["max_host_calls"] = json!(1);
        let limits = limits(&admitted(custom), worker(512 * MIB, 300_000), NOW_MS);
        assert_eq!(limits.fuel(), 9_007_199_254_740_991);
        assert_eq!(limits.max_host_calls(), 1);
    }

    #[test]
    fn a_deadline_is_read_as_unix_milliseconds() {
        let millis = |text: &str| {
            let mut value = request("wasm", None);
            value["deadline"] = json!(text);
            value["projection"]["budgets"]["timeout_ms"] = json!(9_007_199_254_740_991u64);
            let request = parse_request(&bytes(&value)).unwrap();
            request
                .execution_limits(worker(512 * MIB, u64::MAX), 0)
                .unwrap()
                .value(Field::GuestDeadlineMs)
        };
        assert_eq!(millis("2026-11-11T12:00:30.500Z"), 1_794_398_430_500);
        assert_eq!(millis("1970-01-01T00:00:00Z"), 0);
        assert_eq!(millis("1970-01-01T00:00:01Z"), 1_000);
        // Leap day, century boundaries and fractions of any length.
        assert_eq!(millis("2024-02-29T23:59:59Z"), 1_709_251_199_000);
        assert_eq!(millis("2000-03-01T00:00:00Z"), 951_868_800_000);
        assert_eq!(millis("1999-12-31T23:59:59Z"), 946_684_799_000);
        assert_eq!(millis("2100-03-01T00:00:00Z"), 4_107_542_400_000);
        assert_eq!(millis("2026-11-11T12:00:30.5Z"), 1_794_398_430_500);
        assert_eq!(millis("2026-11-11T12:00:30.123456789Z"), 1_794_398_430_123);
    }

    #[test]
    fn every_import_of_the_component_must_be_declared_by_the_revision() {
        let with = |imports: Value| {
            let mut value = request("wasm", None);
            value["projection"]["imports"] = imports;
            parse_request(&bytes(&value)).unwrap()
        };
        let component = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        let logging = "falcone:host/logging@0.2.0";
        let context = "falcone:host/invocation-context@0.2.0";
        // An empty list means none.
        assert!(with(json!([])).check_imports(&component(&[])).is_ok());
        let refusal = with(json!([]))
            .check_imports(&component(&[context]))
            .err()
            .unwrap();
        let refusal = serde_json::to_value(&refusal).unwrap();
        assert_eq!(refusal["status"], 422);
        assert_eq!(refusal["code"], "undeclared_import");
        assert_eq!(refusal["detail"]["reason"], "import_not_declared");
        // A non-empty list that lacks one of them.
        let declared = with(json!([logging]));
        assert!(declared.check_imports(&component(&[logging])).is_ok());
        assert!(
            declared
                .check_imports(&component(&[logging, context]))
                .is_err()
        );
        assert!(declared.check_imports(&component(&[context])).is_err());
        // Declaring more than the component imports is fine.
        assert!(
            with(json!([logging, context]))
                .check_imports(&component(&[context]))
                .is_ok()
        );
    }

    #[test]
    fn the_projection_must_name_the_pinned_engine() {
        let request = admitted(budgets());
        assert!(request.check_engine("49.0.5").is_ok());
        let refusal = request.check_engine("49.0.6").err().unwrap();
        let refusal = serde_json::to_value(&refusal).unwrap();
        assert_eq!(refusal["code"], "invalid_request");
        assert_eq!(refusal["detail"]["reason"], "engine_version_mismatch");
    }

    #[test]
    fn the_credential_strings_cover_the_claims_and_their_grants() {
        let request = admitted(budgets());
        let strings = request.credential_strings();
        for expected in [
            "issuer-unit",
            "audience-unit",
            "tenant-unit",
            "workspace-unit",
            "pool-unit",
            "rev-unit-0001",
            "act-unit-0001",
            "att-unit-0001",
            "falcone:host/http@0.1.0",
            "api.example.test",
        ] {
            assert!(strings.contains(&expected), "{expected}");
        }
        // The grants the guest is given are the projection's, not the credential's.
        let scopes = request.scopes();
        assert_eq!(scopes.hosts, ["api.example.test"]);
        assert!(
            scopes.prefixes.is_empty() && scopes.topics.is_empty() && scopes.targets.is_empty()
        );
    }

    #[test]
    fn what_the_guest_returns_becomes_the_legacy_handler_result() {
        let parsed = |output: &[u8]| {
            HandlerResult::from_output(output).map(|handler| serde_json::to_value(handler).unwrap())
        };
        // A handler result is used as it is.
        assert_eq!(
            parsed(br#"{"statusCode":201,"headers":{"x":"y"},"body":{"a":1}}"#),
            Some(json!({"statusCode": 201, "headers": {"x": "y"}, "body": {"a": 1}}))
        );
        assert_eq!(
            parsed(br#"{"statusCode":304}"#),
            Some(json!({"statusCode": 304}))
        );
        // Any other JSON is the action's result: the body of a 200.
        assert_eq!(
            parsed(br#"{"a":1}"#),
            Some(json!({"statusCode": 200, "body": {"a": 1}}))
        );
        assert_eq!(
            parsed(b"[1,2]"),
            Some(json!({"statusCode": 200, "body": [1, 2]}))
        );
        assert_eq!(parsed(b"null"), Some(json!({"statusCode": 200})));
        // Not JSON, or a handler result that breaks its own rules, is not a result.
        for bad in [
            &b"not json"[..],
            br#"{"statusCode":99}"#,
            br#"{"statusCode":600}"#,
            br#"{"statusCode":304,"body":"x"}"#,
            br#"{"statusCode":200,"body":null}"#,
            br#"{"statusCode":200,"extra":1}"#,
            br#"{"statusCode":"200"}"#,
        ] {
            assert_eq!(parsed(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }
}
