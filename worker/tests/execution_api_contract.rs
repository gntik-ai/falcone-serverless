//! ctr-worker-execution-api v1 conformance (RUN-01.t1). Local and non-executing: no guest
//! code runs and nothing touches the cluster. Every case goes through the public entry
//! points of `worker/src/api`.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

#[path = "../src/api/mod.rs"]
mod api;

use api::{
    error::{ParseError, Violation},
    failure::{FailureClass, FailureCode},
    request::{ExecuteRequest, InvocationIdentity, RuntimeMode, parse_request},
    result::{ExecutionResult, admit_wasm_attempt, parse_result},
    text::{HandlerResult, Name, Params},
    update::{
        CancelReport, CancelRequest, CancelState, ExecutionUpdate, parse_cancel_report,
        parse_cancel_request, parse_update,
    },
};

const SENTINEL: &str = "sentinel-credential-9f3a71c2";
const WASMTIME: &str = "49.0.5";

fn engine() -> Name {
    Name::new(WASMTIME).unwrap()
}

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(path)
}

fn fixture_bytes(path: &str) -> Vec<u8> {
    fs::read(repo("worker/tests/fixtures/contract").join(path)).unwrap()
}

fn fixture(path: &str) -> Value {
    serde_json::from_slice(&fixture_bytes(path)).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn request_json(name: &str) -> Value {
    fixture(&format!("worker-execution-api/requests/{name}.json"))
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn request(value: &Value) -> ExecuteRequest {
    parse_request(&bytes(value)).expect("request parses")
}

/// A request value with `pointer` replaced (or inserted, or removed when `new` is None).
fn with(mut value: Value, pointer: &str, new: Option<Value>) -> Value {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    let parent = value.pointer_mut(parent).unwrap();
    match (parent, new) {
        (Value::Object(map), Some(new)) => {
            map.insert(key.to_owned(), new);
        }
        (Value::Object(map), None) => {
            map.remove(key);
        }
        (Value::Array(items), Some(new)) => items[key.parse::<usize>().unwrap()] = new,
        _ => panic!("bad pointer {pointer}"),
    }
    value
}

/// The result a worker would send for `request`, with the identity derived by hand from the
/// request fixture's literals (never from the code under test).
fn result_json(request: &Value, status_fields: Value) -> Value {
    let projection = &request["projection"];
    let mut identity = json!({
        "function_revision": projection["revision"],
        "runtime_mode": projection["runtime_mode"],
        "artifact_digest": projection["artifact_digest"],
        "wasmtime_version": WASMTIME,
        "language_profile": projection["language_profile"],
        "interface_version": projection["interface_version"],
    });
    match projection.get("base") {
        Some(base) if base["kind"] == "catalog" => {
            identity["base_digest"] = base["digest"].clone();
        }
        Some(base) => identity["base_digest"] = base["image_digest"].clone(),
        None => {}
    }
    let mut result = json!({
        "activation_id": request["activation_id"],
        "attempt_id": request["attempt_id"],
        "logs": [],
        "identity": identity,
    });
    for (key, value) in status_fields.as_object().unwrap() {
        result[key] = value.clone();
    }
    result
}

fn success(request: &Value) -> Value {
    result_json(
        request,
        json!({"status": "success", "result": {"statusCode": 200, "body": {"ok": true}}}),
    )
}

fn failure(request: &Value, code: &str, class: &str) -> Value {
    result_json(
        request,
        json!({"status": "error", "failure": {"code": code, "class": class}}),
    )
}

fn rejected(outcome: Result<impl std::fmt::Debug, ParseError>) -> ParseError {
    outcome.expect_err("must be rejected")
}

fn is_malformed(error: &ParseError) -> bool {
    matches!(error, ParseError::Malformed(_))
}

// ---- runtime_mode and the non-wasm refusal --------------------------------------------

#[test]
fn runtime_mode_is_required_and_closed() {
    let base = request_json("absent-base");
    for (name, value) in [
        (
            "missing",
            with(base.clone(), "/projection/runtime_mode", None),
        ),
        (
            "unknown",
            with(
                base.clone(),
                "/projection/runtime_mode",
                Some(json!("container")),
            ),
        ),
        (
            "empty",
            with(base.clone(), "/projection/runtime_mode", Some(json!(""))),
        ),
        (
            "uppercase",
            with(
                base.clone(),
                "/projection/runtime_mode",
                Some(json!("WASM")),
            ),
        ),
        (
            "null",
            with(base.clone(), "/projection/runtime_mode", Some(Value::Null)),
        ),
        (
            "number",
            with(base.clone(), "/projection/runtime_mode", Some(json!(1))),
        ),
    ] {
        let error = rejected(parse_request(&bytes(&value)));
        assert!(is_malformed(&error), "{name}: {error}");
    }
    for name in ["missing-runtime-mode", "unknown-runtime-mode"] {
        let raw = fixture_bytes(&format!(
            "worker-execution-api/invalid-requests/{name}.json"
        ));
        assert!(is_malformed(&rejected(parse_request(&raw))), "{name}");
    }
    for mode in ["wasm", "oci"] {
        let value = with(base.clone(), "/projection/runtime_mode", Some(json!(mode)));
        assert!(parse_request(&bytes(&value)).is_ok(), "{mode}");
    }
}

#[test]
fn wasm_attempts_are_admitted_without_executing() {
    for name in ["catalog", "byo", "absent-base"] {
        let parsed = request(&request_json(name));
        assert_eq!(parsed.projection.runtime_mode, RuntimeMode::Wasm);
        assert!(admit_wasm_attempt(&parsed, &engine()).is_ok(), "{name}");
    }
}

#[test]
fn non_wasm_revisions_get_a_non_retryable_refusal() {
    let value = request_json("oci");
    let parsed = request(&value);
    assert_eq!(parsed.projection.runtime_mode, RuntimeMode::Oci);
    let refusal = admit_wasm_attempt(&parsed, &engine()).expect_err("oci is refused");
    assert_eq!(refusal.code(), FailureCode::RuntimeModeUnsupported);
    assert_eq!(refusal.class(), FailureClass::NonRetryable);
    // The refusal result is the documented fixture, field for field.
    let serialized = serde_json::to_value(refusal.result()).unwrap();
    assert_eq!(
        serialized,
        fixture("worker-execution-api/results/refusal-non-wasm.json")
    );
    assert_eq!(serialized["failure"]["code"], "runtime_mode_unsupported");
    assert_eq!(serialized["failure"]["class"], "non_retryable");
    assert_eq!(serialized["identity"]["runtime_mode"], "oci");
    assert!(serialized.get("result").is_none());
    // The dispatcher accepts it through the public result entry point.
    parse_result(&parsed, &bytes(&serialized)).expect("refusal result is conformant");
    // Retries of a refused attempt stay refused and non-retryable.
    for attempt in [2, 3, 9] {
        let retry = with(value.clone(), "/attempt_number", Some(json!(attempt)));
        let refusal = admit_wasm_attempt(&request(&retry), &engine()).unwrap_err();
        assert_eq!(
            refusal.class(),
            FailureClass::NonRetryable,
            "attempt {attempt}"
        );
        let result = serde_json::to_value(refusal.result()).unwrap();
        assert_eq!(result["failure"]["class"], "non_retryable");
    }
}

// ---- base union and unresolved aliases -------------------------------------------------

#[test]
fn catalog_byo_and_absent_base_round_trip() {
    for name in ["catalog", "byo", "absent-base", "oci"] {
        let value = request_json(name);
        let parsed = request(&value);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value, "{name}");
    }
    assert!(request(&request_json("catalog")).projection.base.is_some());
    assert!(request(&request_json("byo")).projection.base.is_some());
    let absent = request_json("absent-base");
    assert!(absent["projection"].get("base").is_none());
    assert!(request(&absent).projection.base.is_none());
    // Serializing a request without a base never invents the key.
    let serialized = serde_json::to_value(request(&absent)).unwrap();
    assert!(serialized["projection"].get("base").is_none());
}

#[test]
fn unresolved_default_aliases_are_malformed() {
    let catalog = request_json("catalog");
    let digest = catalog["projection"]["base"]["digest"].clone();
    let alias = |base: Value| with(catalog.clone(), "/projection/base", Some(base));
    for (name, value) in [
        (
            "kind default",
            alias(json!({"kind": "default", "id": "js-base"})),
        ),
        (
            "kind alias",
            alias(json!({"kind": "alias", "id": "js-base", "digest": digest})),
        ),
        ("bare string", alias(json!("default"))),
        ("null base", alias(Value::Null)),
        ("empty object", alias(json!({}))),
        (
            "latest",
            alias(
                json!({"kind": "catalog", "id": "js-base", "version": "latest", "digest": digest}),
            ),
        ),
        (
            "default version",
            alias(
                json!({"kind": "catalog", "id": "js-base", "version": "default", "digest": digest}),
            ),
        ),
        (
            "range",
            alias(json!({"kind": "catalog", "id": "js-base", "version": "^1.4", "digest": digest})),
        ),
        (
            "partial version",
            alias(json!({"kind": "catalog", "id": "js-base", "version": "1.4", "digest": digest})),
        ),
        (
            "id with version",
            alias(
                json!({"kind": "catalog", "id": "js-base@1.4.2", "version": "1.4.2", "digest": digest}),
            ),
        ),
        (
            "no digest",
            alias(json!({"kind": "catalog", "id": "js-base", "version": "1.4.2"})),
        ),
        (
            "tag digest",
            alias(
                json!({"kind": "catalog", "id": "js-base", "version": "1.4.2", "digest": "latest"}),
            ),
        ),
        (
            "short digest",
            alias(
                json!({"kind": "catalog", "id": "js-base", "version": "1.4.2", "digest": "sha256:abcd"}),
            ),
        ),
        (
            "upper digest",
            alias(
                json!({"kind": "catalog", "id": "js-base", "version": "1.4.2", "digest": format!("sha256:{}", "A".repeat(64))}),
            ),
        ),
        (
            "byo mixed",
            alias(
                json!({"kind": "byo", "id": "js-base", "image_digest": digest, "signer_identity": "s", "attestation_ref": "a"}),
            ),
        ),
    ] {
        let error = rejected(parse_request(&bytes(&value)));
        assert!(is_malformed(&error), "{name}: {error}");
    }
    for name in ["base-default-alias", "base-latest-version"] {
        let raw = fixture_bytes(&format!(
            "worker-execution-api/invalid-requests/{name}.json"
        ));
        assert!(is_malformed(&rejected(parse_request(&raw))), "{name}");
    }
}

// ---- failure codes and classes ---------------------------------------------------------

const CLASS_TABLE: [(&str, FailureCode, &str); 4] = [
    (
        "oci_not_authorized",
        FailureCode::OciNotAuthorized,
        "non_retryable",
    ),
    ("base_disabled", FailureCode::BaseDisabled, "non_retryable"),
    ("base_mismatch", FailureCode::BaseMismatch, "non_retryable"),
    (
        "runtime_mode_unsupported",
        FailureCode::RuntimeModeUnsupported,
        "non_retryable",
    ),
];

#[test]
fn every_failure_code_has_its_documented_class() {
    assert_eq!(FailureCode::ALL.len(), CLASS_TABLE.len());
    for (name, code, class) in CLASS_TABLE {
        assert!(FailureCode::ALL.contains(&code), "{name}");
        assert_eq!(code.name(), name);
        assert_eq!(serde_json::to_value(code).unwrap(), json!(name));
        assert_eq!(
            serde_json::to_value(code.class(1)).unwrap(),
            json!(class),
            "{name}"
        );
    }
    assert_eq!(FailureClass::ALL.len(), 3);
    for (class, name) in [
        (FailureClass::Retryable, "retryable"),
        (FailureClass::NonRetryable, "non_retryable"),
        (FailureClass::Uncertain, "uncertain"),
    ] {
        assert_eq!(serde_json::to_value(class).unwrap(), json!(name));
    }
}

#[test]
fn oci_not_authorized_and_base_disabled_stay_non_retryable_on_retries() {
    for code in [FailureCode::OciNotAuthorized, FailureCode::BaseDisabled] {
        for attempt in [1, 2, 3, 10, u32::MAX] {
            assert_eq!(
                code.class(attempt),
                FailureClass::NonRetryable,
                "{code:?} {attempt}"
            );
        }
    }
    // A result that downgrades either code to retryable or uncertain is refused at every attempt.
    for attempt in [1, 2, 5] {
        let value = with(
            request_json("catalog"),
            "/attempt_number",
            Some(json!(attempt)),
        );
        let parsed = request(&value);
        for code in ["oci_not_authorized", "base_disabled"] {
            for class in ["retryable", "uncertain"] {
                let result = failure(&value, code, class);
                let error = rejected(parse_result(&parsed, &bytes(&result)));
                assert_eq!(
                    error,
                    ParseError::Invalid(Violation::ClassMismatch),
                    "{code} {class} {attempt}"
                );
            }
            let result = failure(&value, code, "non_retryable");
            assert!(
                parse_result(&parsed, &bytes(&result)).is_ok(),
                "{code} {attempt}"
            );
        }
    }
}

#[test]
fn failure_fixtures_cover_every_code_and_class() {
    let catalog = request_json("catalog");
    let oci = request_json("oci");
    let cases = [
        ("failure-oci_not_authorized", &oci),
        ("failure-base_disabled", &catalog),
        ("failure-base_mismatch", &catalog),
        ("refusal-non-wasm", &oci),
        ("failure-retryable", &catalog),
        ("failure-uncertain", &catalog),
    ];
    let mut seen = Vec::new();
    for (name, source) in cases {
        let value = fixture(&format!("worker-execution-api/results/{name}.json"));
        let parsed = parse_result(&request(source), &bytes(&value)).expect(name);
        let failure = parsed.failure.as_ref().expect("failure result");
        seen.push((failure.code, failure.class));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value, "{name}");
    }
    for (_, code, class) in CLASS_TABLE {
        assert!(
            seen.iter()
                .any(|(c, k)| *c == Some(code) && serde_json::to_value(k).unwrap() == json!(class))
        );
    }
    assert!(seen.contains(&(None, FailureClass::Retryable)));
    assert!(seen.contains(&(None, FailureClass::Uncertain)));
}

