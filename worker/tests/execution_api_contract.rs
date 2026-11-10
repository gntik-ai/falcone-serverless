//! ctr-worker-execution-api v1 conformance. Parses fixtures through the worker types; it
//! executes no guest code.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

#[path = "../src/api/mod.rs"]
mod api;

use api::{
    admit_wasm_attempt,
    codes::{FailureClass, FailureCode},
    error::{ApiError, MalformedReason, Mismatch, RequestReason},
    legacy::{Invocation, LegacyParams, LegacyResult, MAX_BODY_BYTES},
    redact::{MARKER, Redacted},
    request::{ExecuteAttempt, RuntimeMode},
    result::{ExecutionResult, Refusal, RequestedMode, Status, parse_result},
    update::{Cancel, CancelState, Update, parse_cancel, parse_update},
};

const SENTINEL: &str = "SENTINEL-credential-4f9c2e71";

const LEGACY: [(&str, &str); 11] = [
    (
        "valid/401.json",
        "3ed82379573d4d0edf7a1b9a9d4b3cce47640a29bed85df3397ff8ba4e4854ff",
    ),
    (
        "valid/400.json",
        "0f09bdb42b0ce1ba49ab3e950ea07136491dfaaaac7a65065b25659a7c16b949",
    ),
    (
        "valid/429.json",
        "27ed6860e27beb0dafb7a144948644c5b6c227f1c7c1f74220fd992b80d0d6c5",
    ),
    (
        "valid/404.json",
        "33bdfb0ccf1d60571f69a9a8e58a6f0abf96629c1f1c5e419cf14b399a80f63a",
    ),
    (
        "valid/403.json",
        "fdcb8c79b726564c3e5433ab1eb851700dcd234cd32db908840fa795605e5ff8",
    ),
    (
        "valid/304.json",
        "132ae45adf2c663d4ce7ea9bb75ee5e5e56ea5ca76e39a54363419fdf43d9d1b",
    ),
    (
        "valid/200-json.json",
        "0fd272cd853f40cad7b3e1886b1c39c0e0004f4ae146456a95b8ee6244160127",
    ),
    (
        "valid/200-yaml.json",
        "e5afd3a7fd5f6dd1b4c90f58fb3ba40e6abbb2208f1edbafa30031438e5ed7db",
    ),
    (
        "invalid/missing-path.json",
        "b61cf55fe179fb4174c58d40c3808d4b461039051d5b99bbdbaa4ebc15e2037a",
    ),
    (
        "invalid/oversized-body.json",
        "c98ab3c441140257401123d5e32c3b121a4825eb5b1392552a05d7c9be4879c4",
    ),
    (
        "invalid/non-JSON-body.json",
        "bb28023f7e5605b798847b5aedf913598fc0b2edace94997dfdd8eec1d9227da",
    ),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn contract(name: &str) -> Vec<u8> {
    fs::read(root().join("worker/tests/fixtures/contract").join(name)).unwrap()
}

fn contract_value(name: &str) -> Value {
    serde_json::from_slice(&contract(name)).unwrap()
}

fn legacy_bytes(name: &str) -> Vec<u8> {
    fs::read(
        root()
            .join("docs/contracts/fixtures/legacy-envelope")
            .join(name),
    )
    .unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn admit(value: &Value) -> Result<ExecuteAttempt, ApiError> {
    admit_wasm_attempt(&bytes(value))
}

fn attempt(name: &str) -> ExecuteAttempt {
    admit_wasm_attempt(&contract(name)).unwrap()
}

/// Rewrites the attempt identifiers everywhere a request names them.
fn with_attempt_id(mut request: Value, id: &str) -> Value {
    for pointer in [
        "/attempt_id",
        "/execution_credential/attempt",
        "/invocation/identity/attempt-id",
    ] {
        *request.pointer_mut(pointer).unwrap() = json!(id);
    }
    request
}

fn result_for(name: &str, id: &str) -> Vec<u8> {
    let mut result = contract_value(name);
    result["attempt_id"] = json!(id);
    bytes(&result)
}

#[test]
fn non_wasm_runtime_mode_is_refused_non_retryable() {
    let error = admit_wasm_attempt(&contract("execute-attempt-oci.json")).unwrap_err();
    assert_eq!(error.code(), Some(FailureCode::RuntimeModeNotWasm));
    let ApiError::Refused(refusal) = &error else {
        panic!("expected a refusal");
    };
    assert_eq!(refusal.failure.class, FailureClass::NonRetryable);
    assert_eq!(refusal.status, Status::Failure);
    assert_eq!(refusal.requested_runtime_mode, RequestedMode::Oci);
    // The serialized refusal keeps its contract fields.
    assert_eq!(
        serde_json::to_value(refusal).unwrap(),
        contract_value("refusal-oci.json")
    );
    // Any other value is refused too, and never echoed.
    let mut other = contract_value("execute-attempt-absent-base.json");
    other["projection"]["runtime_mode"] = json!("native");
    let ApiError::Refused(refusal) = admit(&other).unwrap_err() else {
        panic!("expected a refusal");
    };
    assert_eq!(refusal.requested_runtime_mode, RequestedMode::Unrecognized);
    assert_eq!(
        serde_json::to_value(&refusal).unwrap()["requested_runtime_mode"],
        "unrecognized"
    );
    assert_eq!(
        Refusal::new("a".into(), "b".into(), "c".into(), RequestedMode::Oci).failure,
        refusal.failure
    );
}

#[test]
fn runtime_mode_is_required_and_a_string() {
    assert_eq!(
        admit_wasm_attempt(&contract(
            "execute-attempt-malformed-missing-runtime-mode.json"
        ))
        .unwrap_err(),
        ApiError::Malformed(MalformedReason::Schema)
    );
    for mode in [json!(null), json!(7), json!(["wasm"])] {
        let mut request = contract_value("execute-attempt-absent-base.json");
        request["projection"]["runtime_mode"] = mode;
        assert_eq!(
            admit(&request).unwrap_err(),
            ApiError::Malformed(MalformedReason::Schema)
        );
    }
    assert_eq!(
        admit_wasm_attempt(b"not json").unwrap_err(),
        ApiError::Malformed(MalformedReason::Json)
    );
}

#[test]
fn base_variants_round_trip() {
    for name in [
        "execute-attempt-catalog.json",
        "execute-attempt-byo.json",
        "execute-attempt-absent-base.json",
    ] {
        let original = contract_value(name);
        let admitted = admit_wasm_attempt(&contract(name)).unwrap();
        assert_eq!(admitted.projection.runtime_mode, RuntimeMode::Wasm);
        assert_eq!(serde_json::to_value(&admitted).unwrap(), original, "{name}");
    }
    assert!(
        attempt("execute-attempt-absent-base.json")
            .projection
            .base
            .is_none()
    );
    let catalog = attempt("execute-attempt-catalog.json");
    let byo = attempt("execute-attempt-byo.json");
    assert_eq!(
        catalog.projection.base.as_ref().unwrap().digest().as_str(),
        format!("sha256:{}", "c".repeat(64))
    );
    assert_eq!(
        byo.projection.base.as_ref().unwrap().digest().as_str(),
        format!("sha256:{}", "d".repeat(64))
    );
}

#[test]
fn unresolved_alias_unknown_kind_and_bad_bases_are_malformed() {
    assert_eq!(
        admit_wasm_attempt(&contract("execute-attempt-malformed-base-alias.json")).unwrap_err(),
        ApiError::Malformed(MalformedReason::BaseAlias)
    );
    assert_eq!(
        admit_wasm_attempt(&contract("execute-attempt-malformed-base-kind.json")).unwrap_err(),
        ApiError::Malformed(MalformedReason::Schema)
    );
    let digest = format!("sha256:{}", "c".repeat(64));
    for base in [
        json!("default"),
        json!(null),
        json!({"kind": "catalog", "id": "python-base@latest", "digest": digest}),
        json!({"kind": "catalog", "id": "@1.0.0", "digest": digest}),
        json!({"kind": "catalog", "id": "python-base@1.0.0"}),
        json!({"kind": "catalog", "id": "python-base@1.0.0", "digest": "sha256:ABC"}),
        json!({"kind": "byo", "image_digest": digest, "signer_identity": "", "attestation_ref": "a"}),
        json!({"kind": "byo", "image_digest": digest, "signer_identity": "s"}),
        json!({"kind": "catalog", "id": "python-base@1.0.0", "digest": digest, "extra": 1}),
    ] {
        let mut request = contract_value("execute-attempt-absent-base.json");
        request["projection"]["base"] = base.clone();
        assert!(
            matches!(admit(&request), Err(ApiError::Malformed(_))),
            "{base}"
        );
    }
}

#[test]
fn malformed_requests_are_rejected() {
    let base = contract_value("execute-attempt-catalog.json");
    for (pointer, value) in [
        ("/contract_version", json!(2)),
        ("/activation_id", json!("")),
        ("/deadline_unix_ms", json!(0)),
        ("/projection/budgets/guest_deadline_ms", json!(0)),
        ("/projection/imports", json!([""])),
        ("/execution_credential/iat", json!(-1)),
        ("/execution_credential/tenant", json!("")),
    ] {
        let mut request = base.clone();
        *request.pointer_mut(pointer).unwrap() = value;
        assert!(
            matches!(admit(&request), Err(ApiError::Malformed(_))),
            "{pointer}"
        );
    }
    for pointer in [
        "/execution_credential",
        "/projection/artifact_digest",
        "/invocation",
        "/params",
    ] {
        let mut request = base.clone();
        request.as_object_mut().unwrap(); // object root
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        request
            .pointer_mut(if parent.is_empty() { "" } else { parent })
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert_eq!(
            admit(&request).unwrap_err(),
            ApiError::Malformed(MalformedReason::Schema),
            "{pointer}"
        );
    }
    let mut unknown = base.clone();
    unknown["unexpected"] = json!(true);
    assert!(matches!(admit(&unknown), Err(ApiError::Malformed(_))));
    // The credential is structured claims, not an opaque string.
    let mut opaque = base;
    opaque["execution_credential"] = json!("opaque-token");
    assert!(matches!(admit(&opaque), Err(ApiError::Malformed(_))));
}

#[test]
fn credential_and_invocation_bind_to_the_attempt() {
    let base = contract_value("execute-attempt-catalog.json");
    for (pointer, field) in [
        (
            "/execution_credential/activation",
            Mismatch::CredentialActivation,
        ),
        ("/execution_credential/attempt", Mismatch::CredentialAttempt),
        (
            "/execution_credential/revision",
            Mismatch::CredentialRevision,
        ),
        (
            "/invocation/identity/attempt-id",
            Mismatch::InvocationAttempt,
        ),
        (
            "/invocation/identity/idempotency-key",
            Mismatch::IdempotencyKey,
        ),
    ] {
        let mut request = base.clone();
        *request.pointer_mut(pointer).unwrap() = json!("other");
        assert_eq!(
            admit(&request).unwrap_err(),
            ApiError::IdentityMismatch(field)
        );
    }
}

#[test]
fn failure_codes_agree_across_types_fixtures_and_spec() {
    let table = contract_value("failure-codes.json");
    let rows = table.as_array().unwrap();
    assert_eq!(rows.len(), FailureCode::ALL.len());
    let spec =
        fs::read_to_string(root().join("docs/contracts/worker-execution-api/execution-api-v1.md"))
            .unwrap();
    for (code, row) in FailureCode::ALL.iter().zip(rows) {
        assert_eq!(row["code"], code.as_str());
        assert_eq!(row["class"], code.class().as_str());
        assert_eq!(row["gateway_code"], json!(code.gateway_code()));
        assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
        assert_eq!(
            serde_json::to_value(code.class()).unwrap(),
            code.class().as_str()
        );
        // The spec lists the code with its family, class, gateway code and pinned fixture.
        let line = format!(
            "| {} | {} | {} | {} | {} |",
            code.as_str(),
            row["family"].as_str().unwrap(),
            code.class().as_str(),
            code.gateway_code().unwrap_or("worker-only"),
            row["pinned_fixture"].as_str().unwrap_or("none"),
        );
        assert!(spec.contains(&line), "{line}");
    }
    assert_eq!(
        FailureCode::ALL.map(FailureCode::class),
        [
            FailureClass::NonRetryable,
            FailureClass::NonRetryable,
            FailureClass::NonRetryable,
            FailureClass::NonRetryable,
            FailureClass::NonRetryable,
            FailureClass::Retryable,
            FailureClass::Uncertain,
        ]
    );
}

#[test]
fn oci_and_base_codes_match_the_pinned_functions_api_fixtures() {
    // Literal values from the operator note (falcone a19f40ab): the openapi failureReason
    // enum and the sha256 of each error fixture.
    let pinned = [
        (
            "oci_not_authorized",
            "GW_OCI_NOT_AUTHORIZED",
            403,
            "ef0763c3f590a7e5fb31b59073e240d5320681b0f70f13757327cdb7bb26d740",
        ),
        (
            "base_disabled",
            "GW_BASE_DISABLED",
            409,
            "9ed46a38dca56af8f9b9fdc16a413fec45f48a4b1324de2d9a400ed31c49c7d8",
        ),
        (
            "base_mismatch",
            "GW_BASE_MISMATCH",
            409,
            "2ac0fa4c05c628d6a251657de9918564652d32af9473502de510eaedfdcfa92c",
        ),
    ];
    let sources = contract_value("pinned-sources.json");
    assert_eq!(
        sources["revision"],
        "a19f40ab9e160cd522c110efd50c30b2f5e0d708"
    );
    assert_eq!(
        sources["openapi"]["sha256"],
        "03121286b0c7ec716304d51a41b866477f4fc21671d95ebeafb8bf9da70e8960"
    );
    assert_eq!(
        sources["execution_credential_claims_schema"]["sha256"],
        "9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0"
    );
    let copies = sources["error_fixtures"].as_array().unwrap();
    assert_eq!(copies.len(), pinned.len());
    for (name, gateway, status, digest) in pinned {
        let copy = copies
            .iter()
            .find(|c| {
                c["path"] == format!("tests/contracts/functions-api/fixtures/errors/{name}.json")
            })
            .unwrap();
        assert_eq!(copy["sha256"], digest);
        let fixture = contract(copy["copy"].as_str().unwrap());
        assert_eq!(sha256(&fixture), digest, "{name}");
        let error: Value = serde_json::from_slice(&fixture).unwrap();
        let code: FailureCode = serde_json::from_value(error["detail"]["reason"].clone()).unwrap();
        assert_eq!(code.as_str(), name);
        assert_eq!(code.gateway_code(), Some(gateway));
        assert_eq!(error["code"], gateway);
        assert_eq!(error["status"], status);
        assert_eq!(error["retryable"], code.class() == FailureClass::Retryable);
    }
    // Every code with a gateway name is one of the three; the only OCI/base code that is
    // worker-only is the non-wasm refusal.
    let table = contract_value("failure-codes.json");
    let worker_only: Vec<_> = table
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["family"] == "oci_base" && row["gateway_code"].is_null())
        .map(|row| row["code"].as_str().unwrap())
        .collect();
    assert_eq!(worker_only, ["runtime_mode_not_wasm"]);
    let referenced: Vec<_> = FailureCode::ALL
        .iter()
        .filter(|c| c.gateway_code().is_some())
        .map(|c| c.as_str())
        .collect();
    assert_eq!(referenced, pinned.map(|p| p.0));
}

#[test]
fn a_tampered_pinned_fixture_fails_its_sha256() {
    let sources = contract_value("pinned-sources.json");
    for copy in sources["error_fixtures"].as_array().unwrap() {
        let mut tampered = contract(copy["copy"].as_str().unwrap());
        tampered.insert(tampered.len() - 1, b' ');
        assert_ne!(sha256(&tampered), copy["sha256"].as_str().unwrap());
    }
}

#[test]
fn non_retryable_codes_stay_non_retryable_on_retries() {
    let base = contract_value("execute-attempt-catalog.json");
    for code in ["oci-not-authorized", "base-disabled"] {
        for retry in 1..=4 {
            let id = format!("attempt-synthetic-retry-{retry}");
            let request = admit(&with_attempt_id(base.clone(), &id)).unwrap();
            let name = format!("result-failure-{code}.json");
            let parsed = parse_result(&request, &result_for(&name, &id)).unwrap();
            let failure = parsed.failure.unwrap();
            assert_eq!(
                failure.class,
                FailureClass::NonRetryable,
                "{code} retry {retry}"
            );
            let mut forged = contract_value(&name);
            forged["attempt_id"] = json!(id);
            forged["failure"]["class"] = json!("retryable");
            assert_eq!(
                parse_result(&request, &bytes(&forged)).unwrap_err(),
                ApiError::IdentityMismatch(Mismatch::FailureClass)
            );
        }
    }
}

#[test]
fn every_failure_code_result_parses_with_its_class() {
    let request = attempt("execute-attempt-catalog.json");
    for code in FailureCode::ALL {
        if code == FailureCode::RuntimeModeNotWasm {
            continue;
        }
        let name = format!("result-failure-{}.json", code.as_str().replace('_', "-"));
        let parsed = parse_result(&request, &contract(&name)).unwrap();
        let failure = parsed.failure.unwrap();
        assert_eq!(
            (failure.code, failure.class),
            (code, code.class()),
            "{name}"
        );
        assert_eq!(parsed.status, Status::Failure);
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            contract_value(&name)
        );
    }
}

