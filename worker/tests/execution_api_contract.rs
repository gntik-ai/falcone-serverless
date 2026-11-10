//! ctr-worker-execution-api v1 conformance. Local and non-executing: no guest code runs.

#[path = "../src/api/mod.rs"]
pub mod api;

use api::{
    AdmitError, CancelMessage, CancelState, ExecuteAttempt, ExecutionResult, ExecutionUpdate,
    FailureClass, FailureCode, InvocationIdentity, LegacyInvocation, LegacyParams, LegacyRejection,
    LegacyResult, RuntimeMode, WorkerFacts, admit_wasm_attempt,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const WORKER: WorkerFacts<'static> = WorkerFacts {
    wasmtime_version: "49.0.5",
    interface_version: "0.2.0",
};
// Operator-pinned falcone sources (issue #124): path, sha256 of the file at the pinned revision.
const FALCONE_REVISION: &str = "a19f40ab9e160cd522c110efd50c30b2f5e0d708";
const PINNED_ERRORS: [(&str, &str); 3] = [
    (
        "base_disabled",
        "9ed46a38dca56af8f9b9fdc16a413fec45f48a4b1324de2d9a400ed31c49c7d8",
    ),
    (
        "base_mismatch",
        "2ac0fa4c05c628d6a251657de9918564652d32af9473502de510eaedfdcfa92c",
    ),
    (
        "oci_not_authorized",
        "ef0763c3f590a7e5fb31b59073e240d5320681b0f70f13757327cdb7bb26d740",
    ),
];
const CLAIMS_SCHEMA_SHA256: &str =
    "9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0";
const OPENAPI_SHA256: &str = "03121286b0c7ec716304d51a41b866477f4fc21671d95ebeafb8bf9da70e8960";
const SYNTHETIC_TOKEN: &str = "synthetic-0000-0000-0000";

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures() -> PathBuf {
    manifest().join("tests/fixtures/contract")
}

fn read_bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn fixture_bytes(relative: &str) -> Vec<u8> {
    read_bytes(&fixtures().join(relative))
}

fn fixture(relative: &str) -> Value {
    serde_json::from_slice(&fixture_bytes(relative)).expect("fixture is JSON")
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn spec() -> String {
    fs::read_to_string(manifest().join("../docs/contracts/worker-execution-api/README.md"))
        .expect("read v1 spec")
}

fn inventory() -> String {
    fs::read_to_string(manifest().join("../docs/contracts/falcone-contract-inventory.md"))
        .expect("read inventory")
}

/// Removes the value at a JSON pointer; panics if it is not there.
fn remove(value: &mut Value, pointer: &str) {
    let (parent, key) = pointer.rsplit_once('/').expect("pointer with a parent");
    let parent = value.pointer_mut(parent).expect("parent exists");
    let removed = match parent {
        Value::Object(map) => map.remove(key),
        _ => None,
    };
    assert!(removed.is_some(), "{pointer} was not present");
}

fn set(value: &mut Value, pointer: &str, new: Value) {
    *value.pointer_mut(pointer).expect("pointer exists") = new;
}

fn insert(value: &mut Value, pointer: &str, key: &str, new: Value) {
    value
        .pointer_mut(pointer)
        .and_then(Value::as_object_mut)
        .expect("object at pointer")
        .insert(key.to_owned(), new);
}

fn failure_code_names() -> BTreeSet<&'static str> {
    FailureCode::ALL.iter().map(|code| code.as_str()).collect()
}

// ---- AC1: runtime_mode is required; non-wasm is refused on the Wasm worker ----

#[test]
fn projection_without_runtime_mode_fails_deserialization() {
    let mut request = fixture("request/catalog-base.json");
    remove(&mut request, "/projection/runtime_mode");
    assert!(ExecuteAttempt::from_value(&request).is_err());
    // A missing mode is a malformed request, never a refusal.
    let bytes = serde_json::to_vec(&request).unwrap();
    assert!(matches!(
        admit_wasm_attempt(&bytes),
        Err(AdmitError::Malformed(_))
    ));
}

#[test]
fn runtime_mode_that_is_not_a_string_is_malformed_on_the_worker() {
    for bad in [
        json!(null),
        json!(1),
        json!(["wasm"]),
        json!({"mode": "wasm"}),
    ] {
        let mut request = fixture("request/catalog-base.json");
        set(&mut request, "/projection/runtime_mode", bad);
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(matches!(
            admit_wasm_attempt(&bytes),
            Err(AdmitError::Malformed(_))
        ));
    }
}

#[test]
fn oci_projection_is_refused_with_the_exact_refusal_shape() {
    // The request also carries OCI-only fields a later additive revision adds: the
    // worker must refuse on the mode first and never reject them as malformed.
    let bytes = fixture_bytes("request/refusal-oci-only-fields.json");
    let refusal = match admit_wasm_attempt(&bytes) {
        Err(AdmitError::Refused(refusal)) => refusal,
        other => panic!("expected the non-wasm refusal, got {other:?}"),
    };
    let result = refusal.into_result(WORKER).expect("refusal result");
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        fixture("result/refusal-non-wasm.json")
    );
    let failure = result.failure.as_ref().expect("failure block");
    assert_eq!(failure.code.as_str(), "runtime_mode_not_wasm");
    assert_eq!(failure.class, FailureClass::NonRetryable);
    // The same fixture parses back through the result type.
    ExecutionResult::from_value(&fixture("result/refusal-non-wasm.json")).expect("valid refusal");
}

#[test]
fn a_typed_oci_projection_without_extra_fields_is_refused_by_the_wasm_worker() {
    let bytes = fixture_bytes("request/byo-base.json");
    ExecuteAttempt::from_slice(&bytes).expect("typed oci request parses");
    assert!(matches!(
        admit_wasm_attempt(&bytes),
        Err(AdmitError::Refused(_))
    ));
}