#[test]
fn the_spec_lists_every_code_with_its_class() {
    let spec = fs::read_to_string(repo("docs/contracts/worker-execution-api/v1.md")).unwrap();
    for (name, _, class) in CLASS_TABLE {
        let row = spec
            .lines()
            .find(|line| line.starts_with(&format!("| `{name}` |")))
            .unwrap_or_else(|| panic!("spec row for {name}"));
        assert!(row.contains(&format!("`{class}`")), "{row}");
    }
    for class in ["retryable", "non_retryable", "uncertain"] {
        assert!(spec.contains(&format!("`{class}`")), "{class}");
    }
}

// ---- cross-contract check against ctr-functions-api v1 -----------------------------------

const FALCONE_REVISION: &str = "a19f40ab9e160cd522c110efd50c30b2f5e0d708";
// Hashes from the operator note; independent of anything the worker computes.
const PINNED_ERRORS: [(&str, &str, &str); 3] = [
    (
        "base_disabled",
        "9ed46a38dca56af8f9b9fdc16a413fec45f48a4b1324de2d9a400ed31c49c7d8",
        "GW_BASE_DISABLED",
    ),
    (
        "base_mismatch",
        "2ac0fa4c05c628d6a251657de9918564652d32af9473502de510eaedfdcfa92c",
        "GW_BASE_MISMATCH",
    ),
    (
        "oci_not_authorized",
        "ef0763c3f590a7e5fb31b59073e240d5320681b0f70f13757327cdb7bb26d740",
        "GW_OCI_NOT_AUTHORIZED",
    ),
];

