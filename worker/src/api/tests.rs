//! Smoke tests of every entry point. The conformance suite is `execution_api_contract`;
//! these keep the module exercised inside the worker binary's own test build.
use super::{
    error::{ParseError, Violation},
    failure::{FailureClass, FailureCode},
    request::{RuntimeMode, parse_request},
    result::{admit_wasm_attempt, parse_result},
    text::Name,
    update::{parse_cancel_report, parse_cancel_request, parse_update},
};
use serde_json::{Value, json};

fn request(mode: &str, base: Option<Value>) -> Value {
    let grant = json!({"family": "http", "scope": {"hosts": ["a.example.test"]}});
    let digest = format!("sha256:{}", "ab".repeat(32));
    let mut projection = json!({
        "revision": "rev-1",
        "runtime_mode": mode,
        "artifact_digest": digest,
        "component_digest": digest,
        "language_profile": {"name": "javascript", "version": "1.0.0"},
        "engine_version": "49.0.5",
        "wasi_version": "0.2.0",
        "interface_version": "0.2.0",
        "imports": ["falcone:host/http@0.2.0"],
        "budgets": {"fuel": 1, "memory_bytes": 2, "timeout_ms": 3, "max_host_calls": 4},
        "capabilities": [grant],
    });
    if let Some(base) = base {
        projection["base"] = base;
    }
    json!({
        "activation_id": "act-1",
        "attempt_id": "attempt-1",
        "attempt_number": 1,
        "deadline_ms": 10,
        "invocation": {"invocation-key": "k", "attempt-id": "attempt-1", "idempotency-key": "i"},
        "credential": {"claims": {
            "iss": "i", "aud": "a", "tenant": "t", "workspace": "w", "pool_profile": "p",
            "revision": "rev-1", "activation": "act-1", "attempt": "attempt-1",
            "iat": 1, "exp": 2, "allowed_capabilities": [grant],
        }},
        "projection": projection,
        "params": {},
    })
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn catalog() -> Value {
    let digest = format!("sha256:{}", "cd".repeat(32));
    json!({"kind": "catalog", "id": "js-base", "version": "1.2.3", "digest": digest})
}

fn byo() -> Value {
    let digest = format!("sha256:{}", "ef".repeat(32));
    json!({"kind": "byo", "image_digest": digest, "signer_identity": "s", "attestation_ref": "a"})
}

#[test]
fn requests_results_and_refusals_pass_through_the_entry_points() {
    let engine = Name::new("49.0.5").unwrap();
    for base in [None, Some(catalog()), Some(byo())] {
        let value = request("wasm", base.clone());
        let parsed = parse_request(&bytes(&value)).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
        assert_eq!(parsed.projection.runtime_mode, RuntimeMode::Wasm);
        assert!(admit_wasm_attempt(&parsed, &engine).is_ok());
        let mut result = json!({
            "activation_id": "act-1", "attempt_id": "attempt-1", "status": "success",
            "result": {"statusCode": 200}, "logs": ["line"],
            "identity": {
                "function_revision": "rev-1", "runtime_mode": "wasm",
                "artifact_digest": value["projection"]["artifact_digest"],
                "wasmtime_version": "49.0.5",
                "language_profile": {"name": "javascript", "version": "1.0.0"},
                "interface_version": "0.2.0",
            },
        });
        match base {
            None => assert!(parse_result(&parsed, &bytes(&result)).is_ok()),
            Some(base) => {
                let missing = parse_result(&parsed, &bytes(&result)).unwrap_err();
                assert_eq!(missing, ParseError::Invalid(Violation::BaseDigestMissing));
                let digest = base.get("digest").or(base.get("image_digest")).unwrap();
                result["identity"]["base_digest"] = digest.clone();
                assert!(parse_result(&parsed, &bytes(&result)).is_ok());
            }
        }
    }
    let oci = parse_request(&bytes(&request("oci", None))).unwrap();
    let refusal = admit_wasm_attempt(&oci, &engine).unwrap_err();
    assert_eq!(refusal.code(), FailureCode::RuntimeModeUnsupported);
    assert_eq!(refusal.class(), FailureClass::NonRetryable);
    assert_eq!(
        refusal.to_string(),
        "attempt refused: runtime_mode_unsupported (non_retryable), \
         activation_id=<redacted> attempt_id=<redacted> revision=<redacted>"
    );
    let serialized = serde_json::to_value(refusal.result()).unwrap();
    assert_eq!(serialized["status"], "error");
    parse_result(&oci, &bytes(&serialized)).unwrap();
    assert_eq!(FailureClass::ALL.len(), 3);
    assert!(
        FailureCode::ALL
            .iter()
            .all(|c| c.class(2) == FailureClass::NonRetryable)
    );
    assert_eq!(FailureCode::OciNotAuthorized.name(), "oci_not_authorized");
    assert_eq!(
        ParseError::Invalid(Violation::StatusShape).to_string(),
        "contract violation (StatusShape)"
    );
    assert!(parse_request(b"{").is_err());
}

#[test]
fn updates_and_cancel_pass_through_the_entry_points() {
    let ids = json!({"activation_id": "act-1", "attempt_id": "attempt-1"});
    for (kind, extra) in [
        ("heartbeat", json!({})),
        ("progress", json!({"percent": 5})),
        ("checkpoint", json!({"checkpoint_ref": "ref"})),
    ] {
        let mut value = ids.clone();
        value["kind"] = json!(kind);
        value["sequence"] = json!(1);
        for (key, member) in extra.as_object().unwrap() {
            value[key] = member.clone();
        }
        let update = parse_update(&bytes(&value)).unwrap();
        assert_eq!(serde_json::to_value(&update).unwrap(), value);
    }
    let request = parse_cancel_request(&bytes(&ids)).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), ids);
    let mut report = ids.clone();
    report["state"] = json!("uncertain");
    let parsed = parse_cancel_report(&bytes(&report)).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), report);
}