#[test]
fn results_require_and_check_the_identity_block() {
    for (request, result) in [
        (
            "execute-attempt-absent-base.json",
            "result-absent-base.json",
        ),
        ("execute-attempt-catalog.json", "result-catalog.json"),
        ("execute-attempt-byo.json", "result-byo.json"),
    ] {
        let parsed = parse_result(&attempt(request), &contract(result)).unwrap();
        assert_eq!(parsed.status, Status::Success);
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            contract_value(result)
        );
        parsed.check_identity(&attempt(request)).unwrap();
    }
    let catalog = attempt("execute-attempt-catalog.json");
    let absent = attempt("execute-attempt-absent-base.json");
    // Both inverse base_digest mutations are rejected through the public entry point.
    let mut missing = contract_value("result-catalog.json");
    missing["identity"]
        .as_object_mut()
        .unwrap()
        .remove("base_digest");
    assert_eq!(
        parse_result(&catalog, &bytes(&missing)).unwrap_err(),
        ApiError::IdentityMismatch(Mismatch::BaseDigestMissing)
    );
    let mut unexpected = contract_value("result-absent-base.json");
    unexpected["identity"]["base_digest"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert_eq!(
        parse_result(&absent, &bytes(&unexpected)).unwrap_err(),
        ApiError::IdentityMismatch(Mismatch::BaseDigestUnexpected)
    );
    // A byo result against the catalog attempt carries the wrong base digest.
    assert_eq!(
        parse_result(&catalog, &contract("result-byo.json")).unwrap_err(),
        ApiError::IdentityMismatch(Mismatch::BaseDigest)
    );
    let mut no_identity = contract_value("result-absent-base.json");
    no_identity.as_object_mut().unwrap().remove("identity");
    assert_eq!(
        parse_result(&absent, &bytes(&no_identity)).unwrap_err(),
        ApiError::Malformed(MalformedReason::Schema)
    );
    for (pointer, value, field) in [
        ("/activation_id", json!("other"), Mismatch::ActivationId),
        ("/attempt_id", json!("other"), Mismatch::AttemptId),
        ("/identity/revision", json!("other"), Mismatch::Revision),
        (
            "/identity/runtime_mode",
            json!("oci"),
            Mismatch::RuntimeMode,
        ),
        (
            "/identity/artifact_digest",
            json!(format!("sha256:{}", "e".repeat(64))),
            Mismatch::ArtifactDigest,
        ),
        (
            "/identity/wasmtime_version",
            json!("0.0.1"),
            Mismatch::WasmtimeVersion,
        ),
        (
            "/identity/language_profile",
            json!("python"),
            Mismatch::LanguageProfile,
        ),
        (
            "/identity/interface_version",
            json!("other"),
            Mismatch::InterfaceVersion,
        ),
    ] {
        let mut result = contract_value("result-absent-base.json");
        *result.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            parse_result(&absent, &bytes(&result)).unwrap_err(),
            ApiError::IdentityMismatch(field),
            "{pointer}"
        );
    }
    for edit in [
        |r: &mut Value| r["failure"] = json!({"code": "base_disabled", "class": "non_retryable"}),
        |r: &mut Value| r["status"] = json!("failure"),
        |r: &mut Value| {
            r.as_object_mut().unwrap().remove("result");
        },
        |r: &mut Value| r["result"]["statusCode"] = json!(99),
        |r: &mut Value| {
            r["result"]["statusCode"] = json!(304);
            r["result"]["body"] = json!("x");
        },
        |r: &mut Value| r["unexpected"] = json!(1),
    ] {
        let mut result = contract_value("result-absent-base.json");
        edit(&mut result);
        assert!(matches!(
            parse_result(&absent, &bytes(&result)),
            Err(ApiError::Malformed(_))
        ));
    }
}