fn inventory() -> String {
    fs::read_to_string(repo("docs/contracts/falcone-contract-inventory.md")).unwrap()
}

/// `| path | sha256 | ... |` rows of the inventory, keyed by path.
fn inventory_rows(prefix: &str) -> Vec<(String, String)> {
    inventory()
        .lines()
        .filter(|line| line.starts_with(&format!("| {prefix}")))
        .map(|line| {
            let cells: Vec<_> = line.trim_matches('|').split('|').map(str::trim).collect();
            (cells[0].to_owned(), cells[1].to_owned())
        })
        .collect()
}

#[test]
fn worker_base_codes_match_the_pinned_functions_api_error_fixtures() {
    let rows = inventory_rows("worker/tests/fixtures/contract/functions-api-errors/");
    assert_eq!(rows.len(), PINNED_ERRORS.len());
    let inventory = inventory();
    assert!(inventory.contains(FALCONE_REVISION));
    for digest in [
        "03121286b0c7ec716304d51a41b866477f4fc21671d95ebeafb8bf9da70e8960",
        "9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0",
    ] {
        assert!(inventory.contains(digest), "{digest}");
    }
    let mut reasons = Vec::new();
    for (name, digest, gateway_code) in PINNED_ERRORS {
        let path = format!("functions-api-errors/{name}.json");
        let raw = fixture_bytes(&path);
        assert_eq!(sha256(&raw), digest, "{name}: pinned bytes changed");
        let recorded = format!("worker/tests/fixtures/contract/{path}");
        assert!(
            rows.contains(&(recorded, digest.to_owned())),
            "{name}: inventory row"
        );
        let value: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(value["code"], gateway_code);
        assert_eq!(value["retryable"], false);
        reasons.push(value["detail"]["reason"].as_str().unwrap().to_owned());
    }
    // Every OCI/base code name of the worker spec is a detail.reason of the pinned fixtures,
    // and the only worker-defined code is the non-wasm refusal.
    let mut worker_only = Vec::new();
    for code in FailureCode::ALL {
        match reasons.iter().find(|reason| reason.as_str() == code.name()) {
            Some(_) => assert_eq!(code.class(1), FailureClass::NonRetryable, "{}", code.name()),
            None => worker_only.push(code),
        }
    }
    assert_eq!(worker_only, [FailureCode::RuntimeModeUnsupported]);
    for reason in &reasons {
        assert!(
            FailureCode::ALL.iter().any(|code| code.name() == reason),
            "{reason}"
        );
    }
    // The pinned class and the worker class agree: gateway retryable=false is non_retryable.
    for code in FailureCode::ALL
        .iter()
        .filter(|c| **c != FailureCode::RuntimeModeUnsupported)
    {
        assert_eq!(code.class(1), FailureClass::NonRetryable);
    }
}

