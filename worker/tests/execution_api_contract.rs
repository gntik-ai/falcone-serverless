//! Worker execution API v1 conformance. Nothing here executes guest code: requests, results,
//! updates and cancellations are parsed, and each fixture is also validated against the full
//! published JSON Schemas (all `$ref`s resolved offline, the vendored falcone schemas
//! registered under their `$id`). JSON Schema evaluation is test-only; the runtime parsers
//! use serde types and explicit checks.

#[path = "../src/api/mod.rs"]
mod api;

use api::{
    error::{ApiError, Class, FailureCode, Malformed, Reason},
    legacy::{
        LEGACY_BODY_LIMIT_BYTES, LegacyRejection, admit_legacy, parse_legacy_case,
        parse_legacy_invalid,
    },
    request::{ExecuteRequest, parse_request},
    result::{Answered, ExecutionResult, parse_result},
    update::{parse_cancel, parse_update},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const CREDENTIAL_LIKE: &str = "CRED-9f3a7c21-hunter2-s3cr3t";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn contract_dir() -> PathBuf {
    repo().join("docs/contracts/worker-execution-api")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract")
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|_| panic!("read {}", path.display()))
}

fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&read(path)).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fixture(name: &str) -> (Vec<u8>, Value) {
    let bytes = read(&fixture_dir().join(name));
    let value = serde_json::from_slice(&bytes).unwrap();
    (bytes, value)
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn index() -> Vec<Value> {
    json_file(&fixture_dir().join("index.json"))
        .as_array()
        .unwrap()
        .clone()
}

// ---- JSON Schema conformance (test-only) -------------------------------------------------

struct Schemas {
    registry: jsonschema::Registry<'static>,
}

impl Schemas {
    /// Every schema in the contract directory, the vendored falcone schemas and the revision
    /// runtime manifest (the `@0.2.0` grant definitions), registered by `$id`: no network.
    fn load() -> Self {
        let mut builder = jsonschema::Registry::new();
        let manifest = repo().join("docs/contracts/revision-runtime-manifest.schema.json");
        let mut files = vec![manifest];
        for dir in [contract_dir(), contract_dir().join("vendor")] {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.to_string_lossy().ends_with(".schema.json") {
                    files.push(path);
                }
            }
        }
        for path in files {
            let schema = json_file(&path);
            let id = schema["$id"].as_str().unwrap().to_owned();
            builder = builder
                .add(id, jsonschema::Resource::from_contents(schema))
                .unwrap();
        }
        Self {
            registry: builder.prepare().unwrap(),
        }
    }

    fn validator(&self, file: &str) -> jsonschema::Validator {
        let schema = json_file(&contract_dir().join(file));
        jsonschema::options()
            .should_validate_formats(true)
            .with_registry(&self.registry)
            .build(&schema)
            .unwrap()
    }

    fn vendored(&self, file: &str) -> jsonschema::Validator {
        let schema = json_file(&contract_dir().join("vendor").join(file));
        jsonschema::options()
            .should_validate_formats(true)
            .with_registry(&self.registry)
            .build(&schema)
            .unwrap()
    }
}

fn schema_file(kind: &str) -> &'static str {
    match kind {
        "request" => "execute-request.v1.schema.json",
        "result" => "execution-result.v1.schema.json",
        "update" => "execution-update.v1.schema.json",
        "cancel" => "cancel.v1.schema.json",
        other => panic!("{other}"),
    }
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Accept,
    Refuse,
    Malformed,
}

fn parse_outcome(kind: &str, input: &[u8], request: Option<&dyn Answered>) -> Outcome {
    let malformed = |result: Result<(), Malformed>| match result {
        Ok(()) => Outcome::Accept,
        Err(_) => Outcome::Malformed,
    };
    match kind {
        "request" => match parse_request(input) {
            Ok(_) => Outcome::Accept,
            Err(ApiError::Refused(_)) => Outcome::Refuse,
            Err(ApiError::Malformed(_)) => Outcome::Malformed,
        },
        "result" => malformed(parse_result(input, request.unwrap()).map(drop)),
        "update" => malformed(parse_update(input).map(drop)),
        "cancel" => malformed(parse_cancel(input).map(drop)),
        other => panic!("{other}"),
    }
}

fn request_for(file: &str) -> ExecuteRequest {
    parse_request(&fixture(file).0).unwrap()
}

/// The context a result answers: the request, or for a refused request its projection.
fn context_for(file: &str) -> Box<dyn Answered> {
    match parse_request(&fixture(file).0) {
        Ok(request) => Box::new(request),
        Err(ApiError::Refused(_)) => {
            let projection = fixture(file).1["projection"].clone();
            Box::new(serde_json::from_value::<api::types::Projection>(projection).unwrap())
        }
        Err(other) => panic!("{file}: {other}"),
    }
}

fn outcome_name(text: &str) -> Outcome {
    match text {
        "accept" => Outcome::Accept,
        "refuse" => Outcome::Refuse,
        "malformed" => Outcome::Malformed,
        other => panic!("{other}"),
    }
}

/// The parser accepts exactly what the schema accepts, except that a schema-valid request is
/// refused when it is for a runtime mode other than `wasm` (the worker serves only `wasm`) or
/// when its `params` are a legacy input rejected before invocation.
fn expected_outcome(schema_valid: bool, value: &Value) -> Outcome {
    let rejected_params = value
        .get("params")
        .and_then(Value::as_object)
        .is_some_and(|params| admit_legacy(params, |_| ()).is_err());
    match (
        schema_valid,
        value
            .pointer("/projection/runtime_mode")
            .and_then(Value::as_str),
    ) {
        (false, _) => Outcome::Malformed,
        (true, Some("oci")) => Outcome::Refuse,
        (true, _) if rejected_params => Outcome::Refuse,
        (true, _) => Outcome::Accept,
    }
}

#[test]
fn every_fixture_matches_its_schema_verdict_and_the_runtime_parser() {
    let schemas = Schemas::load();
    let entries = index();
    assert!(entries.len() > 50);
    for entry in entries {
        let (kind, file) = (
            entry["kind"].as_str().unwrap(),
            entry["file"].as_str().unwrap(),
        );
        let (raw, value) = fixture(file);
        let schema_valid = schemas.validator(schema_file(kind)).is_valid(&value);
        assert_eq!(
            schema_valid,
            entry["schema"].as_bool().unwrap(),
            "schema verdict: {file}"
        );
        let request = entry["request"].as_str().map(context_for);
        let got = parse_outcome(kind, &raw, request.as_deref());
        assert_eq!(
            got,
            outcome_name(entry["parser"].as_str().unwrap()),
            "parser verdict: {file}"
        );
        // Parity. Cross-document rules (identity against the request, ids) are parser-only and
        // are the schema-valid, parser-malformed result fixtures.
        if kind == "result" && schema_valid {
            assert!(
                matches!(got, Outcome::Accept | Outcome::Malformed),
                "{file}"
            );
        } else {
            assert_eq!(
                got,
                expected_outcome(schema_valid, &value),
                "parity: {file}"
            );
        }
    }
}

/// A JSON number written the way a producer might: `1.0`, `1e0`.
fn number(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn set(value: &mut Value, pointer: &str, new: Value) {
    *value
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("{pointer}")) = new;
}

fn remove(value: &mut Value, pointer: &str) {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    let parent = if parent.is_empty() {
        &mut *value
    } else {
        value.pointer_mut(parent).unwrap()
    };
    match parent {
        Value::Object(map) => assert!(map.remove(key).is_some(), "{pointer}"),
        Value::Array(items) => drop(items.remove(key.parse().unwrap())),
        _ => panic!("{pointer}"),
    }
}