#[test]
fn legacy_fixtures_are_pinned_byte_for_byte() {
    let inventory =
        fs::read_to_string(root().join("docs/contracts/falcone-contract-inventory.md")).unwrap();
    assert_eq!(
        LEGACY
            .iter()
            .filter(|(name, _)| name.starts_with("valid/"))
            .count(),
        8
    );
    assert_eq!(
        LEGACY
            .iter()
            .filter(|(name, _)| name.starts_with("invalid/"))
            .count(),
        3
    );
    for (name, digest) in LEGACY {
        assert_eq!(sha256(&legacy_bytes(name)), digest, "{name}");
        assert!(
            inventory.contains(&format!("legacy-envelope/{name} | {digest} |")),
            "{name}"
        );
    }
}

#[test]
fn valid_legacy_fixtures_round_trip_unchanged() {
    let template = contract_value("execute-attempt-absent-base.json");
    for (name, _) in LEGACY.iter().filter(|(name, _)| name.starts_with("valid/")) {
        let fixture: Value = serde_json::from_slice(&legacy_bytes(name)).unwrap();
        let invocation: Invocation = serde_json::from_value(fixture["invocation"].clone()).unwrap();
        let params: LegacyParams = serde_json::from_value(fixture["params"].clone()).unwrap();
        let expected: LegacyResult = serde_json::from_value(fixture["expected"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&invocation).unwrap(),
            fixture["invocation"],
            "{name}"
        );
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            fixture["params"],
            "{name}"
        );
        assert_eq!(
            serde_json::to_value(&expected).unwrap(),
            fixture["expected"],
            "{name}"
        );
        params.check().unwrap();
        expected.check().unwrap();
        // The fixture's own invocation and params are admitted and come back unchanged.
        let mut request = template.clone();
        request["invocation"] = fixture["invocation"].clone();
        request["params"] = fixture["params"].clone();
        let admitted = admit(&request).unwrap();
        assert_eq!(serde_json::to_value(&admitted).unwrap(), request, "{name}");
        // And a result that wraps the fixture's expected envelope parses unchanged.
        let mut result = contract_value("result-absent-base.json");
        result["result"] = fixture["expected"].clone();
        let parsed = parse_result(&admitted, &bytes(&result)).unwrap();
        assert_eq!(
            serde_json::to_value(&parsed).unwrap()["result"],
            fixture["expected"],
            "{name}"
        );
    }
}