#[test]
fn the_credential_claims_follow_the_pinned_schema() {
    let value = request_json("catalog");
    let claims = value["credential"]["claims"].as_object().unwrap();
    // The required claim names of execution-credential.v1.claims.schema.json (sha256 9c98ba16...).
    let required = [
        "iss",
        "aud",
        "tenant",
        "workspace",
        "pool_profile",
        "revision",
        "activation",
        "attempt",
        "iat",
        "exp",
        "allowed_capabilities",
    ];
    let mut names: Vec<_> = claims.keys().map(String::as_str).collect();
    names.sort_unstable();
    let mut expected = required.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected);
    for name in required {
        let without = with(value.clone(), &format!("/credential/claims/{name}"), None);
        assert!(
            is_malformed(&rejected(parse_request(&bytes(&without)))),
            "{name}"
        );
    }
    let extra = with(value.clone(), "/credential/claims/extra", Some(json!("x")));
    assert!(is_malformed(&rejected(parse_request(&bytes(&extra)))));
    // An opaque credential string is not the v1 shape.
    let opaque = with(value.clone(), "/credential", Some(json!("opaque-token")));
    assert!(is_malformed(&rejected(parse_request(&bytes(&opaque)))));
}

#[test]
fn credential_claims_must_bind_the_attempt() {
    let value = request_json("catalog");
    for (pointer, violation) in [
        (
            "/credential/claims/activation",
            Violation::CredentialMismatch,
        ),
        ("/credential/claims/attempt", Violation::CredentialMismatch),
        ("/credential/claims/revision", Violation::CredentialMismatch),
        ("/invocation/attempt-id", Violation::IdentityMismatch),
    ] {
        let changed = with(value.clone(), pointer, Some(json!("other-value-0001")));
        assert_eq!(
            rejected(parse_request(&bytes(&changed))),
            ParseError::Invalid(violation),
            "{pointer}"
        );
    }
    let expired = with(value.clone(), "/credential/claims/exp", Some(json!(1)));
    assert_eq!(
        rejected(parse_request(&bytes(&expired))),
        ParseError::Invalid(Violation::CredentialMismatch)
    );
    let broader = with(
        value.clone(),
        "/credential/claims/allowed_capabilities/0",
        Some(
            json!({"family": "storage", "interface": "falcone:host/storage@0.2.0", "scope": {"prefixes": ["other/"]}}),
        ),
    );
    assert_eq!(
        rejected(parse_request(&bytes(&broader))),
        ParseError::Invalid(Violation::CapabilityNotGranted)
    );
    for zero in ["fuel", "memory_bytes", "timeout_ms", "max_host_calls"] {
        let changed = with(
            value.clone(),
            &format!("/projection/budgets/{zero}"),
            Some(json!(0)),
        );
        assert!(
            is_malformed(&rejected(parse_request(&bytes(&changed)))),
            "{zero}"
        );
    }
    let attempt = with(value.clone(), "/attempt_number", Some(json!(0)));
    assert!(is_malformed(&rejected(parse_request(&bytes(&attempt)))));
}