fn parity(kind: &str, base: &Value, request: Option<&dyn Answered>, mutations: &[(String, Value)]) {
    let schemas = Schemas::load();
    let validator = schemas.validator(schema_file(kind));
    assert!(validator.is_valid(base));
    for (name, value) in mutations {
        let schema_valid = validator.is_valid(value);
        let got = parse_outcome(kind, &bytes(value), request);
        if name.starts_with("parser-only") {
            // Schema-valid on its own; only the parser knows what the result must repeat.
            assert!(schema_valid, "{name}");
            assert_eq!(got, Outcome::Malformed, "{name}");
            continue;
        }
        let expected = if kind == "request" {
            expected_outcome(schema_valid, value)
        } else if schema_valid {
            Outcome::Accept
        } else {
            Outcome::Malformed
        };
        assert_eq!(
            got, expected,
            "parity for mutation {name}: schema valid = {schema_valid}"
        );
    }
}

#[test]
fn request_parser_matches_the_schema_on_mutations() {
    let (_, base) = fixture("request/valid/wasm-catalog-base.json");
    let mut cases: Vec<(String, Value)> = Vec::new();
    for field in base.as_object().unwrap().keys() {
        let mut missing = base.clone();
        remove(&mut missing, &format!("/{field}"));
        cases.push((format!("remove /{field}"), missing));
        let mut null = base.clone();
        set(&mut null, &format!("/{field}"), Value::Null);
        cases.push((format!("null /{field}"), null));
    }
    for field in base["projection"].as_object().unwrap().keys() {
        let mut missing = base.clone();
        remove(&mut missing, &format!("/projection/{field}"));
        cases.push((format!("remove /projection/{field}"), missing));
        let mut null = base.clone();
        set(&mut null, &format!("/projection/{field}"), Value::Null);
        cases.push((format!("null /projection/{field}"), null));
    }
    for field in base["credential"].as_object().unwrap().keys() {
        let mut missing = base.clone();
        remove(&mut missing, &format!("/credential/{field}"));
        cases.push((format!("remove /credential/{field}"), missing));
    }
    let long = "x".repeat(300);
    let digest = "sha256:".to_owned() + &"1".repeat(64);
    let edits: Vec<(&str, &str, Value)> = vec![
        ("contract_version 2", "/contract_version", json!("2")),
        ("contract_version number", "/contract_version", json!(1)),
        ("attempt_number 0", "/attempt_number", json!(0)),
        ("attempt_number -1", "/attempt_number", json!(-1)),
        ("attempt_number string", "/attempt_number", json!("1")),
        ("attempt_number 7", "/attempt_number", json!(7)),
        ("attempt_number 1.0", "/attempt_number", json!(1.0)),
        ("attempt_number 1e0", "/attempt_number", number("1e0")),
        ("attempt_number 1.5", "/attempt_number", json!(1.5)),
        ("attempt_number 0.0", "/attempt_number", json!(0.0)),
        (
            "attempt_number max",
            "/attempt_number",
            json!(4_294_967_295u64),
        ),
        (
            "attempt_number max.0",
            "/attempt_number",
            json!(4_294_967_295.0),
        ),
        (
            "attempt_number max+1",
            "/attempt_number",
            json!(4_294_967_296u64),
        ),
        (
            "attempt_number max+1.0",
            "/attempt_number",
            json!(4_294_967_296.0),
        ),
        ("empty activation", "/activation_id", json!("")),
        ("long activation", "/activation_id", json!(long)),
        (
            "deadline offset",
            "/deadline",
            json!("2026-11-03T00:00:30+01:00"),
        ),
        ("deadline date only", "/deadline", json!("2026-11-03")),
        (
            "deadline month 13",
            "/deadline",
            json!("2026-13-01T00:00:00Z"),
        ),
        (
            "deadline hour 24",
            "/deadline",
            json!("2026-11-03T24:00:00Z"),
        ),
        (
            "deadline fraction",
            "/deadline",
            json!("2026-11-03T00:00:30.25Z"),
        ),
        (
            "deadline leap day",
            "/deadline",
            json!("2028-02-29T00:00:00Z"),
        ),
        (
            "deadline not leap",
            "/deadline",
            json!("2027-02-29T00:00:00Z"),
        ),
        (
            "deadline april 31",
            "/deadline",
            json!("2026-04-31T00:00:00Z"),
        ),
        (
            "deadline lowercase",
            "/deadline",
            json!("2026-11-03t00:00:30z"),
        ),
        ("params array", "/params", json!([])),
        ("params empty", "/params", json!({})),
        ("mode oci", "/projection/runtime_mode", json!("oci")),
        ("mode OCI", "/projection/runtime_mode", json!("OCI")),
        ("mode native", "/projection/runtime_mode", json!("native")),
        ("mode number", "/projection/runtime_mode", json!(1)),
        (
            "artifact digest upper",
            "/projection/artifact_digest",
            json!("sha256:".to_owned() + &"A".repeat(64)),
        ),
        (
            "artifact digest short",
            "/projection/artifact_digest",
            json!("sha256:abc"),
        ),
        (
            "component digest",
            "/projection/component_digest",
            json!(digest),
        ),
        ("future member", "/projection/future", json!({"a": 1})),
        ("budget zero", "/projection/budgets/fuel", json!(0)),
        ("budget 1.0", "/projection/budgets/fuel", json!(1.0)),
        ("budget 0.5", "/projection/budgets/fuel", json!(0.5)),
        (
            "budget 2e3",
            "/projection/budgets/timeout_ms",
            number("2e3"),
        ),
        (
            "budget safe max",
            "/projection/budgets/fuel",
            json!(9_007_199_254_740_991u64),
        ),
        (
            "budget safe max.0",
            "/projection/budgets/fuel",
            json!(9_007_199_254_740_991.0),
        ),
        (
            "budget over",
            "/projection/budgets/fuel",
            json!(9_007_199_254_740_992u64),
        ),
        (
            "budget over.0",
            "/projection/budgets/memory_bytes",
            json!(9_007_199_254_740_992.0),
        ),
        (
            "budget u64 max",
            "/projection/budgets/fuel",
            json!(u64::MAX),
        ),
        (
            "budget 1e30",
            "/projection/budgets/max_host_calls",
            number("1e30"),
        ),
        ("budget string", "/projection/budgets/fuel", json!("1")),
        ("budget extra", "/projection/budgets/extra", json!(1)),
        (
            "profile extra",
            "/projection/language_profile/extra",
            json!(1),
        ),
        ("import empty", "/projection/declared_imports", json!([""])),
        (
            "imports ok",
            "/projection/declared_imports",
            json!(["falcone:host/http"]),
        ),
        (
            "base alias",
            "/projection/base",
            json!({"kind": "alias", "alias": "default"}),
        ),
        ("base string", "/projection/base", json!("default")),
        (
            "base version default",
            "/projection/base/version",
            json!("default"),
        ),
        (
            "base version latest",
            "/projection/base/version",
            json!("latest"),
        ),
        (
            "base version digit-led",
            "/projection/base/version",
            json!("2"),
        ),
        ("base id default", "/projection/base/id", json!("Default")),
        ("base id with at", "/projection/base/id", json!("a@b")),
        ("base extra", "/projection/base/extra", json!(1)),
        (
            "base digest bad",
            "/projection/base/digest",
            json!("sha256:zz"),
        ),
        ("base mixed kind", "/projection/base/kind", json!("byo")),
        (
            "grant 0.1.0",
            "/projection/granted_capabilities/0/interface",
            json!("falcone:host/http@0.1.0"),
        ),
        (
            "grant 0.3.0",
            "/projection/granted_capabilities/0/interface",
            json!("falcone:host/http@0.3.0"),
        ),
        (
            "grant wrong family interface",
            "/projection/granted_capabilities/0/interface",
            json!("falcone:host/storage@0.2.0"),
        ),
        (
            "grant wildcard",
            "/projection/granted_capabilities/0/scope/hosts",
            json!(["*"]),
        ),
        (
            "grant partial wildcard",
            "/projection/granted_capabilities/0/scope/hosts",
            json!(["*.example.test"]),
        ),
        (
            "grant empty hosts",
            "/projection/granted_capabilities/0/scope/hosts",
            json!([]),
        ),
        (
            "grant empty host",
            "/projection/granted_capabilities/0/scope/hosts",
            json!([""]),
        ),
        (
            "grant wrong scope",
            "/projection/granted_capabilities/0/scope",
            json!({"prefixes": ["a/"]}),
        ),
        (
            "grant two scope fields",
            "/projection/granted_capabilities/0/scope",
            json!({"hosts": ["a"], "prefixes": ["b"]}),
        ),
        (
            "grant empty scope",
            "/projection/granted_capabilities/0/scope",
            json!({}),
        ),
        (
            "grant extra",
            "/projection/granted_capabilities/0/extra",
            json!(1),
        ),
        (
            "grant family unknown",
            "/projection/granted_capabilities/0/family",
            json!("network"),
        ),
        (
            "grant secretRef",
            "/projection/granted_capabilities/0/secretRef",
            json!({"name": "db", "version": "3"}),
        ),
        (
            "grant secretRef null version",
            "/projection/granted_capabilities/0/secretRef",
            json!({"name": "db", "version": null}),
        ),
        (
            "grant secretRef extra",
            "/projection/granted_capabilities/0/secretRef",
            json!({"name": "db", "value": "x"}),
        ),
        (
            "grant secretRef empty name",
            "/projection/granted_capabilities/0/secretRef",
            json!({"name": ""}),
        ),
        (
            "grant guestGrantId",
            "/projection/granted_capabilities/0/guestGrantId",
            json!("g-1"),
        ),
        (
            "grant guestGrantId empty",
            "/projection/granted_capabilities/0/guestGrantId",
            json!(""),
        ),
        (
            "grant guestGrantId null",
            "/projection/granted_capabilities/0/guestGrantId",
            json!(null),
        ),
        (
            "grants empty",
            "/projection/granted_capabilities",
            json!([]),
        ),
        (
            "credential grant 0.2.0",
            "/credential/allowed_capabilities/0/interface",
            json!("falcone:host/http@0.2.0"),
        ),
        (
            "credential no grants",
            "/credential/allowed_capabilities",
            json!([]),
        ),
        ("credential iat negative", "/credential/iat", json!(-1)),
        ("credential iat fraction", "/credential/iat", json!(1.5)),
        ("credential iat string", "/credential/iat", json!("1")),
        ("credential extra", "/credential/extra", json!(1)),
        ("credential long iss", "/credential/iss", json!(long)),
        ("credential empty iss", "/credential/iss", json!("")),
    ];
    for (name, pointer, new) in edits {
        let mut value = base.clone();
        let mut parts = pointer.rsplitn(2, '/');
        let (key, parent) = (parts.next().unwrap(), parts.next().unwrap());
        match value
            .pointer_mut(parent)
            .unwrap_or_else(|| panic!("{pointer}"))
        {
            Value::Object(map) => drop(map.insert(key.to_owned(), new)),
            Value::Array(_) => set(&mut value, pointer, new),
            _ => panic!("{pointer}"),
        }
        cases.push((name.to_owned(), value));
    }
    let mut no_base = base.clone();
    remove(&mut no_base, "/projection/base");
    cases.push(("no base".to_owned(), no_base));
    parity("request", &base, None, &cases);
}

