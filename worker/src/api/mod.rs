//! ctr-worker-execution-api v1 (docs/contracts/worker-execution-api/execution-api-v1.md).
//!
//! JSON request and result parsing only: nothing here loads, deserializes or executes a guest
//! artifact. No formatted error text, log line or message contains a request field; see
//! `redact`. Every rule is compiled in all builds.

pub mod codes;
pub mod error;
pub mod legacy;
pub mod redact;
pub mod request;
pub mod result;
pub mod update;

use error::{ApiError, MalformedReason};
use request::ExecuteAttempt;
use result::{Refusal, RequestedMode};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub const CONTRACT_VERSION: u32 = 1;

/// Serde error text echoes input values, so only its category survives.
pub(crate) fn parse_json<T: DeserializeOwned>(input: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(input).map_err(|error| {
        ApiError::Malformed(match error.classify() {
            serde_json::error::Category::Data => MalformedReason::Schema,
            _ => MalformedReason::Json,
        })
    })
}

/// Admits one execute attempt on the Wasm worker.
///
/// A `runtime_mode` other than `wasm` is refused before any other projection field is read.
/// A missing or non-string `runtime_mode` is malformed, as are unresolved base aliases and
/// unknown base kinds. The returned attempt has passed every v1 rule.
pub fn admit_wasm_attempt(input: &[u8]) -> Result<ExecuteAttempt, ApiError> {
    let document: Value = parse_json(input)?;
    let projection = document.get("projection");
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_owned);
    let (Some(activation_id), Some(attempt_id), Some(revision), Some(mode)) = (
        text(document.get("activation_id")),
        text(document.get("attempt_id")),
        text(projection.and_then(|p| p.get("revision"))),
        text(projection.and_then(|p| p.get("runtime_mode"))),
    ) else {
        return Err(ApiError::Malformed(MalformedReason::Schema));
    };
    if mode != "wasm" {
        let requested = if mode == "oci" {
            RequestedMode::Oci
        } else {
            RequestedMode::Unrecognized
        };
        return Err(ApiError::Refused(Refusal::new(
            activation_id,
            attempt_id,
            revision,
            requested,
        )));
    }
    let attempt: ExecuteAttempt = serde_json::from_value(document)
        .map_err(|_| ApiError::Malformed(MalformedReason::Schema))?;
    attempt.check()?;
    Ok(attempt)
}

#[cfg(test)]
mod tests {
    use super::{
        admit_wasm_attempt,
        codes::{FailureClass, FailureCode},
        error::{ApiError, MalformedReason, Mismatch, RequestReason},
        redact::MARKER,
        result::parse_result,
        update::{parse_cancel, parse_update},
    };
    use serde_json::{Value, json};

    const SENTINEL: &str = "SENTINEL-credential-7b1d";

    fn digest(c: &str) -> String {
        format!("sha256:{}", c.repeat(64))
    }

    fn request(mode: &str, id: &str) -> Value {
        json!({
            "contract_version": 1,
            "activation_id": id,
            "attempt_id": "attempt-1",
            "deadline_unix_ms": 1,
            "execution_credential": {
                "iss": "i", "aud": "a", "tenant": "t", "workspace": "w", "pool_profile": "p",
                "revision": "rev-1", "activation": id, "attempt": "attempt-1",
                "iat": 0, "exp": 1, "allowed_capabilities": [{"capability": "logging"}]
            },
            "projection": {
                "revision": "rev-1", "runtime_mode": mode,
                "base": {"kind": "catalog", "id": "base@1.0.0", "digest": digest("c")},
                "artifact_digest": digest("a"), "component_digest": digest("b"),
                "language_profile": "javascript", "engine_version": "1.0.0",
                "wasi_version": "0.2.3", "interface_version": "falcone:host@0.2.0",
                "imports": ["logging"],
                "budgets": {
                    "guest_memory_bytes": 1, "guest_to_host_bytes": 1, "host_to_guest_bytes": 1,
                    "guest_deadline_ms": 1, "host_call_deadline_ms": 1, "termination_budget_ms": 1
                },
                "granted_capabilities": ["logging"]
            },
            "invocation": {
                "headers": {"X-Correlation-Id": "corr-1", "Idempotency-Key": "idem-1"},
                "identity": {
                    "invocation-key": "inv-1", "attempt-id": "attempt-1", "idempotency-key": "idem-1"
                }
            },
            "params": {"__ow_path": "/v1/workspaces/ws-1/openapi", "name": "x"}
        })
    }

