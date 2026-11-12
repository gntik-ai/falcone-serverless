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

#[test]
fn scope_intersection_of_large_distinct_lists_is_linear() {
    // Disjoint-but-large lists: a quadratic membership scan needs billions of comparisons;
    // bounded set membership finishes in milliseconds. The bound is far above the linear cost.
    const ENTRIES: usize = 60_000;
    let names = |prefix: &str| -> Vec<String> {
        (0..ENTRIES).map(|n| format!("{prefix}-{n:08}")).collect()
    };
    let mut value = request("wasm", Some(catalog()));
    value["projection"]["grants"] = json!([{
        "family": "storage",
        "interface": "falcone:host/storage@0.2.0",
        "scope": {"prefixes": names("p")}
    }]);
    let mut shared = names("p");
    shared.reverse();
    shared.extend(names("q"));
    value["credential"]["allowed_capabilities"] = json!([{
        "family": "storage",
        "interface": "falcone:host/storage@0.1.0",
        "scope": {"prefixes": shared}
    }]);
    let request = parse_request(&bytes(&value)).unwrap();
    let started = std::time::Instant::now();
    let scopes = request.scopes();
    let elapsed = started.elapsed();
    assert_eq!(scopes.prefixes.len(), ENTRIES);
    assert_eq!(scopes.prefixes[0], "p-00000000");
    assert!(elapsed < std::time::Duration::from_secs(2), "{elapsed:?}");
}