// ---- legacy envelope and invocation identity fixtures ------------------------------------

#[test]
fn legacy_envelope_fixtures_round_trip_unchanged() {
    let rows = inventory_rows("docs/contracts/fixtures/legacy-envelope/");
    assert_eq!(
        rows.len(),
        11,
        "8 valid + 3 invalid fixtures are inventoried"
    );
    let template = request_json("absent-base");
    for (path, digest) in rows {
        let raw = fs::read(repo(&path)).unwrap();
        assert_eq!(
            sha256(&raw),
            digest,
            "{path}: sha256 of the inventoried bytes"
        );
        let value: Value = serde_json::from_slice(&raw).unwrap();
        // invocation identity -> invocation-key, attempt-id, idempotency-key
        let identity = &value["invocation"]["identity"];
        let parsed: InvocationIdentity = serde_json::from_value(identity.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), *identity, "{path}");
        for key in ["invocation-key", "attempt-id", "idempotency-key"] {
            assert!(identity.get(key).is_some(), "{path}: {key}");
        }
        // params -> the request params, unchanged
        let params: Params = serde_json::from_value(value["params"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            value["params"],
            "{path}"
        );
        // The same data travels through a full execute request and back.
        let mut carried = with(template.clone(), "/params", Some(value["params"].clone()));
        carried = with(carried, "/invocation", Some(identity.clone()));
        let attempt = identity["attempt-id"].clone();
        carried = with(carried, "/attempt_id", Some(attempt.clone()));
        carried = with(carried, "/credential/claims/attempt", Some(attempt));
        let parsed = request(&carried);
        let echoed = serde_json::to_value(&parsed).unwrap();
        assert_eq!(echoed["params"], value["params"], "{path}");
        assert_eq!(echoed["invocation"], *identity, "{path}");
        // expected -> handler result inside the result envelope (valid fixtures only)
        if let Some(expected) = value.get("expected").filter(|_| path.contains("/valid/")) {
            let handler: HandlerResult = serde_json::from_value(expected.clone()).unwrap();
            assert_eq!(serde_json::to_value(&handler).unwrap(), *expected, "{path}");
            let envelope = result_json(
                &carried,
                json!({"status": "success", "result": expected, "logs": ["handler ran"]}),
            );
            let result: ExecutionResult = parse_result(&parsed, &bytes(&envelope)).expect(&path);
            let back = serde_json::to_value(&result).unwrap();
            assert_eq!(back["result"], *expected, "{path}");
            assert_eq!(back["logs"], json!(["handler ran"]), "{path}");
        }
    }
}

// ---- result identity and absent-versus-null -----------------------------------------------

#[test]
fn parse_result_enforces_base_digest_exactly_when_a_base_is_pinned() {
    let pinned = request_json("catalog");
    let none = request_json("absent-base");
    let digest = pinned["projection"]["base"]["digest"].clone();
    let accepted = |request_value: &Value, result: &Value| {
        parse_result(&request(request_value), &bytes(result)).is_ok()
    };
    let violation = |request_value: &Value, result: &Value| {
        rejected(parse_result(&request(request_value), &bytes(result)))
    };

    // pinned + digest accepted
    assert!(accepted(&pinned, &success(&pinned)));
    // pinned + absent rejected
    let missing = with(success(&pinned), "/identity/base_digest", None);
    assert_eq!(
        violation(&pinned, &missing),
        ParseError::Invalid(Violation::BaseDigestMissing)
    );
    // pinned + null rejected
    let null = with(success(&pinned), "/identity/base_digest", Some(Value::Null));
    assert!(!accepted(&pinned, &null));
    assert!(matches!(
        violation(&pinned, &null),
        ParseError::Malformed(_)
    ));
    // no base + absent accepted
    assert!(accepted(&none, &success(&none)));
    // no base + null rejected
    let null = with(success(&none), "/identity/base_digest", Some(Value::Null));
    assert!(!accepted(&none, &null));
    assert!(matches!(violation(&none, &null), ParseError::Malformed(_)));
    // no base + a digest rejected
    let extra = with(
        success(&none),
        "/identity/base_digest",
        Some(digest.clone()),
    );
    assert_eq!(
        violation(&none, &extra),
        ParseError::Invalid(Violation::BaseDigestUnexpected)
    );
    // a digest that is not the pinned base's digest is a mismatch, not a pass
    let wrong = with(
        success(&pinned),
        "/identity/base_digest",
        Some(json!(format!("sha256:{}", "0".repeat(64)))),
    );
    assert_eq!(
        violation(&pinned, &wrong),
        ParseError::Invalid(Violation::IdentityMismatch)
    );
    // the byo variant binds its image digest
    let byo = request_json("byo");
    assert!(accepted(&byo, &success(&byo)));
    let byo_missing = with(success(&byo), "/identity/base_digest", None);
    assert_eq!(
        violation(&byo, &byo_missing),
        ParseError::Invalid(Violation::BaseDigestMissing)
    );
    // the same rule holds for failure results
    let refused = failure(&pinned, "base_disabled", "non_retryable");
    assert!(accepted(&pinned, &refused));
    let refused = with(refused, "/identity/base_digest", None);
    assert!(!accepted(&pinned, &refused));
}