#[test]
fn an_unknown_runtime_mode_value_is_refused_like_oci() {
    let mut request = fixture("request/catalog-base.json");
    set(&mut request, "/projection/runtime_mode", json!("native"));
    assert!(
        ExecuteAttempt::from_value(&request).is_err(),
        "the type knows only wasm|oci"
    );
    let bytes = serde_json::to_vec(&request).unwrap();
    let refusal = match admit_wasm_attempt(&bytes) {
        Err(AdmitError::Refused(refusal)) => refusal,
        other => panic!("expected the non-wasm refusal, got {other:?}"),
    };
    let result = refusal.into_result(WORKER).unwrap();
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["failure"]["code"], "runtime_mode_not_wasm");
    assert_eq!(value["failure"]["class"], "non_retryable");
    // The identity keeps the requested mode as written.
    assert_eq!(value["identity"]["runtime_mode"], "native");
    ExecutionResult::from_value(&value).expect("refusal for an unknown mode is valid");
}

#[test]
fn wasm_requests_are_admitted() {
    for name in ["catalog-base", "absent-base"] {
        let bytes = fixture_bytes(&format!("request/{name}.json"));
        let attempt = admit_wasm_attempt(&bytes).expect("wasm request admitted");
        assert_eq!(attempt.projection.runtime_mode, RuntimeMode::Wasm);
    }
}

// ---- AC2: base union ----

#[test]
fn base_variants_round_trip_unchanged() {
    for name in ["catalog-base", "byo-base", "absent-base"] {
        let original = fixture(&format!("request/{name}.json"));
        let attempt = ExecuteAttempt::from_value(&original).expect(name);
        assert_eq!(serde_json::to_value(&attempt).unwrap(), original, "{name}");
    }
    let absent = fixture("request/absent-base.json");
    assert!(absent["projection"].get("base").is_none());
}

#[test]
fn unresolved_default_alias_base_is_rejected_as_malformed() {
    assert!(
        ExecuteAttempt::from_value(&fixture("request/invalid-unresolved-alias-base.json")).is_err()
    );
    let catalog = |base: Value| {
        let mut request = fixture("request/catalog-base.json");
        set(&mut request, "/projection/base", base);
        request
    };
    let digest = format!("sha256:{}", "c3".repeat(32));
    let rejected = [
        json!({"kind": "catalog", "id": "javascript-base@1.2.0"}),
        json!({"kind": "catalog", "digest": digest}),
        json!({"kind": "catalog", "id": "javascript-base", "digest": digest}),
        json!({"kind": "catalog", "id": "javascript-base@", "digest": digest}),
        json!({"kind": "catalog", "id": "javascript-base@latest", "digest": digest}),
        json!({"kind": "catalog", "id": "default", "digest": digest}),
        json!({"kind": "catalog", "id": "default@1.0.0", "digest": digest}),
        json!({"kind": "catalog", "id": "@1.2.0", "digest": digest}),
        json!({"kind": "catalog", "id": "a@1.0.0@2", "digest": digest}),
        json!({"kind": "catalog", "id": "javascript-base@1.2.0", "digest": "latest"}),
        json!({"kind": "catalog", "id": "javascript-base@1.2.0", "digest": "sha256:ABCD"}),
        json!({"kind": "catalog", "id": "javascript-base@1.2.0", "digest": digest, "extra": 1}),
        json!(null),
    ];
    for base in rejected {
        assert!(
            ExecuteAttempt::from_value(&catalog(base.clone())).is_err(),
            "{base}"
        );
    }
}

#[test]
fn unknown_base_kind_and_missing_variant_fields_are_rejected() {
    let digest = format!("sha256:{}", "d4".repeat(32));
    let byo = |base: Value| {
        let mut request = fixture("request/byo-base.json");
        set(&mut request, "/projection/base", base);
        request
    };
    let rejected = [
        json!({"kind": "image", "image_digest": digest}),
        json!({"image_digest": digest}),
        json!({"kind": "byo", "signer_identity": "s", "attestation_ref": "a"}),
        json!({"kind": "byo", "image_digest": digest, "attestation_ref": "a"}),
        json!({"kind": "byo", "image_digest": digest, "signer_identity": "s"}),
        json!({"kind": "byo", "image_digest": digest, "signer_identity": "", "attestation_ref": "a"}),
        json!({"kind": "byo", "image_digest": "d4", "signer_identity": "s", "attestation_ref": "a"}),
        json!({"kind": "byo", "image_digest": digest, "signer_identity": "s", "attestation_ref": "a", "id": "x@1"}),
    ];
    for base in rejected {
        assert!(
            ExecuteAttempt::from_value(&byo(base.clone())).is_err(),
            "{base}"
        );
    }
}

#[test]
fn v1_types_deny_unknown_and_missing_fields() {
    let original = fixture("request/catalog-base.json");
    let mut extra_top = original.clone();
    insert(&mut extra_top, "", "extra", json!(1));
    let mut extra_projection = original.clone();
    insert(&mut extra_projection, "/projection", "extra", json!(1));
    let mut extra_budget = original.clone();
    insert(&mut extra_budget, "/projection/budgets", "extra", json!(1));
    for request in [extra_top, extra_projection, extra_budget] {
        assert!(ExecuteAttempt::from_value(&request).is_err());
    }
    for pointer in [
        "/projection/artifact_digest",
        "/projection/component_digest",
        "/projection/language_profile",
        "/projection/engine_version",
        "/projection/wasi_version",
        "/projection/interface_version",
        "/projection/declared_imports",
        "/projection/budgets",
        "/projection/granted_capabilities",
        "/projection/revision",
        "/credential",
        "/credential/token",
        "/credential/claims/tenant",
        "/deadline",
        "/params",
        "/invocation/correlation_id",
    ] {
        let mut request = original.clone();
        remove(&mut request, pointer);
        assert!(ExecuteAttempt::from_value(&request).is_err(), "{pointer}");
    }
    let mut zero_budget = original.clone();
    set(&mut zero_budget, "/projection/budgets/fuel", json!(0));
    assert!(ExecuteAttempt::from_value(&zero_budget).is_err());
}