#[test]
fn result_parser_matches_the_schema_on_mutations() {
    let (_, success) = fixture("result/valid/success-catalog.json");
    let (_, failure) = fixture("result/valid/failure-gw-oci-not-authorized.json");
    let request = request_for("request/valid/wasm-catalog-base.json");
    let digest = "sha256:".to_owned() + &"9".repeat(64);
    let mut cases = Vec::new();
    for (label, base) in [("success", &success), ("failure", &failure)] {
        for field in base.as_object().unwrap().keys() {
            let mut value = base.clone();
            remove(&mut value, &format!("/{field}"));
            cases.push((format!("{label} remove /{field}"), value));
            let mut value = base.clone();
            set(&mut value, &format!("/{field}"), Value::Null);
            cases.push((format!("{label} null /{field}"), value));
        }
        for field in base["identity"].as_object().unwrap().keys() {
            let mut value = base.clone();
            remove(&mut value, &format!("/identity/{field}"));
            // Removing base_digest is valid for the schema alone; only the parser knows a base is pinned.
            if field != "base_digest" {
                cases.push((format!("{label} remove /identity/{field}"), value));
            }
            let mut value = base.clone();
            set(&mut value, &format!("/identity/{field}"), Value::Null);
            cases.push((format!("{label} null /identity/{field}"), value));
        }
    }
    let edits: Vec<(&str, Value, &str, Value)> = vec![
        (
            "status unknown",
            success.clone(),
            "/status",
            json!("partial"),
        ),
        ("logs not array", success.clone(), "/logs", json!("x")),
        ("log not string", success.clone(), "/logs", json!([1])),
        (
            "logs many",
            success.clone(),
            "/logs",
            json!(vec!["l"; 1001]),
        ),
        (
            "logs ok many",
            success.clone(),
            "/logs",
            json!(vec!["l"; 1000]),
        ),
        (
            "log long",
            success.clone(),
            "/logs",
            json!(["x".repeat(4097)]),
        ),
        (
            "result status 200.0",
            success.clone(),
            "/result/statusCode",
            json!(200.0),
        ),
        (
            "result status 599.0",
            success.clone(),
            "/result/statusCode",
            json!(599.0),
        ),
        (
            "result status 600.0",
            success.clone(),
            "/result/statusCode",
            json!(600.0),
        ),
        (
            "result status 200.5",
            success.clone(),
            "/result/statusCode",
            json!(200.5),
        ),
        (
            "result status 99",
            success.clone(),
            "/result/statusCode",
            json!(99),
        ),
        (
            "result status 600",
            success.clone(),
            "/result/statusCode",
            json!(600),
        ),
        (
            "result status 304",
            success.clone(),
            "/result/statusCode",
            json!(304),
        ),
        ("result extra", success.clone(), "/result/extra", json!(1)),
        (
            "result header number",
            success.clone(),
            "/result/headers",
            json!({"a": 1}),
        ),
        (
            "result body array",
            success.clone(),
            "/result/body",
            json!([1]),
        ),
        (
            "result body null",
            success.clone(),
            "/result/body",
            json!(null),
        ),
        (
            "parser-only identity runtime oci",
            success.clone(),
            "/identity/runtime_mode",
            json!("oci"),
        ),
        (
            "parser-only identity base digest",
            success.clone(),
            "/identity/base_digest",
            json!(digest),
        ),
        (
            "identity extra",
            success.clone(),
            "/identity/extra",
            json!(1),
        ),
        ("error status", failure.clone(), "/error/status", json!(409)),
        (
            "error code",
            failure.clone(),
            "/error/code",
            json!("GW_BASE_DISABLED"),
        ),
        (
            "error code unknown",
            failure.clone(),
            "/error/code",
            json!("GW_SOMETHING"),
        ),
        (
            "error class retryable",
            failure.clone(),
            "/error/class",
            json!("retryable"),
        ),
        (
            "error class uncertain",
            failure.clone(),
            "/error/class",
            json!("uncertain"),
        ),
        (
            "error retryable",
            failure.clone(),
            "/error/retryable",
            json!(true),
        ),
        (
            "error reason",
            failure.clone(),
            "/error/detail/reason",
            json!("base_disabled"),
        ),
        (
            "error detail extra",
            failure.clone(),
            "/error/detail/extra",
            json!(1),
        ),
        (
            "error message empty",
            failure.clone(),
            "/error/message",
            json!(""),
        ),
        ("error extra", failure.clone(), "/error/extra", json!(1)),
    ];
    for (name, base, pointer, new) in edits {
        let mut value = base;
        let mut parts = pointer.rsplitn(2, '/');
        let (key, parent) = (parts.next().unwrap(), parts.next().unwrap());
        match value.pointer_mut(parent).unwrap() {
            Value::Object(map) => drop(map.insert(key.to_owned(), new)),
            _ => panic!("{pointer}"),
        }
        cases.push((name.to_owned(), value));
    }
    let mut both = success.clone();
    both["error"] = failure["error"].clone();
    cases.push(("result and error".to_owned(), both));
    let mut neither = success.clone();
    remove(&mut neither, "/result");
    cases.push(("neither".to_owned(), neither));
    let mut class_on_success = success.clone();
    class_on_success["class"] = json!("non_retryable");
    cases.push(("top-level class".to_owned(), class_on_success));
    parity("result", &success, Some(&request), &cases);
}

