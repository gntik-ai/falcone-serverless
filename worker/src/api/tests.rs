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

// --- execution limits and the deadline ------------------------------------------------------

mod service {
    use super::*;
    use crate::limits::{Field, Limits};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const CEILINGS: WorkerCeilings = WorkerCeilings {
        max_memory_bytes: 64 * 1024 * 1024,
        max_wall_clock_ms: 20_000,
    };

    fn plan(edit: impl FnOnce(&mut Value)) -> Plan {
        let mut value = request("wasm", None);
        edit(&mut value);
        parse_request(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .plan()
    }

    fn budgets(fuel: u64, memory: u64, timeout: u64, calls: u64) -> impl FnOnce(&mut Value) {
        move |value| {
            value["projection"]["budgets"] = json!({
                "fuel": fuel, "memory_bytes": memory, "timeout_ms": timeout, "max_host_calls": calls
            });
        }
    }

    #[test]
    fn every_budget_field_reaches_the_limits_the_guest_runs_under() {
        let plan = plan(budgets(7_000, 8 * 1024 * 1024, 4_000, 3));
        let limits = plan.execution_limits(&CEILINGS).unwrap();
        assert_eq!(limits.fuel, 7_000);
        assert_eq!(limits.max_host_calls, 3);
        assert_eq!(
            limits.limits.value(Field::GuestMemoryBytes),
            8 * 1024 * 1024
        );
        // The fixed copy sizes stay the compatibility profile's.
        assert_eq!(
            limits.limits.value(Field::GuestToHostBytes),
            Limits::compatibility().value(Field::GuestToHostBytes)
        );
    }

    #[test]
    fn guest_memory_is_never_more_than_the_budget_the_worker_maximum_or_the_ceiling() {
        // At the worker maximum.
        let at_max = plan(budgets(1, 64 * 1024 * 1024, 1, 1));
        let limits = at_max.execution_limits(&CEILINGS).unwrap();
        assert_eq!(
            limits.limits.value(Field::GuestMemoryBytes),
            64 * 1024 * 1024
        );
        // A worker that allows more than the compatibility ceiling still stops at the ceiling.
        let big = WorkerCeilings {
            max_memory_bytes: 1 << 40,
            max_wall_clock_ms: 1,
        };
        let ceiling = Limits::ceiling_of(Field::GuestMemoryBytes);
        let at_ceiling = plan(budgets(1, ceiling, 1, 1));
        assert_eq!(
            at_ceiling
                .execution_limits(&big)
                .unwrap()
                .limits
                .value(Field::GuestMemoryBytes),
            ceiling
        );
        // One byte over either is refused, never replaced by a default.
        for (plan, ceilings) in [
            (plan(budgets(1, 64 * 1024 * 1024 + 1, 1, 1)), CEILINGS),
            (plan(budgets(1, ceiling + 1, 1, 1)), big),
        ] {
            let refusal = plan.execution_limits(&ceilings).err().unwrap();
            assert_eq!(refusal.code(), Code::InvalidRequest);
            assert_eq!(refusal.reason(), Reason::BudgetAboveWorkerCeiling);
        }
    }

    fn at(seconds: u64, millis: u32) -> SystemTime {
        UNIX_EPOCH + Duration::new(seconds, millis * 1_000_000)
    }

    fn with_deadline(deadline: &str, timeout_ms: u64) -> Plan {
        plan(|value| {
            value["deadline"] = deadline.into();
            value["projection"]["budgets"]["timeout_ms"] = timeout_ms.into();
        })
    }

    #[test]
    fn the_effective_timeout_is_the_shortest_of_request_projection_and_worker() {
        // 2026-11-11T12:00:30.250Z is 1_794_398_430.25 s after the epoch.
        let deadline = "2026-11-11T12:00:30.250Z";
        let now = at(1_794_398_400, 0);
        // Request: 30.25 s left; projection 60 s; worker 20 s -> the worker's 20 s.
        assert_eq!(
            with_deadline(deadline, 60_000).effective_timeout(now, &CEILINGS),
            Duration::from_millis(20_000)
        );
        // Request 30.25 s; projection 5 s; worker 20 s -> the projection's.
        assert_eq!(
            with_deadline(deadline, 5_000).effective_timeout(now, &CEILINGS),
            Duration::from_millis(5_000)
        );
        // Request 1.25 s left; projection 5 s; worker 20 s -> the request's.
        assert_eq!(
            with_deadline(deadline, 5_000).effective_timeout(at(1_794_398_429, 0), &CEILINGS),
            Duration::from_millis(1_250)
        );
        // A deadline already passed leaves nothing, so setup is never started.
        assert_eq!(
            with_deadline(deadline, 5_000).effective_timeout(at(1_794_398_431, 0), &CEILINGS),
            Duration::ZERO
        );
    }

    #[test]
    fn the_deadline_text_is_read_to_the_millisecond_and_to_the_nanosecond() {
        let plan = with_deadline("2026-11-11T12:00:30.250Z", 1);
        assert_eq!(plan.request_deadline, at(1_794_398_430, 250));
        let plan = with_deadline("1970-01-01T00:00:00Z", 1);
        assert_eq!(plan.request_deadline, UNIX_EPOCH);
        let plan = with_deadline("2000-02-29T23:59:59.123456789Z", 1);
        assert_eq!(
            plan.request_deadline,
            UNIX_EPOCH + Duration::new(951_868_799, 123_456_789)
        );
    }

    #[test]
    fn the_plan_carries_the_credential_strings_for_redaction_and_no_grant_secret_to_the_guest() {
        let plan = plan(|_| {});
        assert!(plan.secrets.iter().any(|secret| secret == "tenant-unit"));
        assert!(
            plan.secrets
                .iter()
                .any(|secret| secret == "api.example.test")
        );
        assert!(plan.secrets.iter().all(|secret| !secret.is_empty()));
        // The guest-visible context carries the verified tenant scope and the invocation
        // identity, never a credential claim such as the issuer.
        assert_eq!(plan.context.tenant, "tenant-unit");
        assert!(!format!("{:?}", plan.context).contains("issuer-unit"));
    }
}