#[test]
fn optional_fields_are_omitted_never_null() {
    let mut no_base = fixture("request/absent-base.json");
    insert(&mut no_base, "/projection", "base", json!(null));
    assert!(ExecuteAttempt::from_value(&no_base).is_err());
    let mut null_digest = fixture("result/success-absent-base.json");
    insert(&mut null_digest, "/identity", "base_digest", json!(null));
    assert!(ExecutionResult::from_value(&null_digest).is_err());
    let mut null_failure = fixture("result/success-catalog-base.json");
    insert(&mut null_failure, "", "failure", json!(null));
    assert!(ExecutionResult::from_value(&null_failure).is_err());
    let mut null_message = fixture("update/progress.json");
    set(&mut null_message, "/update/message", json!(null));
    assert!(ExecutionUpdate::from_value(&null_message).is_err());
    let mut null_body = legacy_fixture("valid", "200-json")["params"].clone();
    insert(&mut null_body, "", "__ow_body", json!(null));
    assert_eq!(
        LegacyParams::admit(&null_body),
        Err(LegacyRejection::EnvelopeInvalid)
    );
}

#[test]
fn deadline_is_an_rfc3339_utc_timestamp() {
    for good in ["2026-01-01T00:05:00Z", "2028-02-29T23:59:59.250Z"] {
        let mut request = fixture("request/catalog-base.json");
        set(&mut request, "/deadline", json!(good));
        assert!(ExecuteAttempt::from_value(&request).is_ok(), "{good}");
    }
    for bad in [
        "",
        "2026-02-30T00:00:00Z",
        "2027-02-29T00:00:00Z",
        "2026-01-01T00:05:00+01:00",
        "2026-01-01T24:00:00Z",
        "2026-01-01 00:05:00Z",
        "2026-13-01T00:00:00Z",
        "2026-01-01T00:05:00",
    ] {
        let mut request = fixture("request/catalog-base.json");
        set(&mut request, "/deadline", json!(bad));
        assert!(ExecuteAttempt::from_value(&request).is_err(), "{bad}");
    }
}

// ---- AC3: failure codes and classes ----

#[test]
fn every_failure_code_has_a_fixture_and_a_class() {
    let table = [
        ("oci_not_authorized", "non_retryable"),
        ("base_disabled", "non_retryable"),
        ("base_mismatch", "non_retryable"),
        ("runtime_mode_not_wasm", "non_retryable"),
    ];
    let names: BTreeSet<_> = table.iter().map(|(code, _)| *code).collect();
    assert_eq!(failure_code_names(), names);
    for (code, class) in table {
        let relative = if code == "runtime_mode_not_wasm" {
            "result/refusal-non-wasm.json".to_owned()
        } else {
            format!("result/failure-{code}.json")
        };
        let value = fixture(&relative);
        let result = ExecutionResult::from_value(&value).expect(code);
        let failure = result.failure.expect("failure block");
        assert_eq!(failure.code.as_str(), code);
        assert_eq!(value["failure"]["class"], class, "{code}");
        assert_eq!(
            serde_json::to_value(failure.class).unwrap(),
            json!(class),
            "{code}"
        );
    }
}

#[test]
fn the_spec_lists_every_failure_code_with_its_class() {
    let spec = spec();
    for code in FailureCode::ALL {
        let class = serde_json::to_value(code.class()).unwrap();
        let row = spec
            .lines()
            .find(|line| line.starts_with(&format!("| `{}` |", code.as_str())))
            .unwrap_or_else(|| panic!("no spec row for {}", code.as_str()));
        assert!(
            row.contains(&format!("| {} |", class.as_str().unwrap())),
            "{row}"
        );
    }
}

#[test]
fn oci_not_authorized_and_base_disabled_are_non_retryable_on_every_attempt() {
    for code in ["oci_not_authorized", "base_disabled"] {
        for attempt in 1..=5 {
            let mut value = fixture(&format!("result/failure-{code}.json"));
            let attempt_id = format!("attempt-synthetic-{attempt:04}");
            set(&mut value, "/attempt_id", json!(attempt_id));
            let result = ExecutionResult::from_value(&value).expect(code);
            assert_eq!(result.failure.unwrap().class, FailureClass::NonRetryable);
            for downgraded in ["retryable", "uncertain"] {
                set(&mut value, "/failure/class", json!(downgraded));
                assert!(
                    ExecutionResult::from_value(&value).is_err(),
                    "{code} attempt {attempt} accepted class {downgraded}"
                );
            }
        }
        // The committed retry fixture carries a later attempt id and the same class.
        let retry = fixture(&format!("result/failure-{code}-retry.json"));
        assert_ne!(
            retry["attempt_id"],
            fixture(&format!("result/failure-{code}.json"))["attempt_id"]
        );
        let retry = ExecutionResult::from_value(&retry).unwrap();
        assert_eq!(retry.failure.unwrap().class, FailureClass::NonRetryable);
    }
}

#[test]
fn failure_blocks_are_consistent_with_status() {
    let failure = fixture("result/failure-base_mismatch.json");
    let success = fixture("result/success-catalog-base.json");
    let mut cases = Vec::new();
    let mut failure_with_result = failure.clone();
    insert(&mut failure_with_result, "", "result", json!({}));
    cases.push(failure_with_result);
    let mut success_with_failure = success.clone();
    insert(
        &mut success_with_failure,
        "",
        "failure",
        failure["failure"].clone(),
    );
    cases.push(success_with_failure);
    let mut failure_without_block = failure.clone();
    remove(&mut failure_without_block, "/failure");
    cases.push(failure_without_block);
    let mut success_without_result = success.clone();
    remove(&mut success_without_result, "/result");
    cases.push(success_without_result);
    let mut unknown_code = failure.clone();
    set(&mut unknown_code, "/failure/code", json!("base_exploded"));
    cases.push(unknown_code);
    let mut missing_logs = success;
    remove(&mut missing_logs, "/logs");
    cases.push(missing_logs);
    for case in cases {
        assert!(ExecutionResult::from_value(&case).is_err(), "{case}");
    }
}

// ---- AC4: parity with the pinned ctr-functions-api v1 error fixtures ----

#[test]
fn pinned_functions_api_fixtures_match_their_recorded_sha256() {
    for (name, sha) in PINNED_ERRORS {
        let bytes = fixture_bytes(&format!("functions-api/errors/{name}.json"));
        assert_eq!(sha256_hex(&bytes), sha, "{name}");
    }
    let schema = fixture_bytes("functions-api/execution-credential.v1.claims.schema.json");
    assert_eq!(sha256_hex(&schema), CLAIMS_SCHEMA_SHA256);
}

