//! Worker execution API v1 (`docs/contracts/worker-execution-api`): typed JSON messages and
//! their explicit validation. This module only parses and validates requests, updates,
//! cancellations and results. It runs no guest code, never evaluates JSON Schema, and is
//! allowed to use serde deserialization only because it reads JSON requests.
//!
//! Formatted output (`Debug`, `Display`) never carries a request-controlled string: free text
//! lives in `types::Name` or `types::Redacted`, and error types hold only static codes.

pub mod error;
pub mod grant;
pub mod legacy;
pub mod request;
pub mod result;
pub mod types;
pub mod update;

/// The only contract version this worker speaks.
pub const CONTRACT_VERSION: &str = "1";

/// Largest request, update, cancel or legacy message the parsers read: the 1 MiB parameter
/// limit plus envelope headroom.
pub const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

/// Largest result the parser reads. The contract allows 1000 log lines of 4096 characters, and
/// one character takes at most 12 JSON bytes (an escaped surrogate pair), so the logs alone can
/// reach 49152000 bytes. The rest of the result gets the `MAX_MESSAGE_BYTES` headroom.
pub const MAX_RESULT_BYTES: usize = 1000 * 4096 * 12 + MAX_MESSAGE_BYTES;

/// The vendored capability grants (credential claims) and the worker request grants differ.
pub const CREDENTIAL_GRANT_VERSION: &str = "0.1.0";
pub const REQUEST_GRANT_VERSION: &str = "0.2.0";

use error::Malformed;

fn from_bytes<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Malformed> {
    from_bytes_within(bytes, MAX_MESSAGE_BYTES)
}