#[test]
fn invalid_legacy_fixtures_are_rejected_before_invocation() {
    let template = contract_value("execute-attempt-absent-base.json");
    let expected = [
        ("invalid/missing-path.json", RequestReason::EnvelopeInvalid),
        ("invalid/oversized-body.json", RequestReason::BodyTooLarge),
        ("invalid/non-JSON-body.json", RequestReason::BodyNotJson),
    ];
    for (name, reason) in expected {
        let fixture: Value = serde_json::from_slice(&legacy_bytes(name)).unwrap();
        assert_eq!(fixture["expected"]["invoked"], false);
        assert_eq!(fixture["expected"]["rejection"], reason.label(), "{name}");
        let mut request = template.clone();
        request["invocation"] = fixture["invocation"].clone();
        request["params"] = fixture["params"].clone();
        let error = admit(&request).unwrap_err();
        assert_eq!(error, ApiError::Rejected(reason), "{name}");
        assert_eq!(error.code(), Some(FailureCode::InvalidRequest));
        assert_eq!(error.code().unwrap().class(), FailureClass::NonRetryable);
    }
    let oversized: Value =
        serde_json::from_slice(&legacy_bytes("invalid/oversized-body.json")).unwrap();
    assert_eq!(
        oversized["params"]["__ow_body"].as_str().unwrap().len(),
        MAX_BODY_BYTES + 1
    );
}