#[test]
fn spec_records_every_pinned_falcone_source() {
    let spec = spec();
    assert!(spec.contains(FALCONE_REVISION));
    assert!(spec.contains(OPENAPI_SHA256));
    assert!(spec.contains("docs/contracts/serverless/functions-api/openapi.v1.yaml"));
    assert!(spec.contains(CLAIMS_SCHEMA_SHA256));
    assert!(spec.contains(
        "docs/contracts/serverless/execution-credential/execution-credential.v1.claims.schema.json"
    ));
    for (name, sha) in PINNED_ERRORS {
        let path = format!("tests/contracts/functions-api/fixtures/errors/{name}.json");
        let row = spec
            .lines()
            .find(|line| line.contains(&path))
            .unwrap_or_else(|| panic!("no provenance row for {path}"));
        assert!(row.contains(sha) && row.contains(FALCONE_REVISION), "{row}");
    }
}

#[test]
fn worker_codes_match_the_functions_api_error_fixtures() {
    let mut reasons = BTreeSet::new();
    for (name, _) in PINNED_ERRORS {
        let value = fixture(&format!("functions-api/errors/{name}.json"));
        let code = value["code"].as_str().expect("GW code");
        let reason = value["detail"]["reason"].as_str().expect("reason");
        let lowered = code.strip_prefix("GW_").expect("GW_ prefix").to_lowercase();
        assert_eq!(lowered, reason, "{name}: code and detail.reason disagree");
        assert_eq!(reason, name);
        assert_eq!(value["retryable"], false, "{name}");
        reasons.insert(reason.to_owned());
    }
    let worker_only: BTreeSet<_> = FailureCode::ALL
        .iter()
        .filter(|code| !reasons.contains(code.as_str()))
        .map(|code| code.as_str())
        .collect();
    assert_eq!(worker_only, BTreeSet::from(["runtime_mode_not_wasm"]));
    for reason in &reasons {
        assert!(failure_code_names().contains(reason.as_str()), "{reason}");
    }
    // A gateway code the fixtures define is never non-terminal in the worker either.
    for code in FailureCode::ALL {
        assert_eq!(code.class(), FailureClass::NonRetryable);
    }
}

// ---- AC5: legacy envelope and invocation identity, consumed in place ----

fn legacy_dir() -> PathBuf {
    manifest().join("../docs/contracts/fixtures/legacy-envelope")
}

/// (relative path, sha256) rows of the inventory legacy-envelope fixture table.
fn inventory_legacy_rows() -> Vec<(String, String)> {
    inventory()
        .lines()
        .filter(|line| line.starts_with("| docs/contracts/fixtures/legacy-envelope/"))
        .map(|line| {
            let cells: Vec<_> = line.split('|').map(str::trim).collect();
            (cells[1].to_owned(), cells[2].to_owned())
        })
        .collect()
}

#[test]
fn legacy_fixtures_are_consumed_in_place_with_inventory_sha256() {
    let rows = inventory_legacy_rows();
    assert_eq!(rows.len(), 11);
    for (path, sha) in &rows {
        let bytes = read_bytes(&manifest().join("..").join(path));
        assert_eq!(&sha256_hex(&bytes), sha, "{path}");
    }
    // No copy exists under the worker fixtures.
    let recorded: BTreeSet<_> = rows.iter().map(|(_, sha)| sha.clone()).collect();
    let mut stack = vec![fixtures()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                assert!(
                    !recorded.contains(&sha256_hex(&read_bytes(&path))),
                    "{} copies a legacy fixture",
                    path.display()
                );
            }
        }
    }
}

fn legacy_fixture(kind: &str, name: &str) -> Value {
    serde_json::from_slice(&read_bytes(
        &legacy_dir().join(kind).join(format!("{name}.json")),
    ))
    .expect("legacy fixture is JSON")
}

const VALID: [&str; 8] = [
    "401", "400", "429", "404", "403", "304", "200-json", "200-yaml",
];

#[test]
fn valid_legacy_fixtures_round_trip_through_the_legacy_types() {
    for name in VALID {
        let fixture = legacy_fixture("valid", name);
        let params = LegacyParams::admit(&fixture["params"]).expect(name);
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            fixture["params"],
            "{name}"
        );
        let result = LegacyResult::from_value(&fixture["expected"]).expect(name);
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            fixture["expected"],
            "{name}"
        );
        let invocation: LegacyInvocation =
            serde_json::from_value(fixture["invocation"].clone()).expect(name);
        assert_eq!(
            serde_json::to_value(&invocation).unwrap(),
            fixture["invocation"],
            "{name}"
        );
    }
}

#[test]
fn invalid_legacy_fixtures_are_rejected_before_invocation() {
    let labels = BTreeMap::from([
        ("missing-path", LegacyRejection::EnvelopeInvalid),
        ("oversized-body", LegacyRejection::BodyTooLarge),
        ("non-JSON-body", LegacyRejection::BodyNotJson),
    ]);
    for (name, rejection) in labels {
        let fixture = legacy_fixture("invalid", name);
        assert_eq!(fixture["expected"]["invoked"], false, "{name}");
        assert_eq!(
            LegacyParams::admit(&fixture["params"]),
            Err(rejection),
            "{name}"
        );
        // The label is the fixture's own rejection value, not a wire error code.
        assert_eq!(
            rejection.label(),
            fixture["expected"]["rejection"],
            "{name}"
        );
        assert!(!failure_code_names().contains(rejection.label()));
    }
}

#[test]
fn legacy_body_limit_follows_the_host_interface() {
    let wit = fs::read_to_string(manifest().join("wit/host.wit")).unwrap();
    assert!(wit.contains(&format!(
        "max-host-to-guest-bytes: {}",
        api::MAX_LEGACY_BODY_BYTES
    )));
    let mut params = legacy_fixture("valid", "200-json")["params"].clone();
    insert(
        &mut params,
        "",
        "__ow_body",
        json!("x".repeat(api::MAX_LEGACY_BODY_BYTES)),
    );
    assert!(LegacyParams::admit(&params).is_ok());
    set(
        &mut params,
        "/__ow_body",
        json!("x".repeat(api::MAX_LEGACY_BODY_BYTES + 1)),
    );
    assert_eq!(
        LegacyParams::admit(&params),
        Err(LegacyRejection::BodyTooLarge)
    );
}