#[test]
fn update_and_cancel_parsers_match_the_schema_on_mutations() {
    let ids = json!({"contract_version": "1", "activation_id": "a", "attempt_id": "b"});
    let with = |extra: Value| {
        let mut value = ids.clone();
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value
    };
    let digest = "sha256:".to_owned() + &"1".repeat(64);
    let updates = vec![
        (
            "heartbeat",
            with(json!({"sequence": 1, "kind": "heartbeat"})),
        ),
        (
            "heartbeat percent",
            with(json!({"sequence": 1, "kind": "heartbeat", "percent": 5})),
        ),
        (
            "heartbeat message",
            with(json!({"sequence": 1, "kind": "heartbeat", "message": "m"})),
        ),
        ("progress", with(json!({"sequence": 1, "kind": "progress"}))),
        (
            "progress full",
            with(json!({"sequence": 1, "kind": "progress", "percent": 100, "message": "m"})),
        ),
        (
            "progress 101",
            with(json!({"sequence": 1, "kind": "progress", "percent": 101})),
        ),
        (
            "progress -1",
            with(json!({"sequence": 1, "kind": "progress", "percent": -1})),
        ),
        (
            "progress null percent",
            with(json!({"sequence": 1, "kind": "progress", "percent": null})),
        ),
        (
            "progress null message",
            with(json!({"sequence": 1, "kind": "progress", "message": null})),
        ),
        (
            "progress empty message",
            with(json!({"sequence": 1, "kind": "progress", "message": ""})),
        ),
        (
            "progress long message",
            with(json!({"sequence": 1, "kind": "progress", "message": "x".repeat(257)})),
        ),
        (
            "progress digest",
            with(json!({"sequence": 1, "kind": "progress", "digest": digest})),
        ),
        (
            "checkpoint",
            with(
                json!({"sequence": 1, "kind": "checkpoint", "checkpoint_ref": "c", "digest": digest}),
            ),
        ),
        (
            "checkpoint no digest",
            with(json!({"sequence": 1, "kind": "checkpoint", "checkpoint_ref": "c"})),
        ),
        (
            "checkpoint no ref",
            with(json!({"sequence": 1, "kind": "checkpoint", "digest": digest})),
        ),
        (
            "checkpoint null ref",
            with(
                json!({"sequence": 1, "kind": "checkpoint", "checkpoint_ref": null, "digest": digest}),
            ),
        ),
        (
            "checkpoint percent",
            with(
                json!({"sequence": 1, "kind": "checkpoint", "checkpoint_ref": "c", "digest": digest, "percent": 1}),
            ),
        ),
        (
            "sequence 0",
            with(json!({"sequence": 0, "kind": "heartbeat"})),
        ),
        (
            "sequence 1.0",
            with(json!({"sequence": 1.0, "kind": "heartbeat"})),
        ),
        (
            "sequence 1.5",
            with(json!({"sequence": 1.5, "kind": "heartbeat"})),
        ),
        (
            "sequence safe max",
            with(json!({"sequence": 9_007_199_254_740_991u64, "kind": "heartbeat"})),
        ),
        (
            "sequence over",
            with(json!({"sequence": 9_007_199_254_740_992u64, "kind": "heartbeat"})),
        ),
        (
            "sequence over.0",
            with(json!({"sequence": 9_007_199_254_740_992.0, "kind": "heartbeat"})),
        ),
        (
            "sequence 1e30",
            with(json!({"sequence": number("1e30"), "kind": "heartbeat"})),
        ),
        (
            "progress 100.0",
            with(json!({"sequence": 1, "kind": "progress", "percent": 100.0})),
        ),
        (
            "progress 100.5",
            with(json!({"sequence": 1, "kind": "progress", "percent": 100.5})),
        ),
        (
            "progress -0.0",
            with(json!({"sequence": 1, "kind": "progress", "percent": -0.0})),
        ),
        (
            "progress 1e2",
            with(json!({"sequence": 1, "kind": "progress", "percent": number("1e2")})),
        ),
        ("sequence missing", with(json!({"kind": "heartbeat"}))),
        ("kind unknown", with(json!({"sequence": 1, "kind": "log"}))),
        ("kind missing", with(json!({"sequence": 1}))),
        (
            "extra",
            with(json!({"sequence": 1, "kind": "heartbeat", "extra": 1})),
        ),
        ("version 2", {
            let mut value = with(json!({"sequence": 1, "kind": "heartbeat"}));
            value["contract_version"] = json!("2");
            value
        }),
    ];
    let schemas = Schemas::load();
    for (kind, items) in [("update", updates), ("cancel", cancels(&ids))] {
        let validator = schemas.validator(schema_file(kind));
        for (name, value) in items {
            let valid = validator.is_valid(&value);
            let expected = if valid {
                Outcome::Accept
            } else {
                Outcome::Malformed
            };
            assert_eq!(
                parse_outcome(kind, &bytes(&value), None),
                expected,
                "{kind} {name}"
            );
        }
    }
}

fn cancels(ids: &Value) -> Vec<(&'static str, Value)> {
    let with = |extra: Value| {
        let mut value = ids.clone();
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value
    };
    let mut no_attempt = with(json!({"state": "requested"}));
    no_attempt.as_object_mut().unwrap().remove("attempt_id");
    vec![
        ("requested", with(json!({"state": "requested"}))),
        ("acknowledged", with(json!({"state": "acknowledged"}))),
        ("uncertain", with(json!({"state": "uncertain"}))),
        ("unknown state", with(json!({"state": "done"}))),
        ("null state", with(json!({"state": null}))),
        ("extra", with(json!({"state": "requested", "extra": 1}))),
        ("missing attempt", no_attempt),
    ]
}

// ---- runtime_mode and refusal -------------------------------------------------------------

fn refusal_of(file: &str) -> Box<api::error::Refusal> {
    match parse_request(&fixture(file).0) {
        Err(ApiError::Refused(refusal)) => refusal,
        other => panic!("expected refusal for {file}, got {other:?}"),
    }
}