#[test]
fn updates_and_cancel_round_trip_and_bind_to_the_attempt() {
    let request = attempt("execute-attempt-absent-base.json");
    for name in [
        "update-heartbeat.json",
        "update-progress.json",
        "update-checkpoint.json",
    ] {
        let update = parse_update(&request, &contract(name)).unwrap();
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            contract_value(name),
            "{name}"
        );
    }
    assert!(matches!(
        parse_update(&request, &contract("update-progress.json")).unwrap(),
        Update::Progress { permille: 500, .. }
    ));
    for (name, state) in [
        ("cancel-requested.json", CancelState::Requested),
        ("cancel-acknowledged.json", CancelState::Acknowledged),
        ("cancel-uncertain.json", CancelState::Uncertain),
    ] {
        let cancel: Cancel = parse_cancel(&request, &contract(name)).unwrap();
        assert_eq!(cancel.state, state);
        assert_eq!(serde_json::to_value(&cancel).unwrap(), contract_value(name));
    }
    let mut other = contract_value("update-heartbeat.json");
    other["attempt_id"] = json!("other");
    assert_eq!(
        parse_update(&request, &bytes(&other)).unwrap_err(),
        ApiError::IdentityMismatch(Mismatch::AttemptId)
    );
    let mut over = contract_value("update-progress.json");
    over["permille"] = json!(1001);
    assert_eq!(
        parse_update(&request, &bytes(&over)).unwrap_err(),
        ApiError::Malformed(MalformedReason::Field)
    );
    let mut cancel = contract_value("cancel-requested.json");
    cancel["activation_id"] = json!("other");
    assert_eq!(
        parse_cancel(&request, &bytes(&cancel)).unwrap_err(),
        ApiError::IdentityMismatch(Mismatch::ActivationId)
    );
}