#[test]
fn legacy_envelope_rejects_unknown_ow_parameters_and_bad_shapes() {
    let valid = legacy_fixture("valid", "200-json")["params"].clone();
    let mut unknown = valid.clone();
    insert(&mut unknown, "", "__ow_extra", json!("x"));
    assert_eq!(
        LegacyParams::admit(&unknown),
        Err(LegacyRejection::EnvelopeInvalid)
    );
    let mut bad_headers = valid.clone();
    set(&mut bad_headers, "/__ow_headers", json!(["a"]));
    assert_eq!(
        LegacyParams::admit(&bad_headers),
        Err(LegacyRejection::EnvelopeInvalid)
    );
    // Action parameters sit beside the __ow_* parameters.
    let mut action = valid;
    insert(&mut action, "", "format_hint", json!({"n": 1}));
    let params = LegacyParams::admit(&action).expect("action parameter accepted");
    assert_eq!(serde_json::to_value(&params).unwrap(), action);
    for bad in [
        json!({"statusCode": 99}),
        json!({"statusCode": 600}),
        json!({"statusCode": 200, "extra": 1}),
        json!({"statusCode": 304, "body": {"a": 1}}),
        json!({"headers": {}}),
    ] {
        assert!(LegacyResult::from_value(&bad).is_err(), "{bad}");
    }
}

#[test]
fn invocation_identity_round_trips_between_legacy_and_v1_forms() {
    let fixture = legacy_fixture("valid", "429");
    let legacy: LegacyInvocation = serde_json::from_value(fixture["invocation"].clone()).unwrap();
    let identity = legacy.identity().expect("consistent identity");
    assert_eq!(
        serde_json::to_value(&identity).unwrap(),
        json!({
            "invocation_key": "inv-synthetic-0003",
            "attempt_id": "attempt-synthetic-0001",
            "idempotency_key": "idem-legacy-0001",
            "correlation_id": "corr-legacy-0001"
        })
    );
    assert_eq!(identity.to_legacy(), legacy);
    let v1: InvocationIdentity = serde_json::from_value(
        fixture_for_identity("request/catalog-base.json")["invocation"].clone(),
    )
    .unwrap();
    assert_eq!(v1.to_legacy().identity().unwrap(), v1);
    // The idempotency key travels in the header and in the identity; they must agree.
    let mut mismatch = fixture["invocation"].clone();
    set(
        &mut mismatch,
        "/headers/Idempotency-Key",
        json!("idem-other-0001"),
    );
    let mismatch: LegacyInvocation = serde_json::from_value(mismatch).unwrap();
    assert!(mismatch.identity().is_err());
}

fn fixture_for_identity(relative: &str) -> Value {
    fixture(relative)
}

#[test]
fn legacy_fixtures_travel_inside_an_execute_attempt_unchanged() {
    for name in VALID {
        let legacy = legacy_fixture("valid", name);
        let mut request = fixture("request/catalog-base.json");
        set(&mut request, "/params", legacy["params"].clone());
        let identity: LegacyInvocation =
            serde_json::from_value(legacy["invocation"].clone()).unwrap();
        set(
            &mut request,
            "/invocation",
            serde_json::to_value(identity.identity().unwrap()).unwrap(),
        );
        let attempt = ExecuteAttempt::from_value(&request).expect(name);
        assert_eq!(serde_json::to_value(&attempt).unwrap(), request, "{name}");
        assert_eq!(attempt.invocation.to_legacy(), identity, "{name}");
    }
}

// ---- AC6: runtime identity ----

fn attempt(name: &str) -> ExecuteAttempt {
    ExecuteAttempt::from_slice(&fixture_bytes(&format!("request/{name}.json"))).expect(name)
}

fn result(name: &str) -> ExecutionResult {
    ExecutionResult::from_slice(&fixture_bytes(&format!("result/{name}.json"))).expect(name)
}

#[test]
fn result_requires_the_runtime_identity_block() {
    assert!(ExecutionResult::from_value(&fixture("result/invalid-identity-missing.json")).is_err());
    let valid = fixture("result/success-catalog-base.json");
    for pointer in [
        "/identity/function_revision",
        "/identity/runtime_mode",
        "/identity/artifact_digest",
        "/identity/wasmtime_version",
        "/identity/language_profile",
        "/identity/interface_version",
    ] {
        let mut value = valid.clone();
        remove(&mut value, pointer);
        assert!(ExecutionResult::from_value(&value).is_err(), "{pointer}");
    }
    let mut extra = valid;
    insert(&mut extra, "/identity", "extra", json!(1));
    assert!(ExecutionResult::from_value(&extra).is_err());
}

#[test]
fn base_digest_is_present_exactly_when_the_revision_pins_a_base() {
    let catalog = attempt("catalog-base");
    let absent = attempt("absent-base");
    let byo = attempt("byo-base");
    catalog
        .check_result(&result("success-catalog-base"))
        .expect("pinned base, digest present");
    absent
        .check_result(&result("success-absent-base"))
        .expect("no base, no digest");
    byo.check_result(&result("failure-oci_not_authorized"))
        .expect("byo digest is the image digest");
    // Mismatch 1: a base is pinned but the result omits its digest.
    let mut missing = fixture("result/success-catalog-base.json");
    remove(&mut missing, "/identity/base_digest");
    let missing = ExecutionResult::from_value(&missing).unwrap();
    assert!(catalog.check_result(&missing).is_err());
    // Mismatch 2: no base is pinned but the result reports one.
    let mut spurious = fixture("result/success-absent-base.json");
    insert(
        &mut spurious,
        "/identity",
        "base_digest",
        json!(format!("sha256:{}", "c3".repeat(32))),
    );
    let spurious = ExecutionResult::from_value(&spurious).unwrap();
    assert!(absent.check_result(&spurious).is_err());
    // A digest other than the pinned one is also a mismatch.
    let mut other = fixture("result/success-catalog-base.json");
    set(
        &mut other,
        "/identity/base_digest",
        json!(format!("sha256:{}", "e5".repeat(32))),
    );
    assert!(
        catalog
            .check_result(&ExecutionResult::from_value(&other).unwrap())
            .is_err()
    );
}