#[test]
fn identity_is_required_and_must_describe_the_pinned_revision() {
    let value = request_json("catalog");
    let parsed = request(&value);
    assert!(is_malformed(&rejected(parse_result(
        &parsed,
        &bytes(&with(success(&value), "/identity", None))
    ))));
    for field in [
        "function_revision",
        "runtime_mode",
        "artifact_digest",
        "wasmtime_version",
        "language_profile",
        "interface_version",
    ] {
        let missing = with(success(&value), &format!("/identity/{field}"), None);
        assert!(
            is_malformed(&rejected(parse_result(&parsed, &bytes(&missing)))),
            "{field}"
        );
    }
    for (field, other) in [
        ("function_revision", json!("rev-other-0001")),
        ("runtime_mode", json!("oci")),
        (
            "artifact_digest",
            json!(format!("sha256:{}", "1".repeat(64))),
        ),
        (
            "language_profile",
            json!({"name": "python", "version": "1.0.0"}),
        ),
        ("interface_version", json!("0.9.0")),
    ] {
        let changed = with(success(&value), &format!("/identity/{field}"), Some(other));
        assert_eq!(
            rejected(parse_result(&parsed, &bytes(&changed))),
            ParseError::Invalid(Violation::IdentityMismatch),
            "{field}"
        );
    }
    for pointer in ["/activation_id", "/attempt_id"] {
        let changed = with(success(&value), pointer, Some(json!("other-0001")));
        assert_eq!(
            rejected(parse_result(&parsed, &bytes(&changed))),
            ParseError::Invalid(Violation::CorrelationMismatch),
            "{pointer}"
        );
    }
}

#[test]
fn absent_means_absent_for_every_optional_result_field() {
    let value = request_json("catalog");
    let parsed = request(&value);
    let ok = |result: &Value| parse_result(&parsed, &bytes(result));
    // failure on success, result on error and code on a failure may be absent, never null.
    assert!(ok(&with(success(&value), "/failure", Some(Value::Null))).is_err());
    let error = failure(&value, "base_mismatch", "non_retryable");
    assert!(ok(&with(error.clone(), "/result", Some(Value::Null))).is_err());
    assert!(ok(&with(error.clone(), "/failure/code", Some(Value::Null))).is_err());
    // an uncoded failure is legal: the class alone says how to treat it
    assert!(ok(&with(error.clone(), "/failure/code", None)).is_ok());
    // status and payload must agree
    let both = with(
        success(&value),
        "/failure",
        Some(json!({"class": "non_retryable"})),
    );
    assert_eq!(
        rejected(ok(&both)),
        ParseError::Invalid(Violation::StatusShape)
    );
    let neither = with(error.clone(), "/failure", None);
    assert!(matches!(
        rejected(ok(&neither)),
        ParseError::Invalid(Violation::StatusShape)
    ));
    let no_result = with(success(&value), "/result", None);
    assert_eq!(
        rejected(ok(&no_result)),
        ParseError::Invalid(Violation::StatusShape)
    );
    // logs are required
    assert!(is_malformed(&rejected(ok(&with(
        success(&value),
        "/logs",
        None
    )))));
    assert!(is_malformed(&rejected(ok(&with(
        success(&value),
        "/logs",
        Some(Value::Null)
    )))));
    // unknown codes and statuses are malformed, not silently generic
    assert!(is_malformed(&rejected(ok(&failure(
        &value,
        "base_gone",
        "non_retryable"
    )))));
    assert!(is_malformed(&rejected(ok(&with(
        success(&value),
        "/status",
        Some(json!("ok"))
    )))));
    assert!(is_malformed(&rejected(ok(&with(
        success(&value),
        "/extra",
        Some(json!(1))
    )))));
}

// ---- updates and cancel ---------------------------------------------------------------------