    fn bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }

    #[test]
    fn admits_wasm_and_checks_results_updates_and_cancel() {
        let attempt = admit_wasm_attempt(&bytes(&request("wasm", "act-1"))).unwrap();
        let identity = json!({
            "revision": "rev-1", "runtime_mode": "wasm", "artifact_digest": digest("a"),
            "base_digest": digest("c"), "wasmtime_version": "1.0.0",
            "language_profile": "javascript", "interface_version": "falcone:host@0.2.0"
        });
        let success = json!({
            "activation_id": "act-1", "attempt_id": "attempt-1", "status": "success",
            "result": {"statusCode": 200, "body": "{}"}, "logs": ["line"], "identity": identity
        });
        assert_eq!(attempt.projection.artifact_digest.as_str(), digest("a"));
        let parsed = parse_result(&attempt, &bytes(&success)).unwrap();
        assert!(parsed.check_identity(&attempt).is_ok());
        let mut missing = success.clone();
        missing["identity"]
            .as_object_mut()
            .unwrap()
            .remove("base_digest");
        assert_eq!(
            parse_result(&attempt, &bytes(&missing)).unwrap_err(),
            ApiError::IdentityMismatch(Mismatch::BaseDigestMissing)
        );
        for update in [
            json!({"kind": "heartbeat", "activation_id": "act-1", "attempt_id": "attempt-1", "sequence": 1}),
            json!({"kind": "progress", "activation_id": "act-1", "attempt_id": "attempt-1", "sequence": 2, "permille": 5}),
            json!({"kind": "checkpoint", "activation_id": "act-1", "attempt_id": "attempt-1", "sequence": 3, "checkpoint_ref": "ref"}),
        ] {
            assert!(parse_update(&attempt, &bytes(&update)).is_ok());
        }
        let cancel =
            json!({"activation_id": "act-1", "attempt_id": "attempt-1", "state": "uncertain"});
        assert!(parse_cancel(&attempt, &bytes(&cancel)).is_ok());
    }

    #[test]
    fn refuses_other_modes_and_never_prints_request_fields() {
        let error = admit_wasm_attempt(&bytes(&request("oci", SENTINEL))).unwrap_err();
        assert_eq!(error.code(), Some(FailureCode::RuntimeModeNotWasm));
        for text in [format!("{error:?}"), error.to_string(), error.log_line()] {
            assert!(!text.contains(SENTINEL), "{text}");
        }
        assert!(format!("{error:?}").contains(MARKER));
        let ApiError::Refused(refusal) = &error else {
            panic!("expected a refusal");
        };
        assert!(serde_json::to_string(refusal).unwrap().contains(SENTINEL));
    }

    #[test]
    fn every_error_and_code_has_fixed_text() {
        let mut alias = request("wasm", "act-1");
        alias["projection"]["base"]["id"] = json!("default");
        let mut body = request("wasm", "act-1");
        body["params"]["__ow_body"] = json!("x");
        body["params"]["__ow_headers"] = json!({"Content-Type": "application/json"});
        let errors = [
            admit_wasm_attempt(b"{").unwrap_err(),
            admit_wasm_attempt(&bytes(&alias)).unwrap_err(),
            admit_wasm_attempt(&bytes(&body)).unwrap_err(),
        ];
        assert_eq!(errors[0], ApiError::Malformed(MalformedReason::Json));
        assert_eq!(errors[2], ApiError::Rejected(RequestReason::BodyNotJson));
        for error in errors {
            assert_eq!(error.code(), Some(FailureCode::InvalidRequest));
            assert!(error.log_line().contains("non_retryable"));
        }
        assert_eq!(MalformedReason::BaseAlias.as_str(), "base_alias");
        assert_eq!(RequestReason::BodyTooLarge.label(), "body-too-large");
        assert_eq!(
            Mismatch::BaseDigestUnexpected.as_str(),
            "base_digest_unexpected"
        );
        assert!(
            ApiError::IdentityMismatch(Mismatch::Revision)
                .code()
                .is_none()
        );
        assert_eq!(FailureCode::ALL.len(), 7);
        assert_eq!(
            FailureCode::BaseDisabled.class(),
            FailureClass::NonRetryable
        );
        assert_eq!(
            FailureCode::BaseDisabled.gateway_code(),
            Some("GW_BASE_DISABLED")
        );
    }
}