#[test]
fn identity_must_describe_the_pinned_revision() {
    let catalog = attempt("catalog-base");
    for (pointer, replacement) in [
        ("/identity/function_revision", json!("rev-other-0001")),
        (
            "/identity/artifact_digest",
            json!(format!("sha256:{}", "f6".repeat(32))),
        ),
        ("/identity/wasmtime_version", json!("48.0.0")),
        ("/identity/interface_version", json!("0.1.0")),
        (
            "/identity/language_profile",
            json!({"name": "python", "version": "synthetic-1"}),
        ),
        ("/identity/runtime_mode", json!("oci")),
        ("/activation_id", json!("act-other-0001")),
        ("/attempt_id", json!("attempt-other-0001")),
    ] {
        let mut value = fixture("result/success-catalog-base.json");
        set(&mut value, pointer, replacement);
        let parsed = ExecutionResult::from_value(&value).expect("well-formed");
        assert!(catalog.check_result(&parsed).is_err(), "{pointer}");
    }
}

#[test]
fn refusal_identity_never_claims_wasm() {
    let mut value = fixture("result/refusal-non-wasm.json");
    set(&mut value, "/identity/runtime_mode", json!("wasm"));
    assert!(ExecutionResult::from_value(&value).is_err());
    // A refusal still names the revision it refused.
    let oci = attempt("byo-base");
    oci.check_result(&result("refusal-non-wasm"))
        .expect("refusal matches the oci request");
    // A typed attempt knows its mode, so its refusal must name that mode.
    let mut renamed = fixture("result/refusal-non-wasm.json");
    set(&mut renamed, "/identity/runtime_mode", json!("native"));
    let renamed = ExecutionResult::from_value(&renamed).expect("an unknown-mode refusal is valid");
    assert!(oci.check_result(&renamed).is_err());
    // A refusal omitting the mode is malformed, and a non-refusal result never carries an
    // unrecognised mode.
    let mut unnamed = fixture("result/refusal-non-wasm.json");
    remove(&mut unnamed, "/identity/runtime_mode");
    assert!(ExecutionResult::from_value(&unnamed).is_err());
    let mut odd = fixture("result/failure-base_mismatch.json");
    set(&mut odd, "/identity/runtime_mode", json!("native"));
    assert!(ExecutionResult::from_value(&odd).is_err());
    let mut wrong = fixture("result/refusal-non-wasm.json");
    set(
        &mut wrong,
        "/identity/function_revision",
        json!("rev-other-0001"),
    );
    assert!(
        oci.check_result(&ExecutionResult::from_value(&wrong).unwrap())
            .is_err()
    );
}

#[test]
fn every_result_identity_field_is_required_for_refusals_and_oci_results() {
    for name in [
        "refusal-non-wasm",
        "failure-oci_not_authorized",
        "failure-oci_not_authorized-retry",
    ] {
        let valid = fixture(&format!("result/{name}.json"));
        ExecutionResult::from_value(&valid).expect(name);
        for field in [
            "function_revision",
            "artifact_digest",
            "wasmtime_version",
            "language_profile",
            "interface_version",
        ] {
            let mut value = valid.clone();
            remove(&mut value, &format!("/identity/{field}"));
            assert!(
                ExecutionResult::from_value(&value).is_err(),
                "{name} accepted an identity without {field}"
            );
        }
    }
}

#[test]
fn refusal_and_oci_results_follow_the_pinned_base_exactly() {
    let byo = attempt("byo-base");
    for name in ["refusal-non-wasm", "failure-oci_not_authorized"] {
        byo.check_result(&result(name)).expect(name);
        // Pinned base, digest omitted.
        let mut missing = fixture(&format!("result/{name}.json"));
        remove(&mut missing, "/identity/base_digest");
        let missing = ExecutionResult::from_value(&missing).expect("well-formed");
        assert!(
            byo.check_result(&missing).is_err(),
            "{name}: missing digest"
        );
        // Pinned base, another digest.
        let mut other = fixture(&format!("result/{name}.json"));
        set(
            &mut other,
            "/identity/base_digest",
            json!(format!("sha256:{}", "e5".repeat(32))),
        );
        let other = ExecutionResult::from_value(&other).expect("well-formed");
        assert!(byo.check_result(&other).is_err(), "{name}: other digest");
        // Artifact and language profile must be the pinned ones too.
        let mut artifact = fixture(&format!("result/{name}.json"));
        set(
            &mut artifact,
            "/identity/artifact_digest",
            json!(format!("sha256:{}", "f6".repeat(32))),
        );
        let artifact = ExecutionResult::from_value(&artifact).expect("well-formed");
        assert!(byo.check_result(&artifact).is_err(), "{name}: artifact");
        let mut profile = fixture(&format!("result/{name}.json"));
        set(
            &mut profile,
            "/identity/language_profile",
            json!({"name": "python", "version": "synthetic-1"}),
        );
        let profile = ExecutionResult::from_value(&profile).expect("well-formed");
        assert!(byo.check_result(&profile).is_err(), "{name}: profile");
    }
    // No base pinned: a refusal that reports one is a mismatch.
    let mut request = fixture("request/absent-base.json");
    set(&mut request, "/projection/runtime_mode", json!("oci"));
    let absent_oci = ExecuteAttempt::from_value(&request).expect("typed oci request");
    assert!(
        absent_oci
            .check_result(&result("refusal-non-wasm"))
            .is_err()
    );
    let mut no_base = fixture("result/refusal-non-wasm.json");
    remove(&mut no_base, "/identity/base_digest");
    absent_oci
        .check_result(&ExecutionResult::from_value(&no_base).unwrap())
        .expect("no base pinned, no digest");
}