#[test]
fn execution_updates_and_cancel_round_trip() {
    for name in ["heartbeat", "progress", "checkpoint"] {
        let value = fixture(&format!("worker-execution-api/updates/{name}.json"));
        let update: ExecutionUpdate = parse_update(&bytes(&value)).expect(name);
        assert_eq!(serde_json::to_value(&update).unwrap(), value, "{name}");
        assert_eq!(value["kind"], name);
    }
    let value = fixture("worker-execution-api/cancel/request.json");
    let request: CancelRequest = parse_cancel_request(&bytes(&value)).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), value);
    for (name, state) in [
        ("requested", CancelState::Requested),
        ("acknowledged", CancelState::Acknowledged),
        ("uncertain", CancelState::Uncertain),
    ] {
        let value = fixture(&format!("worker-execution-api/cancel/{name}.json"));
        let report: CancelReport = parse_cancel_report(&bytes(&value)).expect(name);
        assert_eq!(report.state, state);
        assert_eq!(serde_json::to_value(&report).unwrap(), value, "{name}");
        assert_eq!(value["state"], name);
    }
    let value = fixture("worker-execution-api/updates/heartbeat.json");
    for bad in [
        with(value.clone(), "/kind", Some(json!("log"))),
        with(value.clone(), "/kind", None),
        with(value.clone(), "/sequence", Some(json!(-1))),
        with(value.clone(), "/activation_id", Some(json!(""))),
    ] {
        assert!(is_malformed(&rejected(parse_update(&bytes(&bad)))));
    }
    let report = fixture("worker-execution-api/cancel/uncertain.json");
    let bad = with(report, "/state", Some(json!("done")));
    assert!(is_malformed(&rejected(parse_cancel_report(&bytes(&bad)))));
}

// ---- redaction ------------------------------------------------------------------------------

fn rendered(value: &(impl std::fmt::Debug + ?Sized)) -> String {
    format!("{value:?}\n{value:#?}")
}

/// A refused request whose activation id, attempt id and revision each carry the sentinel,
/// consistently in every place the contract binds them.
fn sentinel_request() -> Value {
    let mut value = request_json("oci");
    for (group, tag) in [
        (
            &["/activation_id", "/credential/claims/activation"][..],
            "act",
        ),
        (
            &[
                "/attempt_id",
                "/credential/claims/attempt",
                "/invocation/attempt-id",
            ][..],
            "attempt",
        ),
        (
            &["/projection/revision", "/credential/claims/revision"][..],
            "rev",
        ),
    ] {
        for pointer in group {
            value = with(value, pointer, Some(json!(format!("{SENTINEL}-{tag}"))));
        }
    }
    value
}

#[test]
fn refusal_output_never_carries_request_fields() {
    let value = sentinel_request();
    let parsed = request(&value);
    let refusal = admit_wasm_attempt(&parsed, &engine()).unwrap_err();
    let mut output = vec![refusal.to_string(), format!("{refusal}")];
    output.push(rendered(&refusal));
    output.push(rendered(&refusal.result()));
    output.push(rendered(&parsed));
    output.push(rendered(&parsed.projection));
    output.push(rendered(&Err::<(), _>(&refusal)));
    output.push(rendered(&refusal.code()));
    for text in &output {
        assert!(
            !text.contains(SENTINEL),
            "request-controlled value leaked: {text}"
        );
    }
    let joined = output.join("\n");
    assert!(joined.contains("<redacted>"));
    // Request-derived enums and validated codes are still printed.
    assert!(
        joined.contains("runtime_mode_unsupported") || joined.contains("RuntimeModeUnsupported")
    );
    // The serialized refusal keeps its contract fields, including the request ids.
    let serialized = serde_json::to_value(refusal.result()).unwrap();
    assert_eq!(serialized["activation_id"], value["activation_id"]);
    assert_eq!(serialized["attempt_id"], value["attempt_id"]);
    assert_eq!(
        serialized["identity"]["function_revision"],
        value["projection"]["revision"]
    );
    assert!(serialized.to_string().contains(SENTINEL));
}

#[test]
fn each_request_field_is_redacted_on_its_own() {
    // A short credential-like value in each field alone, in the request-derived error text too.
    for pointer in [
        "/activation_id",
        "/attempt_id",
        "/projection/revision",
        "/projection/language_profile/name",
        "/projection/engine_version",
        "/credential/claims/iss",
        "/credential/claims/tenant",
        "/params/__ow_path",
    ] {
        let mut value = request_json("oci");
        value = with(value, pointer, Some(json!(SENTINEL)));
        // keep the bindings consistent so that the request itself parses
        if pointer == "/activation_id" {
            value = with(
                value,
                "/credential/claims/activation",
                Some(json!(SENTINEL)),
            );
        }
        if pointer == "/attempt_id" {
            value = with(value, "/credential/claims/attempt", Some(json!(SENTINEL)));
            value = with(value, "/invocation/attempt-id", Some(json!(SENTINEL)));
        }
        if pointer == "/projection/revision" {
            value = with(value, "/credential/claims/revision", Some(json!(SENTINEL)));
        }
        let parsed = request(&value);
        let refusal = admit_wasm_attempt(&parsed, &engine()).unwrap_err();
        for text in [
            refusal.to_string(),
            rendered(&refusal),
            rendered(&parsed),
            rendered(refusal.result()),
        ] {
            assert!(!text.contains(SENTINEL), "{pointer}: {text}");
        }
    }
}