fn assert_no_sentinel(error: &ApiError) {
    for text in [
        format!("{error:?}"),
        format!("{error}"),
        format!("{error:#?}"),
        error.log_line(),
        format!("{:?}", error.code()),
    ] {
        assert!(!text.contains(SENTINEL), "{text}");
    }
}

#[test]
fn refusal_output_never_contains_request_fields() {
    for pointer in ["/activation_id", "/attempt_id", "/projection/revision"] {
        let mut request = contract_value("execute-attempt-oci.json");
        *request.pointer_mut(pointer).unwrap() = json!(SENTINEL);
        let error = admit(&request).unwrap_err();
        assert_no_sentinel(&error);
        let ApiError::Refused(refusal) = &error else {
            panic!("expected a refusal");
        };
        // Formatted text shows the marker; the serialized refusal keeps its contract fields.
        assert!(format!("{refusal:?}").contains(MARKER), "{pointer}");
        assert_eq!(format!("{}", refusal.activation_id), MARKER);
        assert!(
            serde_json::to_string(refusal).unwrap().contains(SENTINEL),
            "{pointer}"
        );
        // The refusal is a plain value: formatting every field and clones stays redacted.
        assert!(!format!("{:?}", refusal.clone()).contains(SENTINEL));
        assert_eq!(Redacted::new(SENTINEL).expose(), &SENTINEL);
    }
}