fn from_bytes_within<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    limit: usize,
) -> Result<T, Malformed> {
    if bytes.len() > limit {
        return Err(Malformed::TooLarge);
    }
    // serde_json error text quotes the offending input, so only its category is kept.
    serde_json::from_slice(bytes).map_err(|error| match error.classify() {
        serde_json::error::Category::Data => Malformed::Schema,
        _ => Malformed::Syntax,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        error::{ApiError, ErrorBody, FailureCode, Malformed, Reason},
        legacy::{
            LegacyRejection, LegacyResponse, admit_legacy, parse_legacy_case, parse_legacy_invalid,
        },
        request::parse_request,
        result::{ExecutionResult, Identity, parse_result},
        types::{Presence, Redacted},
        update::{parse_cancel, parse_update},
    };
    use serde_json::{Value, json};

    fn request(mode: &str) -> Value {
        json!({
            "contract_version": "1",
            "activation_id": "act-1",
            "attempt_id": "attempt-1",
            "attempt_number": 2,
            "deadline": "2026-11-03T00:00:30Z",
            "credential": {
                "iss": "dispatcher", "aud": "worker", "tenant": "t", "workspace": "w",
                "pool_profile": "p", "revision": "rev-1", "activation": "act-1",
                "attempt": "attempt-1", "iat": 1, "exp": 2,
                "allowed_capabilities": [{
                    "family": "http", "interface": "falcone:host/http@0.1.0",
                    "scope": {"hosts": ["api.example.test"]}
                }]
            },
            "projection": {
                "revision": "rev-1",
                "runtime_mode": mode,
                "base": {"kind": "catalog", "id": "js", "version": "1.0.0",
                         "digest": format!("sha256:{}", "b".repeat(64))},
                "artifact_digest": format!("sha256:{}", "a".repeat(64)),
                "language_profile": {"name": "javascript", "version": "1"},
                "engine_version": "49.0.5",
                "wasi_version": "0.2.0",
                "interface_version": "0.2.0",
                "declared_imports": [],
                "budgets": {"fuel": 1, "memory_bytes": 1, "timeout_ms": 1, "max_host_calls": 1},
                "granted_capabilities": [{
                    "family": "http", "interface": "falcone:host/http@0.2.0",
                    "scope": {"hosts": ["api.example.test"]}
                }]
            },
            "params": {"__ow_method": "get", "__ow_path": "/v1/x"}
        })
    }

    fn bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }

    #[test]
    fn wasm_request_round_trips_through_a_success_and_every_failure_result() {
        let request = parse_request(&bytes(&request("wasm"))).unwrap();
        let success = ExecutionResult::success(
            &request,
            LegacyResponse {
                status_code: serde_json::from_value(json!(200)).unwrap(),
                headers: Presence::Absent,
                body: Presence::from(Some(json!("ok"))),
            },
            vec!["line".into()],
        );
        let parsed = parse_result(&bytes(&serde_json::to_value(&success).unwrap()), &request);
        assert_eq!(parsed.unwrap(), success);
        for code in FailureCode::ALL {
            for reason in code.reasons() {
                assert_eq!(reason.code(), code);
                let failure = ExecutionResult::failure(&request, *reason);
                let body = failure.error.value().unwrap().unwrap();
                assert_eq!(body, &ErrorBody::new(*reason));
                assert_eq!(body.status, code.status());
                assert!(!body.retryable);
                assert_eq!(
                    parse_result(&bytes(&serde_json::to_value(&failure).unwrap()), &request),
                    Ok(failure.clone())
                );
            }
        }
        assert_eq!(
            FailureCode::ALL.iter().filter(|c| c.worker_only()).count(),
            3
        );
        let identity = Identity::from_projection(&request.projection);
        assert_eq!(success.check_identity(&request.projection), Ok(()));
        assert_eq!(identity.function_revision.as_str(), "rev-1");
    }

    #[test]
    fn oci_request_is_refused_without_echoing_request_text() {
        let mut value = request("oci");
        value["activation_id"] = json!("act-sekret");
        value["credential"]["activation"] = json!("act-sekret");
        let Err(ApiError::Refused(refusal)) = parse_request(&bytes(&value)) else {
            panic!("expected a refusal");
        };
        assert_eq!(refusal.code(), FailureCode::RuntimeModeUnsupported);
        assert_eq!(refusal.reason(), Reason::RuntimeModeUnsupported);
        for text in [
            format!("{refusal:?}"),
            format!("{refusal}"),
            format!("{:?}", refusal.result()),
        ] {
            assert!(!text.contains("sekret"), "{text}");
        }
        let result = serde_json::to_value(refusal.result()).unwrap();
        assert_eq!(result["activation_id"], "act-sekret");
        assert_eq!(result["error"]["class"], "non_retryable");
        assert_eq!(Redacted::new(1).get(), &1);
    }

    #[test]
    fn malformed_messages_report_only_static_categories() {
        let mut value = request("wasm");
        value["projection"]["base"] = Value::Null;
        let Err(ApiError::Malformed(error)) = parse_request(&bytes(&value)) else {
            panic!("expected malformed");
        };
        assert_eq!(error, Malformed::Rule("null_not_absent"));
        assert_eq!(
            parse_request(b"{").unwrap_err().to_string(),
            "malformed execution API message: syntax"
        );
    }

    #[test]
    fn updates_cancel_and_legacy_records_parse() {
        let ids = json!({"contract_version": "1", "activation_id": "a", "attempt_id": "b"});
        let mut update = ids.clone();
        update["sequence"] = json!(1);
        update["kind"] = json!("heartbeat");
        assert!(parse_update(&bytes(&update)).is_ok());
        let mut cancel = ids;
        cancel["state"] = json!("requested");
        assert!(parse_cancel(&bytes(&cancel)).is_ok());
        assert!(parse_legacy_case(b"{}").is_err());
        assert!(parse_legacy_invalid(b"{}").is_err());
    }

    #[test]
    fn legacy_input_is_rejected_before_invocation() {
        let params = |value: Value| value.as_object().unwrap().clone();
        let missing = params(json!({"__ow_method": "get"}));
        let mut calls = 0;
        assert_eq!(
            admit_legacy(&missing, |_| calls += 1),
            Err(LegacyRejection::MissingPath)
        );
        assert_eq!(calls, 0);
        let request = parse_request(&bytes(&request("wasm"))).unwrap();
        let result = LegacyRejection::BodyTooLarge.result(&request);
        assert_eq!(result.error.value().unwrap().unwrap().status, 413);
        assert_eq!(
            LegacyRejection::NonJsonBody.error().code.as_str(),
            "invalid_request"
        );
        assert_eq!(Reason::NonJsonBody.code(), FailureCode::InvalidRequest);
    }
}