#[test]
fn parse_errors_never_echo_input() {
    let catalog = request_json("catalog");
    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, value) in [
        (
            "unknown runtime mode",
            with(
                catalog.clone(),
                "/projection/runtime_mode",
                Some(json!(SENTINEL)),
            ),
        ),
        (
            "unknown top field",
            with(catalog.clone(), &format!("/{SENTINEL}"), Some(json!(1))),
        ),
        (
            "unknown nested field",
            with(
                catalog.clone(),
                &format!("/projection/{SENTINEL}"),
                Some(json!(1)),
            ),
        ),
        (
            "unknown base kind",
            with(
                catalog.clone(),
                "/projection/base/kind",
                Some(json!(SENTINEL)),
            ),
        ),
        (
            "alias version",
            with(
                catalog.clone(),
                "/projection/base/version",
                Some(json!(SENTINEL)),
            ),
        ),
        (
            "bad digest",
            with(
                catalog.clone(),
                "/projection/artifact_digest",
                Some(json!(SENTINEL)),
            ),
        ),
        (
            "bad type",
            with(catalog.clone(), "/attempt_number", Some(json!(SENTINEL))),
        ),
        (
            "credential string",
            with(catalog.clone(), "/credential", Some(json!(SENTINEL))),
        ),
        (
            "activation mismatch",
            with(
                catalog.clone(),
                "/credential/claims/activation",
                Some(json!(SENTINEL)),
            ),
        ),
        (
            "attempt mismatch",
            with(
                catalog.clone(),
                "/invocation/attempt-id",
                Some(json!(SENTINEL)),
            ),
        ),
        (
            "revision mismatch",
            with(
                catalog.clone(),
                "/credential/claims/revision",
                Some(json!(SENTINEL)),
            ),
        ),
        (
            "grant mismatch",
            with(
                catalog.clone(),
                "/credential/claims/allowed_capabilities/0",
                Some(json!({"family": SENTINEL})),
            ),
        ),
    ] {
        cases.push((name.into(), bytes(&value)));
    }
    cases.push((
        "not json".into(),
        format!("{{\"activation_id\": {SENTINEL}").into_bytes(),
    ));
    cases.push(("trailing".into(), format!("{{}} {SENTINEL}").into_bytes()));
    for (name, raw) in cases {
        let error = rejected(parse_request(&raw));
        for text in [error.to_string(), rendered(&error)] {
            assert!(!text.contains(SENTINEL), "{name}: {text}");
        }
    }
    let parsed = request(&catalog);
    let good = success(&catalog);
    for (name, value) in [
        ("code", failure(&catalog, SENTINEL, "non_retryable")),
        ("class", failure(&catalog, "base_disabled", SENTINEL)),
        (
            "status",
            with(good.clone(), "/status", Some(json!(SENTINEL))),
        ),
        (
            "activation",
            with(good.clone(), "/activation_id", Some(json!(SENTINEL))),
        ),
        (
            "attempt",
            with(good.clone(), "/attempt_id", Some(json!(SENTINEL))),
        ),
        (
            "revision",
            with(
                good.clone(),
                "/identity/function_revision",
                Some(json!(SENTINEL)),
            ),
        ),
        (
            "base digest",
            with(good.clone(), "/identity/base_digest", Some(json!(SENTINEL))),
        ),
        (
            "field",
            with(good.clone(), &format!("/{SENTINEL}"), Some(json!(SENTINEL))),
        ),
        ("logs", with(good.clone(), "/logs", Some(json!(SENTINEL)))),
    ] {
        let error = rejected(parse_result(&parsed, &bytes(&value)));
        for text in [error.to_string(), rendered(&error)] {
            assert!(!text.contains(SENTINEL), "{name}: {text}");
        }
    }
    let update = fixture("worker-execution-api/updates/checkpoint.json");
    for bad in [
        with(update.clone(), "/kind", Some(json!(SENTINEL))),
        with(update.clone(), &format!("/{SENTINEL}"), Some(json!(1))),
    ] {
        let error = rejected(parse_update(&bytes(&bad)));
        assert!(!rendered(&error).contains(SENTINEL));
        assert!(!error.to_string().contains(SENTINEL));
    }
    let cancel = fixture("worker-execution-api/cancel/uncertain.json");
    let error = rejected(parse_cancel_report(&bytes(&with(
        cancel,
        "/state",
        Some(json!(SENTINEL)),
    ))));
    assert!(!rendered(&error).contains(SENTINEL) && !error.to_string().contains(SENTINEL));
}

#[test]
fn successful_parses_do_not_print_request_fields_either() {
    let value = with(
        request_json("catalog"),
        "/params/token",
        Some(json!(SENTINEL)),
    );
    let parsed = request(&value);
    assert!(!rendered(&parsed).contains(SENTINEL));
    assert!(!rendered(&parsed.credential).contains(SENTINEL));
    let result = parse_result(&parsed, &bytes(&success(&value))).unwrap();
    assert!(!rendered(&result).contains(SENTINEL));
    let sentinel_logs = with(success(&value), "/logs", Some(json!([SENTINEL])));
    let result = parse_result(&parsed, &bytes(&sentinel_logs)).unwrap();
    assert!(!rendered(&result).contains(SENTINEL));
}