#[test]
fn error_output_never_contains_request_fields() {
    let base = contract_value("execute-attempt-catalog.json");
    // Every string field of a request carries the sentinel, one at a time, in a way that
    // makes the request fail; serde's own error text would echo it.
    for pointer in [
        "/projection/base/kind",
        "/projection/base/id",
        "/projection/base/digest",
        "/projection/runtime_mode",
        "/projection/revision",
        "/execution_credential/tenant",
        "/execution_credential/iat",
        "/invocation/identity/attempt-id",
        "/invocation/identity/idempotency-key",
        "/params/__ow_path",
        "/params/__ow_body",
        "/params/__ow_method",
        "/params/__ow_headers",
        "/unexpected",
    ] {
        let mut request = base.clone();
        if pointer == "/unexpected" {
            request[SENTINEL] = json!(1);
        } else {
            let target = request.pointer_mut(pointer);
            match target {
                Some(slot) => *slot = json!(SENTINEL),
                None => {
                    request["params"]["__ow_body"] = json!(SENTINEL);
                    request["params"]["__ow_headers"]["content-type"] = json!("application/json");
                }
            }
        }
        if let Err(error) = admit(&request) {
            assert_no_sentinel(&error);
        }
    }
    // Admitted attempts and results print only markers, even when every identifier is the
    // sentinel.
    let mut request = base;
    for pointer in [
        "/activation_id",
        "/execution_credential/activation",
        "/attempt_id",
        "/execution_credential/attempt",
        "/invocation/identity/attempt-id",
    ] {
        *request.pointer_mut(pointer).unwrap() = json!(SENTINEL);
    }
    let admitted = admit(&request).unwrap();
    assert!(!format!("{admitted:?}").contains(SENTINEL));
    let mut result = contract_value("result-catalog.json");
    result["activation_id"] = json!(SENTINEL);
    result["attempt_id"] = json!(SENTINEL);
    result["logs"] = json!([SENTINEL]);
    let parsed: ExecutionResult = parse_result(&admitted, &bytes(&result)).unwrap();
    assert!(!format!("{parsed:?}").contains(SENTINEL));
    result["identity"]["revision"] = json!(SENTINEL);
    let error = parse_result(&admitted, &bytes(&result)).unwrap_err();
    assert_no_sentinel(&error);
    assert_eq!(error, ApiError::IdentityMismatch(Mismatch::Revision));
    assert!(!format!("{:?}", MalformedReason::Json).contains(SENTINEL));
}