#[test]
fn a_refusal_reports_the_pinned_artifact_language_and_base() {
    for (request, base_digest) in [
        ("request/catalog-base.json", Some("c3")),
        ("request/absent-base.json", None),
    ] {
        let mut value = fixture(request);
        set(&mut value, "/projection/runtime_mode", json!("native"));
        let bytes = serde_json::to_vec(&value).unwrap();
        let Err(AdmitError::Refused(refusal)) = admit_wasm_attempt(&bytes) else {
            panic!("{request}: expected the refusal");
        };
        let identity =
            serde_json::to_value(refusal.into_result(WORKER).unwrap()).unwrap()["identity"].clone();
        assert_eq!(
            identity["artifact_digest"],
            value["projection"]["artifact_digest"]
        );
        assert_eq!(
            identity["language_profile"],
            value["projection"]["language_profile"]
        );
        match base_digest {
            Some(byte) => assert_eq!(
                identity["base_digest"],
                json!(format!("sha256:{}", byte.repeat(32)))
            ),
            None => assert!(identity.get("base_digest").is_none()),
        }
    }
}

#[test]
fn a_refused_projection_missing_identity_facts_or_with_a_bad_base_is_malformed() {
    let bad_digest = json!("sha256:short");
    let cases: Vec<(&str, &str, Option<Value>)> = vec![
        ("request/byo-base.json", "/projection/artifact_digest", None),
        (
            "request/byo-base.json",
            "/projection/artifact_digest",
            Some(bad_digest.clone()),
        ),
        (
            "request/byo-base.json",
            "/projection/language_profile",
            None,
        ),
        (
            "request/byo-base.json",
            "/projection/base/image_digest",
            None,
        ),
        (
            "request/byo-base.json",
            "/projection/base/image_digest",
            Some(bad_digest.clone()),
        ),
        (
            "request/byo-base.json",
            "/projection/base/kind",
            Some(json!("floating")),
        ),
        ("request/catalog-base.json", "/projection/base/digest", None),
        (
            "request/catalog-base.json",
            "/projection/base/digest",
            Some(bad_digest),
        ),
        (
            "request/catalog-base.json",
            "/projection/base/id",
            Some(json!("default@latest")),
        ),
        (
            "request/catalog-base.json",
            "/projection/base/id",
            Some(json!("python")),
        ),
    ];
    for (name, pointer, replacement) in cases {
        let mut request = fixture(name);
        set(&mut request, "/projection/runtime_mode", json!("oci"));
        match replacement {
            Some(new) => set(&mut request, pointer, new),
            None => remove(&mut request, pointer),
        }
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(
            matches!(admit_wasm_attempt(&bytes), Err(AdmitError::Malformed(_))),
            "{name} {pointer} was not malformed"
        );
    }
}

// ---- Error messages never echo rejected input ----

const SECRET: &str = "synthetic-secret-value-9f3a";

fn assert_no_echo(error: &dyn std::fmt::Debug, what: &str) {
    assert!(
        !format!("{error:?}").contains(SECRET),
        "{what} echoed the rejected input"
    );
}

#[test]
fn rejected_input_is_never_echoed_by_errors() {
    let mut cases: Vec<(&str, Value)> = Vec::new();
    for (name, base, pointer) in [
        (
            "unknown request key",
            "request/catalog-base.json",
            "/projection",
        ),
        (
            "unknown credential key",
            "request/catalog-base.json",
            "/credential/claims",
        ),
        (
            "unknown base key",
            "request/catalog-base.json",
            "/projection/base",
        ),
    ] {
        let mut value = fixture(base);
        insert(&mut value, pointer, SECRET, json!(1));
        cases.push((name, value));
    }
    for (name, pointer) in [
        ("base kind", "/projection/base/kind"),
        ("runtime mode", "/projection/runtime_mode"),
        ("digest", "/projection/artifact_digest"),
        ("deadline", "/deadline"),
        ("catalog id", "/projection/base/id"),
        ("fuel", "/projection/budgets/fuel"),
    ] {
        let mut value = fixture("request/catalog-base.json");
        set(&mut value, pointer, json!(SECRET));
        cases.push((name, value));
    }
    for (name, request) in cases {
        let from_value = ExecuteAttempt::from_value(&request).expect_err(name);
        assert_no_echo(&from_value, name);
        assert!(!from_value.to_string().contains(SECRET), "{name}");
        let bytes = serde_json::to_vec(&request).unwrap();
        let from_slice = ExecuteAttempt::from_slice(&bytes).expect_err(name);
        assert_no_echo(&from_slice, name);
        if let Err(admit) = admit_wasm_attempt(&bytes) {
            assert_no_echo(&admit, name);
        }
    }
    let mut result_cases = Vec::new();
    for pointer in [
        "/status",
        "/failure/code",
        "/failure/class",
        "/identity/runtime_mode",
    ] {
        let mut value = fixture("result/failure-base_mismatch.json");
        set(&mut value, pointer, json!(SECRET));
        result_cases.push(value);
    }
    let mut value = fixture("result/failure-base_mismatch.json");
    insert(&mut value, "/identity", SECRET, json!(1));
    result_cases.push(value);
    for value in result_cases {
        let error = ExecutionResult::from_value(&value).expect_err("rejected result");
        assert_no_echo(&error, "result");
        assert!(!error.to_string().contains(SECRET));
    }
    let update = json!({
        "activation_id": "act-synthetic-0001", "attempt_id": "attempt-synthetic-0001",
        "sequence": 1, "update": {"type": SECRET}
    });
    assert_no_echo(
        &ExecutionUpdate::from_value(&update).unwrap_err(),
        "update type",
    );
    let mut cancel = fixture("cancel/requested.json");
    set(&mut cancel, "/state", json!(SECRET));
    assert_no_echo(
        &CancelMessage::from_value(&cancel).unwrap_err(),
        "cancel state",
    );
}

#[test]
fn success_fixtures_carry_the_legacy_envelope() {
    for name in ["success-catalog-base", "success-absent-base"] {
        let value = fixture(&format!("result/{name}.json"));
        let parsed = ExecutionResult::from_value(&value).expect(name);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value, "{name}");
        // result is the legacy handler result.
        LegacyResult::from_value(
            value["result"]
                .as_object()
                .map(|_| &value["result"])
                .unwrap(),
        )
        .expect("legacy result");
    }
}