#[test]
fn runtime_mode_is_required_and_anything_but_wasm_is_refused() {
    let schema = json_file(&contract_dir().join("execute-request.v1.schema.json"));
    let required = &schema["$defs"]["projection"]["required"];
    assert!(
        required
            .as_array()
            .unwrap()
            .contains(&json!("runtime_mode"))
    );
    let common = json_file(&contract_dir().join("common.v1.schema.json"));
    assert_eq!(
        common["$defs"]["runtimeMode"]["enum"],
        json!(["wasm", "oci"])
    );
    assert!(matches!(
        parse_request(&fixture("request/invalid/runtime-mode-missing.json").0),
        Err(ApiError::Malformed(_))
    ));
    for file in [
        "request/refused/oci-catalog-base.json",
        "request/refused/oci-byo-base.json",
    ] {
        let refusal = refusal_of(file);
        assert_eq!(refusal.code(), FailureCode::RuntimeModeUnsupported);
        let result = serde_json::to_value(refusal.result()).unwrap();
        assert_eq!(result["status"], "failure");
        assert!(result.get("result").is_none());
        let error = &result["error"];
        assert_eq!(error["code"], "runtime_mode_unsupported");
        assert_eq!(error["status"], 409);
        assert_eq!(
            error["detail"],
            json!({"reason": "runtime_mode_unsupported"})
        );
        assert_eq!(error["retryable"], false);
        assert_eq!(error["class"], "non_retryable");
        assert_eq!(result["identity"]["runtime_mode"], "oci");
        // The refusal is the published refusal fixture, and its own result validates.
        assert!(
            Schemas::load()
                .validator("execution-result.v1.schema.json")
                .is_valid(&result)
        );
    }
    let (_, expected) = fixture("result/valid/failure-runtime-mode-unsupported.json");
    let result =
        serde_json::to_value(refusal_of("request/refused/oci-catalog-base.json").result()).unwrap();
    assert_eq!(result["error"], expected["error"]);
    assert_eq!(result["identity"], expected["identity"]);
    // A value outside the enum is malformed, and nothing is executed either way.
    assert!(matches!(
        parse_request(&fixture("request/invalid/runtime-mode-native.json").0),
        Err(ApiError::Malformed(_))
    ));
}

// ---- base union -----------------------------------------------------------------------

#[test]
fn catalog_byo_and_absent_bases_round_trip_and_aliases_are_malformed() {
    for (file, kind) in [
        ("request/valid/wasm-catalog-base.json", Some("catalog")),
        ("request/valid/wasm-byo-base.json", Some("byo")),
        ("request/valid/wasm-no-base.json", None),
    ] {
        let (raw, value) = fixture(file);
        let request = parse_request(&raw).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), value, "{file}");
        assert_eq!(value["projection"]["base"]["kind"].as_str(), kind);
        assert_eq!(value["projection"].get("base").is_some(), kind.is_some());
    }
    for file in [
        "request/invalid/base-alias-kind.json",
        "request/invalid/base-alias-version.json",
        "request/invalid/base-alias-latest.json",
        "request/invalid/base-null.json",
        "request/invalid/base-catalog-without-digest.json",
        "request/invalid/base-byo-without-signer.json",
    ] {
        assert!(
            matches!(parse_request(&fixture(file).0), Err(ApiError::Malformed(_))),
            "{file}"
        );
    }
    // Unknown projection members are ignored, so later revisions stay additive.
    assert!(parse_request(&fixture("request/valid/wasm-component-digest.json").0).is_ok());
}

// ---- failure codes ----------------------------------------------------------------------

fn spec_table() -> BTreeMap<String, (u16, Vec<String>, String, String)> {
    let spec = String::from_utf8(read(&contract_dir().join("execution-api.md"))).unwrap();
    let section = spec
        .split("## Failure codes")
        .nth(1)
        .expect("failure codes section");
    let section = section.split("\n## ").next().unwrap();
    let mut table = BTreeMap::new();
    for line in section.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<_> = line.trim_matches('|').split('|').map(str::trim).collect();
        let tick = |cell: &str| cell.trim_matches('`').to_owned();
        let reasons = cells[2].split(" or ").map(tick).collect();
        table.insert(
            tick(cells[0]),
            (
                cells[1].parse().unwrap(),
                reasons,
                cells[3].to_owned(),
                cells[4].to_owned(),
            ),
        );
    }
    table
}