// ---- Credential boundary ----

#[test]
fn credential_debug_and_display_redact_the_token() {
    let attempt = attempt("catalog-base");
    for text in [
        format!("{:?}", attempt.credential),
        format!("{}", attempt.credential),
        format!("{attempt:?}"),
        format!("{:#?}", attempt),
    ] {
        assert!(!text.contains(SYNTHETIC_TOKEN), "{text}");
        assert!(text.contains("<REDACTED>"), "{text}");
    }
    // The wire form still carries the token for the verifier.
    assert_eq!(attempt.credential.token().expose(), SYNTHETIC_TOKEN);
}

#[test]
fn credential_claims_bind_to_the_pinned_claims_schema() {
    let schema = fixture("functions-api/execution-credential.v1.claims.schema.json");
    let required: BTreeSet<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_owned())
        .collect();
    let properties: BTreeSet<_> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(required, properties);
    assert_eq!(schema["additionalProperties"], false);
    let request = fixture("request/catalog-base.json");
    let claims: BTreeSet<_> = request["credential"]["claims"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(claims, required);
    for name in &required {
        let mut value = request.clone();
        remove(&mut value, &format!("/credential/claims/{name}"));
        assert!(ExecuteAttempt::from_value(&value).is_err(), "{name}");
    }
    let mut extra = request.clone();
    insert(&mut extra, "/credential/claims", "scope", json!("x"));
    assert!(ExecuteAttempt::from_value(&extra).is_err());
    for pointer in ["/credential/claims/iat", "/credential/claims/exp"] {
        for bad in [json!(-1), json!("1")] {
            let mut value = request.clone();
            set(&mut value, pointer, bad);
            assert!(ExecuteAttempt::from_value(&value).is_err(), "{pointer}");
        }
    }
    let mut empty_token = request;
    set(&mut empty_token, "/credential/token", json!(""));
    assert!(ExecuteAttempt::from_value(&empty_token).is_err());
}

#[test]
fn credential_claims_must_match_the_attempt_and_revision() {
    let request = fixture("request/catalog-base.json");
    for (pointer, replacement) in [
        ("/credential/claims/activation", json!("act-other-0001")),
        ("/credential/claims/attempt", json!("attempt-other-0001")),
        ("/credential/claims/revision", json!("rev-other-0001")),
        ("/invocation/attempt_id", json!("attempt-other-0001")),
    ] {
        let mut value = request.clone();
        set(&mut value, pointer, replacement);
        assert!(ExecuteAttempt::from_value(&value).is_err(), "{pointer}");
    }
    // Credentials may narrow the revision grants, never widen them.
    let mut narrower = request.clone();
    set(
        &mut narrower,
        "/credential/claims/allowed_capabilities",
        json!([]),
    );
    assert!(ExecuteAttempt::from_value(&narrower).is_ok());
    let mut wider = request;
    insert_grant(&mut wider);
    assert!(ExecuteAttempt::from_value(&wider).is_err());
}

fn insert_grant(request: &mut Value) {
    let extra = json!({"family": "http", "interface": "falcone:host/http@0.2.0", "scope": {"hosts": ["api.example.invalid"]}});
    request
        .pointer_mut("/credential/claims/allowed_capabilities")
        .and_then(Value::as_array_mut)
        .unwrap()
        .push(extra);
}

#[test]
fn fixtures_hold_only_synthetic_values() {
    let mut stack = vec![fixtures()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let text = String::from_utf8(read_bytes(&path)).unwrap();
            for marker in ["Bearer ", "BEGIN PRIVATE", "eyJ", "password"] {
                assert!(
                    !text.contains(marker),
                    "{} contains {marker}",
                    path.display()
                );
            }
        }
    }
}

// ---- Updates and cancel ----

#[test]
fn execution_updates_round_trip() {
    for name in ["heartbeat", "progress", "checkpoint"] {
        let value = fixture(&format!("update/{name}.json"));
        let update = ExecutionUpdate::from_value(&value).expect(name);
        assert_eq!(serde_json::to_value(&update).unwrap(), value, "{name}");
    }
    let base = fixture("update/progress.json");
    let mut bad = Vec::new();
    for edit in [
        ("/update/percent", json!(101)),
        ("/update/type", json!("log")),
        ("/sequence", json!(-1)),
        ("/sequence", json!(0)),
        ("/update/message", json!("")),
    ] {
        let mut value = base.clone();
        set(&mut value, edit.0, edit.1);
        bad.push(value);
    }
    let mut heartbeat = fixture("update/heartbeat.json");
    insert(&mut heartbeat, "/update", "percent", json!(1));
    bad.push(heartbeat);
    let mut checkpoint = fixture("update/checkpoint.json");
    remove(&mut checkpoint, "/update/checkpoint_ref");
    bad.push(checkpoint);
    let mut inline = fixture("update/checkpoint.json");
    insert(&mut inline, "/update", "payload", json!("inline"));
    bad.push(inline);
    for value in bad {
        assert!(ExecutionUpdate::from_value(&value).is_err(), "{value}");
    }
}

#[test]
fn cancel_states_round_trip_and_uncertain_is_its_own_class() {
    let expected = [
        ("requested", CancelState::Requested, None),
        ("acknowledged", CancelState::Acknowledged, None),
        (
            "uncertain",
            CancelState::Uncertain,
            Some(FailureClass::Uncertain),
        ),
    ];
    for (name, state, class) in expected {
        let value = fixture(&format!("cancel/{name}.json"));
        let message: CancelMessage = serde_json::from_value(value.clone()).expect(name);
        assert_eq!(message.state, state);
        assert_eq!(message.state.outcome_class(), class, "{name}");
        assert_eq!(serde_json::to_value(&message).unwrap(), value, "{name}");
    }
    let mut unknown = fixture("cancel/requested.json");
    set(&mut unknown, "/state", json!("cancelled"));
    assert!(serde_json::from_value::<CancelMessage>(unknown).is_err());
}