#[test]
fn failure_codes_appear_in_the_spec_and_fixtures_with_their_class() {
    let table = spec_table();
    let codes: BTreeSet<_> = FailureCode::ALL
        .iter()
        .map(|c| c.as_str().to_owned())
        .collect();
    assert_eq!(table.keys().cloned().collect::<BTreeSet<_>>(), codes);
    for code in FailureCode::ALL {
        let (status, reasons, class, origin) = &table[code.as_str()];
        assert_eq!(*status, code.status());
        let expected: Vec<_> = code
            .reasons()
            .iter()
            .map(|r| {
                serde_json::to_value(r)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(reasons, &expected);
        assert_eq!(class, "non_retryable");
        assert_eq!(code.class(), Class::NonRetryable);
        assert_eq!(
            origin == "worker-only",
            code.worker_only(),
            "{}",
            code.as_str()
        );
    }
    // Each code has a result fixture for each of its reasons.
    let mut seen = BTreeSet::new();
    for entry in index().iter().filter(|e| {
        e["file"]
            .as_str()
            .unwrap()
            .starts_with("result/valid/failure-")
    }) {
        let (_, value) = fixture(entry["file"].as_str().unwrap());
        seen.insert(value["error"]["code"].as_str().unwrap().to_owned());
        assert_eq!(value["error"]["class"], "non_retryable");
        assert_eq!(value["error"]["retryable"], false);
    }
    assert_eq!(seen, codes);
}

#[test]
fn oci_not_authorized_and_base_disabled_are_non_retryable_on_every_attempt() {
    for attempt in 1..=5u64 {
        let (_, mut request_value) = fixture("request/valid/wasm-catalog-base.json");
        request_value["attempt_number"] = json!(attempt);
        let request = parse_request(&bytes(&request_value)).unwrap();
        for (reason, code) in [
            (Reason::OciNotAuthorized, "GW_OCI_NOT_AUTHORIZED"),
            (Reason::BaseDisabled, "GW_BASE_DISABLED"),
            (Reason::BaseMismatch, "GW_BASE_MISMATCH"),
        ] {
            let failure = serde_json::to_value(ExecutionResult::failure(&request, reason)).unwrap();
            assert_eq!(failure["error"]["code"], code);
            assert_eq!(failure["error"]["class"], "non_retryable");
            assert_eq!(failure["error"]["retryable"], false);
            assert!(parse_result(&bytes(&failure), &request).is_ok());
            // A retry never turns them retryable or uncertain, however the result claims it.
            for (class, retryable) in [
                ("retryable", true),
                ("uncertain", false),
                ("uncertain", true),
                ("retryable", false),
            ] {
                let mut forged = failure.clone();
                forged["error"]["class"] = json!(class);
                forged["error"]["retryable"] = json!(retryable);
                assert!(
                    parse_result(&bytes(&forged), &request).is_err(),
                    "{code} {class} {retryable}"
                );
            }
        }
        // The refusal of a non-wasm revision on a retry is non-retryable too.
        let mut oci = request_value.clone();
        oci["projection"]["runtime_mode"] = json!("oci");
        let Err(ApiError::Refused(refusal)) = parse_request(&bytes(&oci)) else {
            panic!()
        };
        let result = serde_json::to_value(refusal.result()).unwrap();
        assert_eq!(result["error"]["class"], "non_retryable");
    }
}

const FUNCTIONS_API_ERROR_FIXTURES: [(&str, &str); 3] = [
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

#[test]
fn worker_codes_match_the_pinned_functions_api_error_fixtures() {
    let mut gateway = BTreeMap::new();
    for (name, digest) in FUNCTIONS_API_ERROR_FIXTURES {
        let bytes = read(&fixture_dir().join(format!("functions-api-errors/{name}.json")));
        assert_eq!(
            sha256(&bytes),
            digest,
            "{name} is a pinned copy of falcone a19f40ab"
        );
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        gateway.insert(value["code"].as_str().unwrap().to_owned(), value);
    }
    let mut worker_only = BTreeSet::new();
    for code in FailureCode::ALL {
        match gateway.get(code.as_str()) {
            Some(published) => {
                assert!(!code.worker_only());
                assert_eq!(published["status"], code.status());
                assert_eq!(published["retryable"], false);
                assert_eq!(published["message"], code.message());
                let reasons: Vec<_> = code
                    .reasons()
                    .iter()
                    .map(|r| serde_json::to_value(r).unwrap())
                    .collect();
                assert_eq!(reasons, [published["detail"]["reason"].clone()]);
            }
            None => {
                assert!(code.worker_only());
                worker_only.insert(code.as_str());
            }
        }
    }
    // Every published base and OCI code is in the worker contract, and only these three are not.
    assert_eq!(gateway.len(), 3);
    assert_eq!(
        worker_only,
        BTreeSet::from([
            "invalid_request",
            "request_too_large",
            "runtime_mode_unsupported"
        ])
    );
    for code in gateway.keys() {
        assert!(
            FailureCode::ALL.iter().any(|c| c.as_str() == code),
            "{code}"
        );
    }
    assert_eq!(FailureCode::InvalidRequest.status(), 400);
    assert_eq!(FailureCode::RequestTooLarge.status(), 413);
    assert!(
        FailureCode::ALL
            .iter()
            .all(|c| c.class() == Class::NonRetryable)
    );
}

// ---- legacy envelope fixtures ---------------------------------------------------------

fn inventory_hashes() -> BTreeMap<String, String> {
    let inventory = String::from_utf8(read(
        &repo().join("docs/contracts/falcone-contract-inventory.md"),
    ))
    .unwrap();
    inventory
        .lines()
        .filter(|line| line.starts_with("| docs/contracts/fixtures/legacy-envelope/"))
        .map(|line| {
            let cells: Vec<_> = line.trim_matches('|').split('|').map(str::trim).collect();
            (cells[0].to_owned(), cells[1].to_owned())
        })
        .collect()
}

#[test]
fn legacy_fixtures_are_byte_exact_and_valid_ones_round_trip() {
    let hashes = inventory_hashes();
    assert_eq!(hashes.len(), 11);
    let schemas = Schemas::load();
    let results = schemas.validator("execution-result.v1.schema.json");
    let request = request_for("request/valid/wasm-catalog-base.json");
    for (path, digest) in &hashes {
        let raw = read(&repo().join(path));
        assert_eq!(&sha256(&raw), digest, "{path}");
        if path.contains("/valid/") {
            let case = parse_legacy_case(&raw).unwrap();
            let original: Value = serde_json::from_slice(&raw).unwrap();
            assert_eq!(serde_json::to_value(&case).unwrap(), original, "{path}");
            // The function's result travels unchanged inside the worker result envelope.
            let result = ExecutionResult::success(&request, case.expected.clone(), Vec::new());
            let value = serde_json::to_value(&result).unwrap();
            assert_eq!(value["result"], original["expected"], "{path}");
            assert!(results.is_valid(&value), "{path}");
            assert_eq!(parse_result(&bytes(&value), &request).unwrap(), result);
        }
    }
    assert_eq!(hashes.keys().filter(|p| p.contains("/valid/")).count(), 8);
}

#[test]
fn valid_legacy_inputs_are_admitted_and_invoked() {
    for path in inventory_hashes().keys().filter(|p| p.contains("/valid/")) {
        let case = parse_legacy_case(&read(&repo().join(path))).unwrap();
        let params = case.params.get().as_object().unwrap();
        let mut invoked = 0;
        assert_eq!(admit_legacy(params, |_| invoked += 1), Ok(()), "{path}");
        assert_eq!(invoked, 1, "{path}");
    }
}

#[test]
fn invalid_legacy_inputs_are_rejected_with_their_code_and_never_invoked() {
    let request = request_for("request/valid/wasm-catalog-base.json");
    let schemas = Schemas::load();
    let results = schemas.validator("execution-result.v1.schema.json");
    for (name, rejection, code, status, reason) in [
        (
            "missing-path",
            LegacyRejection::MissingPath,
            "invalid_request",
            400,
            "missing_path",
        ),
        (
            "non-JSON-body",
            LegacyRejection::NonJsonBody,
            "invalid_request",
            400,
            "non_json_body",
        ),
        (
            "oversized-body",
            LegacyRejection::BodyTooLarge,
            "request_too_large",
            413,
            "body_too_large",
        ),
    ] {
        let path = repo().join(format!(
            "docs/contracts/fixtures/legacy-envelope/invalid/{name}.json"
        ));
        let case = parse_legacy_invalid(&read(&path)).unwrap();
        assert!(!case.expected.invoked, "{name}");
        let params = case.params.get().as_object().unwrap();
        let mut invoked = 0;
        let outcome = admit_legacy(params, |_| invoked += 1);
        assert_eq!(outcome, Err(rejection), "{name}");
        assert_eq!(invoked, 0, "{name}: nothing is invoked");
        let value = serde_json::to_value(rejection.result(&request)).unwrap();
        assert_eq!(value["error"]["code"], code, "{name}");
        assert_eq!(value["error"]["status"], status, "{name}");
        assert_eq!(value["error"]["detail"]["reason"], reason, "{name}");
        assert_eq!(value["error"]["retryable"], false, "{name}");
        assert_eq!(value["error"]["class"], "non_retryable", "{name}");
        assert!(results.is_valid(&value), "{name}");
        assert!(parse_result(&bytes(&value), &request).is_ok(), "{name}");
    }
    // The oversized fixture is exactly one byte over the limit and valid JSON.
    let case = parse_legacy_invalid(&read(
        &repo().join("docs/contracts/fixtures/legacy-envelope/invalid/oversized-body.json"),
    ))
    .unwrap();
    let body = case.params.get()["__ow_body"].as_str().unwrap();
    assert_eq!(body.len(), LEGACY_BODY_LIMIT_BYTES + 1);
    assert!(serde_json::from_str::<Value>(body).is_ok());
}

#[test]
fn execute_requests_apply_the_legacy_rejections_before_any_invocation() {
    let (_, base) = fixture("request/valid/wasm-catalog-base.json");
    let schemas = Schemas::load();
    let requests = schemas.validator("execute-request.v1.schema.json");
    let results = schemas.validator("execution-result.v1.schema.json");
    for (name, code, status, reason) in [
        ("missing-path", "invalid_request", 400, "missing_path"),
        ("non-JSON-body", "invalid_request", 400, "non_json_body"),
        ("oversized-body", "request_too_large", 413, "body_too_large"),
    ] {
        let path = repo().join(format!(
            "docs/contracts/fixtures/legacy-envelope/invalid/{name}.json"
        ));
        let case = parse_legacy_invalid(&read(&path)).unwrap();
        let mut value = base.clone();
        value["params"] = case.params.get().clone();
        // The schema takes any params object: the refusal is the parser's.
        assert!(requests.is_valid(&value), "{name}");
        let Err(ApiError::Refused(refusal)) = parse_request(&bytes(&value)) else {
            panic!("{name}: the execute parser accepted a rejected legacy input");
        };
        assert_eq!(refusal.code().as_str(), code, "{name}");
        let result = serde_json::to_value(refusal.result()).unwrap();
        assert_eq!(result["status"], "failure", "{name}");
        assert_eq!(result["error"]["code"], code, "{name}");
        assert_eq!(result["error"]["status"], status, "{name}");
        assert_eq!(result["error"]["detail"]["reason"], reason, "{name}");
        assert_eq!(result["error"]["retryable"], false, "{name}");
        assert_eq!(result["error"]["class"], "non_retryable", "{name}");
        assert!(results.is_valid(&result), "{name}");
        assert!(
            result.get("result").is_none(),
            "{name}: nothing was invoked"
        );
        // A non-wasm revision is refused for its runtime mode, whatever its params.
        value["projection"]["runtime_mode"] = json!("oci");
        let Err(ApiError::Refused(refusal)) = parse_request(&bytes(&value)) else {
            panic!("{name}: expected a refusal");
        };
        assert_eq!(refusal.reason(), Reason::RuntimeModeUnsupported, "{name}");
    }
    // Every valid legacy input stays admissible through the same entry point.
    for path in inventory_hashes().keys().filter(|p| p.contains("/valid/")) {
        let case = parse_legacy_case(&read(&repo().join(path))).unwrap();
        let mut value = base.clone();
        value["params"] = case.params.get().clone();
        assert!(parse_request(&bytes(&value)).is_ok(), "{path}");
    }
}

// ---- result identity ----------------------------------------------------------------------

#[test]
fn base_digest_is_present_exactly_when_the_projection_pins_a_base() {
    // (request fixture, its success result fixture, the digest the projection pins)
    let cases = [
        (
            "request/valid/wasm-no-base.json",
            "result/valid/success-no-base.json",
            None,
        ),
        (
            "request/valid/wasm-catalog-base.json",
            "result/valid/success-catalog.json",
            Some("sha256:".to_owned() + &"b".repeat(64)),
        ),
        (
            "request/valid/wasm-byo-base.json",
            "result/valid/success-byo.json",
            Some("sha256:".to_owned() + &"c".repeat(64)),
        ),
    ];
    for (request_file, result_file, pinned) in cases {
        let request = request_for(request_file);
        let (_, template) = fixture(result_file);
        let other = json!("sha256:".to_owned() + &"9".repeat(64));
        let states: [(&str, Option<Value>); 4] = [
            ("absent", None),
            ("null", Some(Value::Null)),
            ("pinned digest", pinned.clone().map(Value::from)),
            ("another digest", Some(other)),
        ];
        for (state, digest) in states {
            if digest.is_none() && state != "absent" {
                continue;
            }
            let mut result = template.clone();
            match &digest {
                None => remove_if_present(&mut result, "/identity/base_digest"),
                Some(value) => result["identity"]["base_digest"] = value.clone(),
            }
            let accepted = parse_result(&bytes(&result), &request).is_ok();
            let expected = matches!(
                (pinned.is_some(), state),
                (true, "pinned digest") | (false, "absent")
            );
            assert_eq!(accepted, expected, "{request_file}: base_digest {state}");
        }
    }
}

fn remove_if_present(value: &mut Value, pointer: &str) {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    if let Some(Value::Object(map)) = value.pointer_mut(parent) {
        map.remove(key);
    }
}

#[test]
fn absent_means_absent_for_every_optional_result_member() {
    let request = request_for("request/valid/wasm-catalog-base.json");
    let (_, success) = fixture("result/valid/success-catalog.json");
    let (_, failure) = fixture("result/valid/failure-gw-base-disabled.json");
    for (label, base, pointer) in [
        ("error on success", &success, "/error"),
        ("result on failure", &failure, "/result"),
        ("headers", &success, "/result/headers"),
        ("body", &success, "/result/body"),
    ] {
        let mut value = base.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let target = if parent.is_empty() {
            &mut value
        } else {
            value.pointer_mut(parent).unwrap()
        };
        target
            .as_object_mut()
            .unwrap()
            .insert(key.to_owned(), Value::Null);
        assert!(
            parse_result(&bytes(&value), &request).is_err(),
            "{label}: null is not absent"
        );
    }
    for (name, value) in [("success", &success), ("failure", &failure)] {
        assert!(parse_result(&bytes(value), &request).is_ok(), "{name}");
    }
    // A result with no identity block is rejected.
    let mut value = success.clone();
    value.as_object_mut().unwrap().remove("identity");
    assert!(parse_result(&bytes(&value), &request).is_err());
}

#[test]
fn parser_only_rules_reject_cross_document_mismatches() {
    let (_, base) = fixture("request/valid/wasm-catalog-base.json");
    for (name, pointer, new) in [
        (
            "credential activation",
            "/credential/activation",
            json!("act-other"),
        ),
        (
            "credential attempt",
            "/credential/attempt",
            json!("attempt-other"),
        ),
        (
            "credential revision",
            "/credential/revision",
            json!("rev-other"),
        ),
    ] {
        let mut value = base.clone();
        set(&mut value, pointer, new);
        assert!(
            matches!(
                parse_request(&bytes(&value)),
                Err(ApiError::Malformed(Malformed::Rule("credential_binding")))
            ),
            "{name}"
        );
    }
    let request = request_for("request/valid/wasm-catalog-base.json");
    let (_, success) = fixture("result/valid/success-catalog.json");
    for (name, pointer, new) in [
        ("activation", "/activation_id", json!("act-other")),
        ("attempt", "/attempt_id", json!("attempt-other")),
        (
            "revision",
            "/identity/function_revision",
            json!("rev-other"),
        ),
        (
            "artifact",
            "/identity/artifact_digest",
            json!("sha256:".to_owned() + &"0".repeat(64)),
        ),
        ("engine", "/identity/wasmtime_version", json!("0.0.0")),
        ("interface", "/identity/interface_version", json!("9.9.9")),
        (
            "profile",
            "/identity/language_profile/name",
            json!("python"),
        ),
    ] {
        let mut value = success.clone();
        set(&mut value, pointer, new);
        assert!(parse_result(&bytes(&value), &request).is_err(), "{name}");
    }
}

// ---- grants ---------------------------------------------------------------------------

#[test]
fn credential_and_request_grants_use_different_versions() {
    let schemas = Schemas::load();
    let claims = schemas.vendored("execution-credential.v1.claims.schema.json");
    assert!(claims.is_valid(&fixture("credential/valid.json").1));
    assert!(!claims.is_valid(&fixture("credential/grant-0-2-0.json").1));
    let requests = schemas.validator("execute-request.v1.schema.json");
    assert!(requests.is_valid(&fixture("request/valid/wasm-catalog-base.json").1));
    assert!(!requests.is_valid(&fixture("request/invalid/request-grant-0-1-0.json").1));
    // The runtime parser agrees on all four, and on the same mismatches inside a request.
    assert!(parse_request(&fixture("request/valid/wasm-catalog-base.json").0).is_ok());
    for file in [
        "request/invalid/request-grant-0-1-0.json",
        "request/invalid/credential-grant-0-2-0.json",
    ] {
        assert!(
            matches!(parse_request(&fixture(file).0), Err(ApiError::Malformed(_))),
            "{file}"
        );
        assert!(!requests.is_valid(&fixture(file).1), "{file}");
    }
    // The published credential reference is the canonical grants reference, unchanged.
    let vendored =
        json_file(&contract_dir().join("vendor/execution-credential.v1.claims.schema.json"));
    assert_eq!(
        vendored["properties"]["allowed_capabilities"]["items"]["$ref"],
        "https://falcone.dev/contracts/serverless/capability-grants/v1#/$defs/grant"
    );
    let request_schema = json_file(&contract_dir().join("execute-request.v1.schema.json"));
    assert_eq!(
        request_schema["$defs"]["projection"]["properties"]["granted_capabilities"]["items"]["$ref"],
        "urn:falcone:contracts:revision-runtime-manifest:1#/$defs/grant"
    );
    assert_eq!(
        request_schema["properties"]["credential"]["$ref"],
        "https://falcone.dev/contracts/serverless/execution-credential/v1/claims"
    );
    // No execution API schema defines its own copy of the grant shape.
    for entry in fs::read_dir(contract_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.to_string_lossy().ends_with(".schema.json") {
            let text = String::from_utf8(read(&path)).unwrap();
            for shape in [
                "storageScope",
                "httpScope",
                "eventScope",
                "workflowScope",
                "falcone:host/",
            ] {
                assert!(!text.contains(shape), "{}: {shape}", path.display());
            }
        }
    }
}

#[test]
fn vendored_schemas_are_byte_for_byte_pinned() {
    let vendor = contract_dir().join("vendor");
    let grants = read(&vendor.join("capability-grants.v1.schema.json"));
    assert_eq!(grants.len(), 6679);
    assert_eq!(
        sha256(&grants),
        "7f93939d0cc4b87d0bb6ef9af145a8b68a060ab918dc96ffc6247086a2b25dd2"
    );
    let claims = read(&vendor.join("execution-credential.v1.claims.schema.json"));
    assert_eq!(
        sha256(&claims),
        "9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0"
    );
    let readme = String::from_utf8(read(&vendor.join("README.md"))).unwrap();
    for needle in [
        "gntik-ai/falcone",
        "docs/contracts/serverless/capability-grants/capability-grants.v1.schema.json",
        "a19f40ab",
        "7f93939d0cc4b87d0bb6ef9af145a8b68a060ab918dc96ffc6247086a2b25dd2",
        "9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0",
    ] {
        assert!(readme.contains(needle), "{needle}");
    }
}

#[test]
fn spec_states_the_grant_version_divergence_once() {
    let spec = String::from_utf8(read(&contract_dir().join("execution-api.md"))).unwrap();
    let sentence = "The credential claims keep capability-grants v1 (`@0.1.0`) while the request projection uses the revision runtime manifest grants (`@0.2.0`).";
    assert_eq!(spec.matches(sentence).count(), 1);
}

// ---- redaction --------------------------------------------------------------------------

fn assert_redacted(text: &str, context: &str) {
    assert!(!text.contains(CREDENTIAL_LIKE), "{context}: {text}");
    assert!(!text.contains("s3cr3t"), "{context}: {text}");
}

fn output_of(error: &ApiError) -> Vec<String> {
    let log_line = format!("worker request failed: {error}");
    vec![
        format!("{error:?}"),
        format!("{error:#?}"),
        format!("{error}"),
        log_line,
    ]
}

#[test]
fn credential_like_request_strings_never_appear_in_error_output() {
    // Each place a request carries a string, with the sibling that must agree with it.
    let fields: [(&str, &[&str]); 21] = [
        ("/activation_id", &["/credential/activation"]),
        ("/attempt_id", &["/credential/attempt"]),
        ("/projection/revision", &["/credential/revision"]),
        ("/credential/iss", &[]),
        ("/credential/aud", &[]),
        ("/credential/tenant", &[]),
        ("/credential/workspace", &[]),
        ("/credential/pool_profile", &[]),
        ("/deadline", &[]),
        ("/projection/language_profile/name", &[]),
        ("/projection/language_profile/version", &[]),
        ("/projection/engine_version", &[]),
        ("/projection/wasi_version", &[]),
        ("/projection/interface_version", &[]),
        ("/projection/declared_imports/0", &[]),
        ("/projection/base/id", &[]),
        ("/projection/base/version", &[]),
        ("/projection/granted_capabilities/0/scope/hosts/0", &[]),
        ("/credential/allowed_capabilities/0/scope/hosts/0", &[]),
        ("/params/__ow_path", &[]),
        ("/projection/artifact_digest", &[]),
    ];
    for (pointer, siblings) in fields {
        let (_, mut base) = fixture("request/valid/wasm-catalog-base.json");
        base["projection"]["declared_imports"] = json!(["falcone:host/http"]);
        for place in std::iter::once(&pointer).chain(siblings.iter()) {
            set(&mut base, place, json!(CREDENTIAL_LIKE));
        }
        // The value cannot be mistaken for anything else: either it is accepted (and the
        // request must not print it) or rejected (and the error must not print it).
        let mut oci = base.clone();
        oci["projection"]["runtime_mode"] = json!("oci");
        let mut malformed = base.clone();
        malformed["attempt_number"] = json!(0);
        for (label, value) in [
            ("wasm", base.clone()),
            ("oci", oci),
            ("malformed", malformed),
        ] {
            match parse_request(&bytes(&value)) {
                Ok(request) => {
                    assert_redacted(&format!("{request:?}"), &format!("{pointer} request"));
                    assert_redacted(&format!("{request:#?}"), &format!("{pointer} request"));
                    let result = ExecutionResult::failure(&request, Reason::BaseDisabled);
                    assert_redacted(&format!("{result:?}"), &format!("{pointer} result"));
                }
                Err(error) => {
                    for text in output_of(&error) {
                        assert_redacted(&text, &format!("{pointer} {label}"));
                    }
                    if let ApiError::Refused(refusal) = &error {
                        // The serialized refusal keeps its contract fields, values included.
                        let result = serde_json::to_value(refusal.result()).unwrap();
                        for field in [
                            "activation_id",
                            "attempt_id",
                            "status",
                            "error",
                            "identity",
                            "logs",
                            "contract_version",
                        ] {
                            assert!(result.get(field).is_some(), "{pointer}: {field}");
                        }
                        assert_eq!(result["activation_id"], value["activation_id"]);
                        assert_eq!(result["attempt_id"], value["attempt_id"]);
                        assert_eq!(
                            result["identity"]["function_revision"],
                            value["projection"]["revision"]
                        );
                        assert_eq!(result["error"]["class"], "non_retryable");
                        assert_redacted(&format!("{:?}", refusal.result()), pointer);
                    }
                }
            }
        }
    }
    // Truncated and mistyped input, with the value inside it, reports only a static category.
    for raw in [
        format!("{{\"activation_id\": \"{CREDENTIAL_LIKE}\""),
        format!(
            "{{\"activation_id\": \"{CREDENTIAL_LIKE}\", \"attempt_number\": \"{CREDENTIAL_LIKE}\"}}"
        ),
        format!("[\"{CREDENTIAL_LIKE}\"]"),
        format!("\"{CREDENTIAL_LIKE}\""),
    ] {
        let error = parse_request(raw.as_bytes()).unwrap_err();
        for text in output_of(&error) {
            assert_redacted(&text, &raw);
        }
        for result in [
            parse_update(raw.as_bytes()).map(drop),
            parse_cancel(raw.as_bytes()).map(drop),
        ] {
            assert_redacted(&format!("{:?} {}", result, result.unwrap_err()), &raw);
        }
    }
}

// ---- size bound -------------------------------------------------------------------------

#[test]
fn oversized_messages_are_rejected_before_parsing() {
    let huge = vec![b' '; api::MAX_MESSAGE_BYTES + 1];
    assert!(matches!(
        parse_request(&huge),
        Err(ApiError::Malformed(Malformed::TooLarge))
    ));
    assert_eq!(parse_cancel(&huge), Err(Malformed::TooLarge));
}
