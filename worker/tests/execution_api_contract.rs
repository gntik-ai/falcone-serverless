//! ctr-worker-execution-api v1 conformance. No guest code runs and nothing uses the network.
//!
//! Every fixture is validated with `jsonschema` against the complete published schemas (the
//! vendored capability-grants and credential-claims schemas registered offline), and the runtime
//! parsers must agree with the schemas. `jsonschema` is a dev-dependency only.

use falcone_serverless_worker::api;

use api::{
    Malformed, Rejection, parse_cancel, parse_projection, parse_request, parse_result, parse_update,
};
use jsonschema::{Draft, Registry, Validator};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn contract_dir() -> PathBuf {
    root().join("docs/contracts/worker-execution-api")
}

fn fixtures() -> PathBuf {
    root().join("worker/tests/fixtures/contract")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn fixture(relative: &str) -> Value {
    json_file(&fixtures().join(format!("{relative}.json")))
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

/// Fixture names (without extension) of one directory, sorted; an empty directory is an error.
fn names(directory: &str) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(fixtures().join(directory))
        .unwrap()
        .map(|e| {
            let name = e.unwrap().file_name().into_string().unwrap();
            name.strip_suffix(".json").unwrap().to_owned()
        })
        .collect();
    names.sort();
    assert!(!names.is_empty(), "{directory} has no fixtures");
    names
}

// --- schemas ------------------------------------------------------------------------------

const MANIFEST_ID: &str = "urn:falcone:contracts:revision-runtime-manifest:1";
const GRANTS_ID: &str = "https://falcone.dev/contracts/serverless/capability-grants/v1";
const CLAIMS_ID: &str = "https://falcone.dev/contracts/serverless/execution-credential/v1/claims";
const SCHEMAS: [&str; 6] = [
    "common",
    "projection",
    "execute-request",
    "update",
    "cancel",
    "result",
];

fn schema(name: &str) -> Value {
    json_file(&contract_dir().join(format!("{name}.v1.schema.json")))
}

fn registry() -> Registry<'static> {
    let mut registry = Registry::new();
    for name in SCHEMAS {
        let document = schema(name);
        let id = document["$id"].as_str().unwrap().to_owned();
        registry = registry.add(id, document).unwrap();
    }
    registry = registry
        .add(
            MANIFEST_ID,
            json_file(&root().join("docs/contracts/revision-runtime-manifest.schema.json")),
        )
        .unwrap();
    registry = registry
        .add(
            GRANTS_ID,
            json_file(&contract_dir().join("vendor/capability-grants.v1.schema.json")),
        )
        .unwrap();
    registry = registry
        .add(
            CLAIMS_ID,
            json_file(&contract_dir().join("vendor/execution-credential.v1.claims.schema.json")),
        )
        .unwrap();
    registry.prepare().unwrap()
}

/// Validators that resolve every `$ref` offline from the registry.
struct Schemas {
    registry: &'static Registry<'static>,
}

impl Schemas {
    fn new() -> Self {
        static REGISTRY: std::sync::OnceLock<Registry<'static>> = std::sync::OnceLock::new();
        Self {
            registry: REGISTRY.get_or_init(registry),
        }
    }

    fn validator(&self, reference: &str) -> Validator {
        jsonschema::options()
            .with_draft(Draft::Draft202012)
            .should_validate_formats(true)
            .offline()
            .with_registry(self.registry)
            .build(&json!({ "$ref": reference }))
            .unwrap()
    }

    fn for_message(&self, kind: &str) -> Validator {
        match kind {
            "request" => self.validator("urn:falcone:contracts:worker-execution-api:1:execute"),
            "projection" => {
                self.validator("urn:falcone:contracts:worker-execution-api:1:projection")
            }
            "update" => self.validator("urn:falcone:contracts:worker-execution-api:1:update"),
            "cancel" => self.validator("urn:falcone:contracts:worker-execution-api:1:cancel"),
            "result" => self.validator("urn:falcone:contracts:worker-execution-api:1:result"),
            "credential" => self.validator(CLAIMS_ID),
            _ => unreachable!(),
        }
    }
}

// --- parsers under one interface ---------------------------------------------------------

/// What the runtime parser did with a message.
#[derive(Debug, PartialEq)]
enum Verdict {
    Accepted(Value),
    Malformed,
    /// Refused with the serialized refusal.
    Refused(Value),
}

fn parse(kind: &str, document: &Value, pinned: Option<&Value>) -> Verdict {
    let input = bytes(document);
    match kind {
        "request" => match parse_request(&input) {
            Ok(request) => Verdict::Accepted(serde_json::to_value(&request).unwrap()),
            Err(Rejection::Malformed(_)) => Verdict::Malformed,
            Err(Rejection::Refused(refused)) => {
                Verdict::Refused(serde_json::to_value(&refused.refusal).unwrap())
            }
        },
        "projection" => match parse_projection(&input) {
            Ok(projection) => Verdict::Accepted(serde_json::to_value(&projection).unwrap()),
            Err(_) => Verdict::Malformed,
        },
        "update" => parse_update(&input).map_or(Verdict::Malformed, |m| {
            Verdict::Accepted(serde_json::to_value(&m).unwrap())
        }),
        "cancel" => parse_cancel(&input).map_or(Verdict::Malformed, |m| {
            Verdict::Accepted(serde_json::to_value(&m).unwrap())
        }),
        "result" => {
            let pinned = pinned.expect("a result names what it answers");
            parse_against(&input, pinned)
        }
        "credential" => {
            let mut request = fixture("request/valid/wasm-no-base");
            request["credential"] = document.clone();
            match parse("request", &request, None) {
                Verdict::Accepted(_) => Verdict::Accepted(document.clone()),
                other => other,
            }
        }
        _ => unreachable!(),
    }
}

/// A result fixture names the request or projection fixture it answers.
fn parse_against(input: &[u8], pinned: &Value) -> Verdict {
    let name = pinned.as_str().unwrap();
    let document = fixture(name);
    let outcome = if name.starts_with("request/") {
        let request = parse_request(&bytes(&document)).unwrap();
        parse_result(input, &request)
    } else {
        let projection = parse_projection(&bytes(&document)).unwrap();
        parse_result(input, &projection)
    };
    outcome.map_or(Verdict::Malformed, |r| {
        Verdict::Accepted(serde_json::to_value(&r).unwrap())
    })
}

/// Splits a result fixture into the document and what it answers.
fn unwrap_result(wrapper: &Value) -> (Value, Value) {
    (wrapper["result"].clone(), wrapper["pinned"].clone())
}

fn judge(kind: &str, document: &Value, pinned: Option<&Value>) -> (bool, Verdict) {
    let schemas = Schemas::new();
    (
        schemas.for_message(kind).is_valid(document),
        parse(kind, document, pinned),
    )
}

// --- fixtures: schema and parser agree ---------------------------------------------------

#[test]
fn valid_fixtures_pass_the_schema_and_round_trip_through_the_types() {
    for kind in ["request", "projection", "update", "cancel", "credential"] {
        for name in names(&format!("{kind}/valid")) {
            let document = fixture(&format!("{kind}/valid/{name}"));
            let (schema_valid, verdict) = judge(kind, &document, None);
            assert!(schema_valid, "{kind}/{name}: schema rejects");
            assert_eq!(
                verdict,
                Verdict::Accepted(document),
                "{kind}/{name}: round trip"
            );
        }
    }
    for name in names("result/valid") {
        let (document, pinned) = unwrap_result(&fixture(&format!("result/valid/{name}")));
        let (schema_valid, verdict) = judge("result", &document, Some(&pinned));
        assert!(schema_valid, "result/{name}: schema rejects");
        assert_eq!(
            verdict,
            Verdict::Accepted(document),
            "result/{name}: round trip"
        );
    }
}

#[test]
fn invalid_fixtures_fail_the_schema_and_the_parser() {
    for kind in ["request", "projection", "update", "cancel", "credential"] {
        for name in names(&format!("{kind}/invalid")) {
            let document = fixture(&format!("{kind}/invalid/{name}"));
            let (schema_valid, verdict) = judge(kind, &document, None);
            assert!(!schema_valid, "{kind}/{name}: schema accepts");
            assert_ne!(
                std::mem::discriminant(&verdict),
                std::mem::discriminant(&Verdict::Accepted(Value::Null)),
                "{kind}/{name}: parser accepts"
            );
        }
    }
    for name in names("result/invalid") {
        let (document, pinned) = unwrap_result(&fixture(&format!("result/invalid/{name}")));
        let (schema_valid, verdict) = judge("result", &document, Some(&pinned));
        assert!(!schema_valid, "result/{name}: schema accepts");
        assert_eq!(verdict, Verdict::Malformed, "result/{name}");
    }
}

#[test]
fn rules_a_schema_cannot_state_are_enforced_by_the_parsers() {
    let rejected = [
        ("request", "request/rejected"),
        ("update", "update/rejected"),
    ];
    for (kind, directory) in rejected {
        for name in names(directory) {
            let document = fixture(&format!("{directory}/{name}"));
            let (schema_valid, verdict) = judge(kind, &document, None);
            assert!(
                schema_valid,
                "{directory}/{name}: the schema is meant to accept it"
            );
            assert_eq!(verdict, Verdict::Malformed, "{directory}/{name}");
        }
    }
    for name in names("result/cross") {
        let (document, pinned) = unwrap_result(&fixture(&format!("result/cross/{name}")));
        let (schema_valid, verdict) = judge("result", &document, Some(&pinned));
        assert!(
            schema_valid,
            "result/cross/{name}: the schema is meant to accept it"
        );
        assert_eq!(verdict, Verdict::Malformed, "result/cross/{name}");
    }
}

fn expected_refusal(code: &str, status: u16, reason: &str) -> Value {
    let message = match code {
        "runtime_mode_not_supported" => "This worker runs only revisions in runtime mode wasm.",
        "invalid_request" => "The request is malformed.",
        "request_too_large" => "The request body exceeds the size budget.",
        _ => unreachable!(),
    };
    json!({"status": status, "code": code, "message": message, "detail": {"reason": reason}, "retryable": false})
}

#[test]
fn a_revision_in_a_mode_other_than_wasm_is_refused_non_retryably() {
    let expected = expected_refusal("runtime_mode_not_supported", 409, "runtime_mode_not_wasm");
    // The refusal is the failure envelope of the pinned result fixture.
    let (document, _) = unwrap_result(&fixture("result/valid/failure-runtime-mode-not-supported"));
    assert_eq!(document["result"], expected);
    assert_eq!(document["class"], "non_retryable");
    for name in ["oci-mode", "unknown-runtime-mode"] {
        let document = fixture(&format!("request/refused/{name}"));
        let (_, verdict) = judge("request", &document, None);
        assert_eq!(verdict, Verdict::Refused(expected.clone()), "{name}");
    }
    // Only the schema-valid `oci` value belongs to a request that can be answered.
    let oci = fixture("request/refused/oci-mode");
    assert!(Schemas::new().for_message("request").is_valid(&oci));
    let Err(Rejection::Refused(refused)) = parse_request(&bytes(&oci)) else {
        panic!("oci is not refused");
    };
    let attribution = refused
        .attribution
        .expect("the answer needs the activation");
    assert_eq!(
        serde_json::to_value(&attribution.activation_id).unwrap(),
        oci["activation_id"]
    );
    assert_eq!(
        serde_json::to_value(&attribution.attempt_id).unwrap(),
        oci["attempt_id"]
    );
    assert_eq!(
        serde_json::to_value(attribution.attempt_number).unwrap(),
        oci["attempt_number"]
    );
    assert_eq!(
        *attribution.projection,
        parse_projection(&bytes(&oci["projection"])).unwrap()
    );
    let unknown = fixture("request/refused/unknown-runtime-mode");
    assert!(!Schemas::new().for_message("request").is_valid(&unknown));
    // The projection itself is a valid document in both modes.
    for name in ["wasm-no-base", "oci-catalog-base", "oci-byo-base"] {
        assert!(parse_projection(&bytes(&fixture(&format!("projection/valid/{name}")))).is_ok());
    }
}

#[test]
fn refused_fixtures_are_refused_non_retryably() {
    for name in names("request/refused") {
        let document = fixture(&format!("request/refused/{name}"));
        let (_, verdict) = judge("request", &document, None);
        let Verdict::Refused(refusal) = verdict else {
            panic!("{name} is not refused");
        };
        assert_eq!(refusal["retryable"], false, "{name}");
    }
}

#[test]
fn the_runtime_mode_is_required_and_closed() {
    let mut request = fixture("request/valid/wasm-no-base");
    for mode in [json!("wasm"), json!("oci")] {
        request["projection"]["runtime_mode"] = mode;
        assert!(Schemas::new().for_message("request").is_valid(&request));
    }
    for mode in [
        json!("native"),
        json!("WASM"),
        json!(""),
        json!(null),
        json!(1),
    ] {
        request["projection"]["runtime_mode"] = mode;
        assert!(!Schemas::new().for_message("request").is_valid(&request));
        // Every value other than `wasm` is a refusal or malformed, never an admitted request.
        assert!(parse_request(&bytes(&request)).is_err());
    }
}

// --- base union ----------------------------------------------------------------------------

#[test]
fn both_base_variants_and_an_absent_base_round_trip() {
    let absent = fixture("projection/valid/wasm-no-base");
    assert!(absent.get("base").is_none());
    for (name, kind) in [
        ("wasm-catalog-base", "catalog"),
        ("wasm-byo-base", "byo"),
        ("oci-catalog-base", "catalog"),
    ] {
        let base = fixture(&format!("projection/valid/{name}"))["base"].clone();
        assert_eq!(base["kind"], kind);
    }
    let Verdict::Accepted(round_trip) = parse("projection", &absent, None) else {
        panic!("absent base is rejected");
    };
    assert_eq!(round_trip, absent);
    assert!(round_trip.get("base").is_none());
}

#[test]
fn an_unresolved_default_alias_is_malformed() {
    let schemas = Schemas::new();
    for name in [
        "base-unresolved-default-alias",
        "base-alias-kind",
        "base-unknown-kind",
        "base-null",
    ] {
        let document = fixture(&format!("projection/invalid/{name}"));
        assert!(
            !schemas.for_message("projection").is_valid(&document),
            "{name}"
        );
        assert!(parse_projection(&bytes(&document)).is_err(), "{name}");
    }
    let mut projection = fixture("projection/valid/wasm-catalog-base");
    for alias in ["default", "latest", "1.x", "1.4", "v1.4.2", "", "1.4.2-rc1"] {
        projection["base"]["version"] = json!(alias);
        assert!(
            !schemas.for_message("projection").is_valid(&projection),
            "{alias}"
        );
        assert!(parse_projection(&bytes(&projection)).is_err(), "{alias}");
    }
    projection["base"]["version"] = json!("1.4.2");
    projection["base"]["id"] = json!("python@default");
    assert!(!schemas.for_message("projection").is_valid(&projection));
    assert!(parse_projection(&bytes(&projection)).is_err());
}

// --- identity and the absent-versus-null rule -------------------------------------------

#[test]
fn the_base_digest_is_absent_exactly_when_no_base_is_pinned() {
    // (fixture, pinned base?, base_digest as written) in the four cases plus both inverses.
    let cases = [
        ("result/invalid/base-digest-null-without-base", false),
        ("result/invalid/base-digest-null-with-base", true),
        ("result/valid/success-none-base", false),
        ("result/valid/success-catalog-base", true),
        ("result/cross/pinned-base-without-base-digest", true),
        ("result/cross/base-digest-without-pinned-base", false),
    ];
    for (name, pinned_base) in cases {
        let (document, pinned) = unwrap_result(&fixture(name));
        let projection = fixture(pinned.as_str().unwrap());
        assert_eq!(projection.get("base").is_some(), pinned_base, "{name}");
        let accepted = matches!(
            parse("result", &document, Some(&pinned)),
            Verdict::Accepted(_)
        );
        assert_eq!(
            accepted,
            name.contains("/valid/"),
            "{name}: accepted only for absent-without-base and digest-with-base"
        );
    }
    // The same rule for a BYO base: its image digest is the base digest.
    let (document, pinned) = unwrap_result(&fixture("result/valid/success-byo-base"));
    assert_eq!(
        document["identity"]["base_digest"],
        fixture(pinned.as_str().unwrap())["base"]["image_digest"]
    );
}

#[test]
fn the_result_requires_the_identity_block() {
    let (document, pinned) = unwrap_result(&fixture("result/invalid/identity-missing"));
    assert!(document.get("identity").is_none());
    assert!(!Schemas::new().for_message("result").is_valid(&document));
    assert_eq!(
        parse("result", &document, Some(&pinned)),
        Verdict::Malformed
    );
    let required = schema("common")["$defs"]["identity"]["required"].clone();
    for field in required.as_array().unwrap() {
        let (mut document, pinned) = unwrap_result(&fixture("result/valid/success-catalog-base"));
        document["identity"]
            .as_object_mut()
            .unwrap()
            .remove(field.as_str().unwrap());
        assert!(
            !Schemas::new().for_message("result").is_valid(&document),
            "{field}"
        );
        assert_eq!(
            parse("result", &document, Some(&pinned)),
            Verdict::Malformed,
            "{field}"
        );
    }
}

/// Every optional result field specified as absent rejects `null` in schema and parser.
#[test]
fn an_explicit_null_is_never_absence() {
    let base_result = || unwrap_result(&fixture("result/valid/success-catalog-base"));
    let failure = || unwrap_result(&fixture("result/valid/failure-base-disabled"));
    let cases: Vec<(&str, (Value, Value), &str)> = vec![
        ("class on success", base_result(), "/class"),
        ("headers", base_result(), "/result/headers"),
        ("body", base_result(), "/result/body"),
        ("base_digest", base_result(), "/identity/base_digest"),
        ("class on failure", failure(), "/class"),
    ];
    for (label, (mut document, pinned), pointer) in cases {
        match pointer.rsplit_once('/').unwrap() {
            ("", key) => document[key] = Value::Null,
            (parent, key) => document.pointer_mut(parent).unwrap()[key] = Value::Null,
        }
        assert!(
            !Schemas::new().for_message("result").is_valid(&document),
            "{label}"
        );
        assert_eq!(
            parse("result", &document, Some(&pinned)),
            Verdict::Malformed,
            "{label}"
        );
    }
    let nulls = [
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/invocation/idempotency_key",
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/invocation/correlation_id",
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/projection/base",
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/projection/grants/1/secretRef",
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/projection/grants/1/secretRef/version",
        ),
        ("projection", "projection/valid/wasm-byo-base", "/base"),
        ("update", "update/valid/progress", "/update/total"),
    ];
    for (kind, name, pointer) in nulls {
        let mut document = fixture(name);
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        document.pointer_mut(parent).unwrap()[key] = Value::Null;
        assert!(
            !Schemas::new().for_message(kind).is_valid(&document),
            "{pointer}"
        );
        assert_ne!(
            std::mem::discriminant(&parse(kind, &document, None)),
            std::mem::discriminant(&Verdict::Accepted(Value::Null)),
            "{pointer}"
        );
    }
}

// --- required fields, unknown fields -------------------------------------------------------

fn required_of(document: &Value) -> Vec<String> {
    document["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn every_required_field_is_required_and_every_object_is_closed() {
    let schemas = Schemas::new();
    let common = schema("common");
    let claims =
        json_file(&contract_dir().join("vendor/execution-credential.v1.claims.schema.json"));
    // (kind, fixture, pointer to the object, its required fields from the published schema)
    let targets: Vec<(&str, &str, &str, Vec<String>)> = vec![
        (
            "request",
            "request/valid/wasm-catalog-base",
            "",
            required_of(&schema("execute-request")),
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/projection",
            required_of(&schema("projection")),
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/credential",
            required_of(&claims),
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/invocation",
            vec!["invocation_key".into()],
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/projection/budgets",
            required_of(&schema("projection")["properties"]["budgets"]),
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/projection/language_profile",
            required_of(&common["$defs"]["language_profile"]),
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/projection/base",
            required_of(&common["$defs"]["base"]["oneOf"][0]),
        ),
        (
            "request",
            "request/valid/wasm-byo-base",
            "/projection/base",
            required_of(&common["$defs"]["base"]["oneOf"][1]),
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/projection/grants/0",
            vec!["family".into(), "interface".into(), "scope".into()],
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/projection/grants/0/scope",
            vec!["hosts".into()],
        ),
        (
            "request",
            "request/valid/wasm-catalog-base",
            "/credential/allowed_capabilities/0",
            vec!["family".into(), "interface".into(), "scope".into()],
        ),
        (
            "projection",
            "projection/valid/wasm-catalog-base",
            "",
            required_of(&schema("projection")),
        ),
        (
            "update",
            "update/valid/progress",
            "",
            required_of(&schema("update")),
        ),
        (
            "update",
            "update/valid/progress",
            "/update",
            vec!["kind".into(), "completed".into()],
        ),
        (
            "update",
            "update/valid/checkpoint",
            "/update",
            vec!["kind".into(), "checkpoint_ref".into()],
        ),
        (
            "cancel",
            "cancel/valid/requested",
            "",
            required_of(&schema("cancel")),
        ),
        (
            "cancel",
            "cancel/valid/requested",
            "/cancel",
            vec!["state".into()],
        ),
    ];
    for (kind, name, pointer, required) in targets {
        assert!(!required.is_empty(), "{name}{pointer}");
        for field in &required {
            let mut document = fixture(name);
            let object = if pointer.is_empty() {
                &mut document
            } else {
                document.pointer_mut(pointer).unwrap()
            };
            object.as_object_mut().unwrap().remove(field);
            assert!(
                !schemas.for_message(kind).is_valid(&document),
                "{name}{pointer} without {field}"
            );
            assert_ne!(
                std::mem::discriminant(&parse(kind, &document, None)),
                std::mem::discriminant(&Verdict::Accepted(Value::Null)),
                "{name}{pointer} without {field}: parser accepts"
            );
        }
        let mut document = fixture(name);
        let object = if pointer.is_empty() {
            &mut document
        } else {
            document.pointer_mut(pointer).unwrap()
        };
        object
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!("x"));
        assert!(
            !schemas.for_message(kind).is_valid(&document),
            "{name}{pointer} extra field"
        );
        assert_ne!(
            std::mem::discriminant(&parse(kind, &document, None)),
            std::mem::discriminant(&Verdict::Accepted(Value::Null)),
            "{name}{pointer} extra field: parser accepts"
        );
    }
}

#[test]
fn scalar_rules_agree_between_schema_and_parser() {
    let digest = |c: char| json!(format!("sha256:{}", c.to_string().repeat(64)));
    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "/deadline",
            "2026-11-11T12:00:30Z",
            json!("2026-11-11T12:00:30Z"),
        ),
        (
            "/deadline",
            "fraction",
            json!("2026-11-11T12:00:30.123456789Z"),
        ),
        ("/deadline", "offset", json!("2026-11-11T12:00:30+01:00")),
        ("/deadline", "no zone", json!("2026-11-11T12:00:30")),
        ("/deadline", "month 13", json!("2026-13-11T12:00:30Z")),
        ("/deadline", "feb 30", json!("2026-02-30T12:00:30Z")),
        ("/deadline", "leap second", json!("2026-06-30T23:59:60Z")),
        ("/deadline", "second 61", json!("2026-06-30T23:59:61Z")),
        ("/deadline", "hour 24", json!("2026-11-11T24:00:00Z")),
        ("/deadline", "leap day", json!("2028-02-29T00:00:00Z")),
        ("/deadline", "no leap day", json!("2026-02-29T00:00:00Z")),
        ("/deadline", "empty", json!("")),
        ("/deadline", "number", json!(1)),
        ("/attempt_number", "zero", json!(0)),
        ("/attempt_number", "negative", json!(-1)),
        ("/attempt_number", "text", json!("1")),
        ("/activation_id", "slash", json!("act/ok:1")),
        ("/activation_id", "space", json!("act 1")),
        ("/activation_id", "empty", json!("")),
        ("/activation_id", "leading dash", json!("-act")),
        ("/activation_id", "129 chars", json!("a".repeat(129))),
        ("/activation_id", "128 chars", json!("a".repeat(128))),
        (
            "/projection/artifact_digest",
            "uppercase",
            json!(format!("sha256:{}", "A".repeat(64))),
        ),
        ("/projection/artifact_digest", "short", json!("sha256:abc")),
        (
            "/projection/artifact_digest",
            "sha512",
            json!(format!("sha512:{}", "a".repeat(64))),
        ),
        (
            "/projection/artifact_digest",
            "bare hex",
            json!("a".repeat(64)),
        ),
        ("/projection/artifact_digest", "ok", digest('e')),
        ("/projection/interface_version", "0.1.0", json!("0.1.0")),
        ("/projection/budgets/fuel", "zero", json!(0)),
        ("/projection/budgets/timeout_ms", "fraction", json!(1.5)),
        ("/projection/imports/0", "empty", json!("")),
        ("/projection/grants/0/scope/hosts/0", "wildcard", json!("*")),
        ("/projection/grants/0/scope/hosts", "empty list", json!([])),
        ("/projection/grants/0/family", "unknown", json!("network")),
        (
            "/projection/grants/0/interface",
            "wrong family",
            json!("falcone:host/storage@0.2.0"),
        ),
        ("/projection/grants/1/secretRef/x", "unknown key", json!(1)),
        ("/credential/exp", "negative", json!(-1)),
        ("/credential/iat", "string", json!("1")),
        ("/credential/iss", "empty", json!("")),
        ("/params/__ow_path", "number", json!(1)),
        ("/params/__ow_headers", "non-string header", json!({"a": 1})),
        ("/params/__ow_query", "array", json!([])),
        ("/params/__ow_method", "number", json!(1)),
        (
            "/params/action_param",
            "anything",
            json!({"nested": [1, null]}),
        ),
    ];
    for (pointer, label, value) in cases {
        let mut document = fixture("request/valid/wasm-no-base");
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let parent = if parent.is_empty() {
            &mut document
        } else {
            document.pointer_mut(parent).unwrap()
        };
        match parent {
            Value::Array(items) => items[key.parse::<usize>().unwrap()] = value,
            other => other[key] = value,
        }
        // The credential is bound to the identifier, so the two move together.
        if pointer == "/activation_id" {
            document["credential"]["activation"] = document["activation_id"].clone();
        }
        let schema_valid = Schemas::new().for_message("request").is_valid(&document);
        let parser_accepts = matches!(parse("request", &document, None), Verdict::Accepted(_));
        assert_eq!(schema_valid, parser_accepts, "{pointer} {label}");
    }
}

#[test]
fn a_progress_total_below_completed_is_rejected_by_the_parser() {
    let document = fixture("update/rejected/progress-over-total");
    assert_eq!(parse("update", &document, None), Verdict::Malformed);
    assert_eq!(
        parse_update(&bytes(&document)),
        Err(Malformed::Constraint("update.progress"))
    );
}

// --- grants -------------------------------------------------------------------------------

#[test]
fn credential_grants_are_0_1_0_and_request_grants_are_0_2_0() {
    let schemas = Schemas::new();
    // A credential with a 0.1.0 grant passes and with a 0.2.0 grant fails.
    let credential = fixture("credential/valid/grants-0.1.0");
    assert!(schemas.for_message("credential").is_valid(&credential));
    let bad_credential = fixture("credential/invalid/grant-0.2.0");
    assert!(!schemas.for_message("credential").is_valid(&bad_credential));
    // A request with a 0.2.0 grant passes and with a 0.1.0 grant fails.
    let request = fixture("request/valid/wasm-no-base");
    assert!(schemas.for_message("request").is_valid(&request));
    let projection_01 = fixture("request/invalid/projection-grant-0.1.0");
    assert!(!schemas.for_message("request").is_valid(&projection_01));
    assert!(parse_request(&bytes(&projection_01)).is_err());
    let credential_02 = fixture("request/invalid/credential-grant-0.2.0");
    assert!(!schemas.for_message("request").is_valid(&credential_02));
    assert!(parse_request(&bytes(&credential_02)).is_err());
    // The request carries both versions at once, each in its own list.
    assert_eq!(
        request["credential"]["allowed_capabilities"][0]["interface"],
        "falcone:host/http@0.1.0"
    );
    assert_eq!(
        request["projection"]["grants"][0]["interface"],
        "falcone:host/http@0.2.0"
    );
    // Every family accepts only its own interface and scope field, in both lists.
    for (family, interface, field) in [
        ("storage", "storage", "prefixes"),
        ("http", "http", "hosts"),
        ("event", "events", "topics"),
        ("workflow", "workflows", "targets"),
    ] {
        for (version, pointer) in [
            ("0.2.0", "/projection/grants"),
            ("0.1.0", "/credential/allowed_capabilities"),
        ] {
            let mut document = fixture("request/valid/wasm-no-base");
            document.pointer_mut(pointer).unwrap()[0] = json!({
                "family": family,
                "interface": format!("falcone:host/{interface}@{version}"),
                "scope": {field: ["entry"]}
            });
            assert!(
                schemas.for_message("request").is_valid(&document),
                "{family} {version}"
            );
            assert!(
                parse_request(&bytes(&document)).is_ok(),
                "{family} {version}"
            );
            let mut crossed = document.clone();
            let other = if field == "hosts" {
                "prefixes"
            } else {
                "hosts"
            };
            crossed.pointer_mut(pointer).unwrap()[0]["scope"] = json!({other: ["entry"]});
            assert!(
                !schemas.for_message("request").is_valid(&crossed),
                "{family} crossed"
            );
            assert!(parse_request(&bytes(&crossed)).is_err(), "{family} crossed");
        }
    }
}

#[test]
fn the_credential_keeps_the_canonical_grant_reference() {
    let claims =
        json_file(&contract_dir().join("vendor/execution-credential.v1.claims.schema.json"));
    assert_eq!(
        claims["properties"]["allowed_capabilities"]["items"]["$ref"],
        "https://falcone.dev/contracts/serverless/capability-grants/v1#/$defs/grant"
    );
    assert_eq!(claims["$id"], CLAIMS_ID);
    // No schema owned by this contract defines a grant shape of its own.
    for name in SCHEMAS {
        let text =
            fs::read_to_string(contract_dir().join(format!("{name}.v1.schema.json"))).unwrap();
        assert!(!text.contains("\"family\""), "{name}");
        assert!(!text.contains("falcone:host/"), "{name}");
    }
    let projection = schema("projection");
    assert_eq!(
        projection["properties"]["grants"]["items"]["$ref"],
        format!("{MANIFEST_ID}#/$defs/grant")
    );
    let request = schema("execute-request");
    assert_eq!(request["properties"]["credential"]["$ref"], CLAIMS_ID);
}

#[test]
fn vendored_files_and_pinned_fixtures_keep_their_hashes() {
    let vendor = contract_dir().join("vendor");
    let grants = fs::read(vendor.join("capability-grants.v1.schema.json")).unwrap();
    assert_eq!(grants.len(), 6679);
    assert_eq!(
        sha256(&grants),
        "7f93939d0cc4b87d0bb6ef9af145a8b68a060ab918dc96ffc6247086a2b25dd2"
    );
    let claims = fs::read(vendor.join("execution-credential.v1.claims.schema.json")).unwrap();
    assert_eq!(
        sha256(&claims),
        "9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0"
    );
    let errors = fixtures().join("functions-api/errors");
    for (name, hash) in [
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
    ] {
        assert_eq!(
            sha256(&fs::read(errors.join(format!("{name}.json"))).unwrap()),
            hash,
            "{name}"
        );
    }
    let readme = fs::read_to_string(vendor.join("README.md")).unwrap();
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

// --- failure codes --------------------------------------------------------------------------

fn code_table() -> Vec<Value> {
    json_file(&contract_dir().join("failure-codes.v1.json"))
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn every_functions_api_code_matches_the_pinned_error_fixtures() {
    let table = code_table();
    let errors = fixtures().join("functions-api/errors");
    let mut pinned = BTreeSet::new();
    for entry in fs::read_dir(&errors).unwrap() {
        let pin = json_file(&entry.unwrap().path());
        pinned.insert(pin["code"].as_str().unwrap().to_owned());
        let row = table
            .iter()
            .find(|row| row["code"] == pin["code"])
            .unwrap_or_else(|| panic!("{} is missing from the table", pin["code"]));
        assert_eq!(row["origin"], "functions-api");
        assert_eq!(row["status"], pin["status"]);
        assert_eq!(row["reasons"], json!([pin["detail"]["reason"]]));
        assert_eq!(row["retryable"], pin["retryable"]);
    }
    let from_functions_api: BTreeSet<_> = table
        .iter()
        .filter(|row| row["origin"] == "functions-api")
        .map(|row| row["code"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(from_functions_api, pinned);
    assert_eq!(
        pinned,
        [
            "GW_BASE_DISABLED",
            "GW_BASE_MISMATCH",
            "GW_OCI_NOT_AUTHORIZED"
        ]
        .map(String::from)
        .into()
    );
    // The only worker-only codes: the non-wasm refusal and the two legacy rejections.
    let worker_only: BTreeSet<_> = table
        .iter()
        .filter(|row| row["origin"] == "worker")
        .map(|row| row["code"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        worker_only,
        [
            "invalid_request",
            "request_too_large",
            "runtime_mode_not_supported"
        ]
        .map(String::from)
        .into()
    );
    for row in &table {
        assert_eq!(row["retryable"], false, "{}", row["code"]);
        assert_eq!(row["class"], "non_retryable", "{}", row["code"]);
    }
}

#[test]
fn the_code_table_matches_the_schema_the_spec_and_the_fixtures() {
    let table = code_table();
    let common = schema("common");
    let schema_codes: BTreeSet<_> = common["$defs"]["failure"]["properties"]["code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    let table_codes: BTreeSet<_> = table
        .iter()
        .map(|r| r["code"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(schema_codes, table_codes);
    let spec = fs::read_to_string(contract_dir().join("execution-api-v1.md")).unwrap();
    let fixture_codes: BTreeSet<_> = names("result/valid")
        .into_iter()
        .filter(|name| name.starts_with("failure-"))
        .map(|name| {
            let (document, _) = unwrap_result(&fixture(&format!("result/valid/{name}")));
            document["result"]["code"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(fixture_codes, table_codes, "every code has a fixture");
    for row in &table {
        let code = row["code"].as_str().unwrap();
        let line = spec
            .lines()
            .find(|line| line.starts_with(&format!("| `{code}` |")))
            .unwrap_or_else(|| panic!("{code} is not in the spec table"));
        assert!(line.contains(&format!("| {} |", row["status"])), "{code}");
        assert!(line.contains("non_retryable"), "{code}");
        for reason in row["reasons"].as_array().unwrap() {
            assert!(line.contains(reason.as_str().unwrap()), "{code}");
        }
    }
    // Each failure fixture carries the status, reason and class of its table row, parses, and
    // the GW_ ones equal the pinned error fixtures on status, reason and retryable.
    for name in names("result/valid")
        .into_iter()
        .filter(|n| n.starts_with("failure-"))
    {
        let (document, pinned) = unwrap_result(&fixture(&format!("result/valid/{name}")));
        let row = table
            .iter()
            .find(|r| r["code"] == document["result"]["code"])
            .unwrap();
        assert_eq!(document["result"]["status"], row["status"], "{name}");
        assert!(
            row["reasons"]
                .as_array()
                .unwrap()
                .contains(&document["result"]["detail"]["reason"]),
            "{name}"
        );
        assert_eq!(document["class"], row["class"], "{name}");
        assert_eq!(document["result"]["retryable"], row["retryable"], "{name}");
        assert!(
            matches!(
                parse("result", &document, Some(&pinned)),
                Verdict::Accepted(_)
            ),
            "{name}"
        );
    }
}

#[test]
fn oci_not_authorized_and_base_disabled_stay_non_retryable_on_retry() {
    let schemas = Schemas::new();
    for name in ["oci-not-authorized", "base-disabled"] {
        for variant in [name.to_owned(), format!("{name}-retry")] {
            let (document, pinned) =
                unwrap_result(&fixture(&format!("result/valid/failure-{variant}")));
            assert_eq!(document["class"], "non_retryable", "{variant}");
            assert_eq!(document["result"]["retryable"], false, "{variant}");
            assert!(
                schemas.for_message("result").is_valid(&document),
                "{variant}"
            );
            assert!(
                matches!(
                    parse("result", &document, Some(&pinned)),
                    Verdict::Accepted(_)
                ),
                "{variant}"
            );
            // Any attempt number, and any claim of retryability or another class, is rejected.
            for attempt in [1, 2, 7] {
                let mut retried = document.clone();
                retried["attempt_number"] = json!(attempt);
                assert!(matches!(
                    parse("result", &retried, Some(&pinned)),
                    Verdict::Accepted(_)
                ));
                for (class, retryable) in [
                    ("retryable", true),
                    ("uncertain", false),
                    ("retryable", false),
                    ("non_retryable", true),
                ] {
                    let mut wrong = retried.clone();
                    wrong["class"] = json!(class);
                    wrong["result"]["retryable"] = json!(retryable);
                    assert!(
                        !schemas.for_message("result").is_valid(&wrong),
                        "{variant} {class} {retryable}"
                    );
                    assert_eq!(
                        parse("result", &wrong, Some(&pinned)),
                        Verdict::Malformed,
                        "{variant} {class} {retryable}"
                    );
                }
            }
        }
    }
    let first = fixture("result/valid/failure-base-disabled")["result"].clone();
    let retry = fixture("result/valid/failure-base-disabled-retry")["result"].clone();
    assert_eq!(first["result"], retry["result"]);
    assert_eq!(first["attempt_number"], 1);
    assert_eq!(retry["attempt_number"], 2);
    assert_ne!(first["attempt_id"], retry["attempt_id"]);
}

#[test]
fn a_failure_envelope_must_carry_the_status_and_reason_of_its_code() {
    let schemas = Schemas::new();
    let (document, pinned) = unwrap_result(&fixture("result/valid/failure-base-mismatch"));
    let mutations: Vec<(&str, Value)> = vec![
        ("/result/status", json!(403)),
        ("/result/status", json!(500)),
        ("/result/detail/reason", json!("base_disabled")),
        ("/result/detail/reason", json!("unknown")),
        ("/result/code", json!("GW_UNKNOWN")),
        ("/result/code", json!("base_mismatch")),
        ("/result/retryable", json!(true)),
        ("/class", json!("uncertain")),
    ];
    for (pointer, value) in mutations {
        let mut mutated = document.clone();
        *mutated.pointer_mut(pointer).unwrap() = value;
        assert!(
            !schemas.for_message("result").is_valid(&mutated),
            "{pointer}"
        );
        assert_eq!(
            parse("result", &mutated, Some(&pinned)),
            Verdict::Malformed,
            "{pointer}"
        );
    }
    // A failure carries a failure envelope and a success a handler result, never the other way.
    let (success, pinned) = unwrap_result(&fixture("result/valid/success-none-base"));
    let mut crossed = success.clone();
    crossed["status"] = json!("failure");
    assert!(!schemas.for_message("result").is_valid(&crossed));
    assert_eq!(parse("result", &crossed, Some(&pinned)), Verdict::Malformed);
}

// --- legacy envelope ------------------------------------------------------------------------

fn inventory() -> String {
    fs::read_to_string(root().join("docs/contracts/falcone-contract-inventory.md")).unwrap()
}

/// `path -> sha256` of the legacy fixture table of the inventory.
fn legacy_hashes() -> Vec<(String, String)> {
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
fn the_legacy_fixtures_are_the_inventory_bytes() {
    let rows = legacy_hashes();
    assert_eq!(rows.len(), 11);
    for (path, hash) in rows {
        assert_eq!(
            sha256(&fs::read(root().join(&path)).unwrap()),
            hash,
            "{path}"
        );
    }
}

/// An execute message that carries a legacy fixture's params and invocation identity.
fn legacy_request(legacy: &Value) -> Value {
    let identity = &legacy["invocation"]["identity"];
    let mut request = fixture("request/valid/wasm-no-base");
    request["attempt_id"] = identity["attempt-id"].clone();
    request["credential"]["attempt"] = identity["attempt-id"].clone();
    request["invocation"] = json!({
        "invocation_key": identity["invocation-key"],
        "idempotency_key": identity["idempotency-key"],
        "correlation_id": legacy["invocation"]["headers"]["X-Correlation-Id"],
    });
    request["params"] = legacy["params"].clone();
    request
}

fn legacy(directory: &str, name: &str) -> Value {
    json_file(&root().join(format!(
        "docs/contracts/fixtures/legacy-envelope/{directory}/{name}.json"
    )))
}

#[test]
fn valid_legacy_fixtures_round_trip_unchanged() {
    let schemas = Schemas::new();
    for name in [
        "401", "400", "429", "404", "403", "304", "200-json", "200-yaml",
    ] {
        let fixture = legacy("valid", name);
        let request = legacy_request(&fixture);
        assert!(schemas.for_message("request").is_valid(&request), "{name}");
        let Verdict::Accepted(round_trip) = parse("request", &request, None) else {
            panic!("{name} is not accepted");
        };
        assert_eq!(round_trip["params"], fixture["params"], "{name}: params");
        assert_eq!(
            round_trip["invocation"], request["invocation"],
            "{name}: identity"
        );
        assert_eq!(
            round_trip["attempt_id"],
            fixture["invocation"]["identity"]["attempt-id"]
        );
        // The expected handler result of the fixture is a valid success result.
        let mut result = fixture_result("result/valid/success-none-base");
        result["result"] = fixture["expected"].clone();
        let pinned = json!("projection/valid/wasm-no-base");
        if fixture["expected"]["statusCode"] == 304 {
            assert!(fixture["expected"].get("body").is_none());
        }
        assert!(
            schemas.for_message("result").is_valid(&result),
            "{name}: result schema"
        );
        assert_eq!(
            parse("result", &result, Some(&pinned)),
            Verdict::Accepted(result.clone()),
            "{name}: result round trip"
        );
    }
}

fn fixture_result(name: &str) -> Value {
    unwrap_result(&fixture(name)).0
}

#[test]
fn invalid_legacy_inputs_are_refused_before_invocation() {
    let schemas = Schemas::new();
    // (fixture, code, status, reason, does the schema alone reject it)
    let cases = [
        ("missing-path", "invalid_request", 400, "missing_path", true),
        (
            "non-JSON-body",
            "invalid_request",
            400,
            "non_json_body",
            false,
        ),
        (
            "oversized-body",
            "request_too_large",
            413,
            "body_too_large",
            true,
        ),
    ];
    for (name, code, status, reason, schema_rejects) in cases {
        let fixture = legacy("invalid", name);
        assert_eq!(fixture["expected"]["invoked"], false, "{name}");
        let request = legacy_request(&fixture);
        assert_eq!(
            !schemas.for_message("request").is_valid(&request),
            schema_rejects,
            "{name}"
        );
        // The refusal is returned instead of a request: nothing can be invoked.
        let outcome = parse_request(&bytes(&request));
        let Err(Rejection::Refused(refused)) = outcome else {
            panic!("{name} is not refused");
        };
        let refusal = serde_json::to_value(&refused.refusal).unwrap();
        assert_eq!(refusal, expected_refusal(code, status, reason), "{name}");
        assert_eq!(refusal["detail"]["reason"], reason, "{name}");
        assert_eq!(refusal["retryable"], false, "{name}");
        assert!(
            refused.attribution.is_some(),
            "{name}: the caller can answer"
        );
    }
    // The size limit is exactly max-host-to-guest-bytes, and one byte less is accepted.
    let oversized = legacy("invalid", "oversized-body");
    let body = oversized["params"]["__ow_body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(body.len(), 1_048_577);
    let mut request = legacy_request(&oversized);
    request["params"]["__ow_body"] = json!(body.replacen("AA", "A", 1));
    assert_eq!(
        request["params"]["__ow_body"].as_str().unwrap().len(),
        1_048_576
    );
    assert!(schemas.for_message("request").is_valid(&request));
    assert!(parse_request(&bytes(&request)).is_ok());
}

#[test]
fn the_legacy_rejections_check_in_order_and_only_for_json() {
    let valid = legacy_request(&legacy("valid", "200-json"));
    let mut request = valid.clone();
    request["params"]["__ow_body"] = json!("not json");
    // No JSON content type: the body is opaque text.
    assert!(parse_request(&bytes(&request)).is_ok());
    for content_type in [
        "application/json",
        "Application/JSON; charset=utf-8",
        "application/json ;x=y",
    ] {
        request["params"]["__ow_headers"]["Content-Type"] = json!(content_type);
        assert!(parse_request(&bytes(&request)).is_err(), "{content_type}");
    }
    request["params"]["__ow_headers"]["Content-Type"] = json!("text/plain");
    assert!(parse_request(&bytes(&request)).is_ok());
    // A valid JSON body passes; the path is checked first, then the size, then JSON.
    request["params"]["__ow_headers"]["content-type"] = json!("application/json");
    request["params"]["__ow_body"] = json!("{\"a\":1}");
    assert!(parse_request(&bytes(&request)).is_ok());
    request["params"]["__ow_body"] = json!(format!("{{\"a\":\"{}\"", "x".repeat(1_048_576)));
    let Err(Rejection::Refused(refused)) = parse_request(&bytes(&request)) else {
        panic!()
    };
    assert_eq!(
        serde_json::to_value(&refused.refusal).unwrap()["code"],
        "request_too_large"
    );
    request["params"]
        .as_object_mut()
        .unwrap()
        .remove("__ow_path");
    let Err(Rejection::Refused(refused)) = parse_request(&bytes(&request)) else {
        panic!()
    };
    assert_eq!(
        serde_json::to_value(&refused.refusal).unwrap()["detail"]["reason"],
        "missing_path"
    );
}

#[test]
fn the_credential_is_bound_to_the_request() {
    for name in [
        "credential-activation-mismatch",
        "credential-revision-mismatch",
    ] {
        let document = fixture(&format!("request/rejected/{name}"));
        assert!(matches!(
            parse_request(&bytes(&document)),
            Err(Rejection::Malformed(Malformed::Constraint(
                "credential.binding"
            )))
        ));
    }
    let mut document = fixture("request/valid/wasm-no-base");
    document["credential"]["attempt"] = json!("att-synthetic-0002");
    assert!(parse_request(&bytes(&document)).is_err());
}

// --- redaction ------------------------------------------------------------------------------

/// Every string value (not key) of 8 or more characters in a JSON document.
fn long_strings(value: &Value, found: &mut BTreeSet<String>) {
    match value {
        Value::String(text) if text.chars().count() >= 8 => {
            found.insert(text.clone());
        }
        Value::Array(items) => items.iter().for_each(|v| long_strings(v, found)),
        Value::Object(map) => map.values().for_each(|v| long_strings(v, found)),
        _ => {}
    }
}

fn assert_no_value(printed: &str, document: &Value, label: &str) {
    let mut strings = BTreeSet::new();
    long_strings(document, &mut strings);
    assert!(!strings.is_empty(), "{label}: nothing to look for");
    for value in strings {
        assert!(!printed.contains(&value), "{label}: prints {value}");
    }
}

#[test]
fn no_accepted_message_prints_a_request_value() {
    for kind in ["request", "projection", "update", "cancel"] {
        for name in names(&format!("{kind}/valid")) {
            let document = fixture(&format!("{kind}/valid/{name}"));
            let input = bytes(&document);
            let printed = match kind {
                "request" => {
                    let parsed = parse_request(&input).unwrap();
                    format!("{parsed:?}|{parsed}")
                }
                "projection" => {
                    let parsed = parse_projection(&input).unwrap();
                    format!("{parsed:?}|{parsed}")
                }
                "update" => {
                    let parsed = parse_update(&input).unwrap();
                    format!("{parsed:?}|{parsed}")
                }
                _ => {
                    let parsed = parse_cancel(&input).unwrap();
                    format!("{parsed:?}|{parsed}")
                }
            };
            assert_no_value(&printed, &document, &format!("{kind}/{name}"));
        }
    }
    for name in names("result/valid") {
        let (document, pinned) = unwrap_result(&fixture(&format!("result/valid/{name}")));
        let projection = parse_projection(&bytes(&fixture(pinned.as_str().unwrap_or("")))).ok();
        let request = pinned
            .as_str()
            .filter(|p| p.starts_with("request/"))
            .map(|p| parse_request(&bytes(&fixture(p))).unwrap());
        let parsed = match (&request, &projection) {
            (Some(request), _) => parse_result(&bytes(&document), request),
            (None, Some(projection)) => parse_result(&bytes(&document), projection),
            _ => panic!("{name}"),
        }
        .unwrap();
        assert_no_value(
            &format!("{parsed:?}|{parsed}"),
            &document,
            &format!("result/{name}"),
        );
    }
    // A projection of the wasm fixture contains a 64-hex digest and a timestamp deadline.
    let request = fixture("request/valid/wasm-no-base");
    assert!(request["deadline"].as_str().unwrap().ends_with('Z'));
    assert!(
        request["projection"]["artifact_digest"]
            .as_str()
            .unwrap()
            .len()
            >= 64
    );
}

#[test]
fn no_refusal_or_error_prints_a_request_value() {
    let mut refused = Vec::new();
    for name in names("request/refused") {
        refused.push((
            format!("request/refused/{name}"),
            fixture(&format!("request/refused/{name}")),
        ));
    }
    for name in ["missing-path", "oversized-body", "non-JSON-body"] {
        refused.push((
            format!("legacy/{name}"),
            legacy_request(&legacy("invalid", name)),
        ));
    }
    // Credential-like values in every identifier a refusal carries.
    let marker = "canary_marker_text";
    let mut leaked = fixture("request/refused/oci-mode");
    leaked["activation_id"] = json!(marker);
    leaked["credential"]["activation"] = json!(marker);
    leaked["attempt_id"] = json!(format!("{marker}_attempt"));
    leaked["credential"]["attempt"] = json!(format!("{marker}_attempt"));
    leaked["projection"]["revision"] = json!(format!("{marker}_revision"));
    leaked["credential"]["revision"] = json!(format!("{marker}_revision"));
    refused.push(("leaked identifiers".into(), leaked));
    for (label, document) in refused {
        let rejection = parse_request(&bytes(&document)).unwrap_err();
        let Rejection::Refused(inner) = &rejection else {
            panic!("{label}")
        };
        let printed = format!(
            "{rejection:?}|{rejection}|{inner:?}|{inner}|{:?}|{}|{:?}",
            inner.refusal, inner.refusal, inner.attribution
        );
        assert_no_value(&printed, &document, &label);
        assert!(!printed.contains("canary_"), "{label}");
        // The serialized refusal keeps its contract fields and carries no request value.
        let serialized = serde_json::to_string(&inner.refusal).unwrap();
        assert_no_value(&serialized, &document, &label);
        for field in ["status", "code", "message", "detail", "retryable"] {
            assert!(
                serialized.contains(&format!("\"{field}\"")),
                "{label}: {field}"
            );
        }
    }
    // Malformed input never echoes a value, a field name or a serde message.
    let mut bad = fixture("request/valid/wasm-no-base");
    bad["activation_id"] = json!("canary_ secret value");
    bad["canary_unknown_field"] = json!("canary_marker_text");
    bad["credential"]["iss"] = json!(12345);
    for document in [bad, json!({"canary_marker_text": 1})] {
        let error = parse_request(&bytes(&document)).unwrap_err();
        let printed = format!("{error:?}|{error}");
        assert!(!printed.contains("canary_"), "{printed}");
    }
    let garbage = parse_request(b"{\"canary_marker_text\": nope").unwrap_err();
    assert!(!format!("{garbage:?}|{garbage}").contains("canary_"));
    let Err(error) = parse_update(b"{\"canary_marker_text\": 1}") else {
        panic!()
    };
    assert!(!format!("{error:?}|{error}").contains("canary_"));
    // Results: a credential-like revision never shows in a rejected or accepted result.
    let (mut document, pinned) = unwrap_result(&fixture("result/cross/wrong-revision"));
    document["identity"]["function_revision"] = json!(marker);
    let projection = parse_projection(&bytes(&fixture(pinned.as_str().unwrap()))).unwrap();
    let error = parse_result(&bytes(&document), &projection).unwrap_err();
    assert!(!format!("{error:?}|{error}").contains("canary_"));
}

// --- static checks of worker/src/api -------------------------------------------------------

fn api_sources() -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending = vec![root().join("worker/src/api")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let text = fs::read_to_string(&path).unwrap();
                found.push((path, text));
            }
        }
    }
    assert!(found.len() >= 8);
    found
}

/// The attribute lists of every `#[derive(...)]` in `source`, comments excluded.
fn derives(source: &str) -> Vec<String> {
    let code: String = source
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let mut found = Vec::new();
    let mut rest = code.as_str();
    while let Some(at) = rest.find("derive(") {
        let tail = &rest[at + "derive(".len()..];
        let end = tail.find(')').unwrap_or(tail.len());
        found.push(tail[..end].to_owned());
        rest = &tail[end..];
    }
    found
}

fn derives_debug(source: &str) -> bool {
    derives(source).iter().any(|list| {
        list.split(',')
            .any(|item| item.trim().rsplit("::").next() == Some("Debug"))
    })
}

#[test]
fn no_type_in_the_api_derives_debug() {
    for (path, source) in api_sources() {
        assert!(!derives_debug(&source), "{}", path.display());
    }
    // The check itself.
    for bad in [
        "#[derive(Debug)]\nstruct A;",
        "#[derive(Clone, Debug, PartialEq)]\nstruct A;",
        "#[derive(\n    Clone,\n    Debug,\n)]\nstruct A;",
        "#[derive(std::fmt::Debug)]\nstruct A;",
        "#[cfg_attr(test, derive(Debug))]\nstruct A;",
        "#[derive(Clone)]\n#[derive(Debug)]\nstruct A;",
    ] {
        assert!(derives_debug(bad), "{bad}");
    }
    for good in [
        "#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]\nstruct A;",
        "// #[derive(Debug)]\nstruct A;",
        "impl std::fmt::Debug for A {}",
        "#[derive(DebugLike)]\nstruct A;",
    ] {
        assert!(!derives_debug(good), "{good}");
    }
}

#[test]
fn the_api_has_no_gating_allowance_or_schema_evaluation() {
    for (path, source) in api_sources() {
        let label = path.display().to_string();
        assert!(!source.contains("allow("), "{label}: no allow attribute");
        assert!(!source.contains("expect("), "{label}: no expect attribute");
        assert!(
            !source.contains("jsonschema"),
            "{label}: no schema evaluation"
        );
        assert!(!source.contains("wasmtime::"), "{label}: no engine access");
        assert!(!source.contains("tests/"), "{label}");
        // Only a `#[cfg(test)] mod tests;` declaration is gated.
        for (at, _) in source.match_indices("cfg(") {
            let after = &source[at..];
            assert!(
                after.starts_with("cfg(test)]\nmod tests;"),
                "{label}: gated code at byte {at}"
            );
        }
    }
}

#[test]
fn jsonschema_stays_a_dev_dependency() {
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(root().join("worker/Cargo.toml")).unwrap()).unwrap();
    assert!(manifest["dev-dependencies"].get("jsonschema").is_some());
    for table in ["dependencies", "build-dependencies"] {
        assert!(
            manifest
                .get(table)
                .is_none_or(|t| t.get("jsonschema").is_none()),
            "{table}"
        );
    }
}

#[test]
fn the_spec_states_the_grant_divergence_and_the_provenance() {
    let spec = fs::read_to_string(contract_dir().join("execution-api-v1.md")).unwrap();
    let section = spec
        .split("## Grant versions")
        .nth(1)
        .unwrap()
        .lines()
        .find(|l| l.contains("@0.1.0"))
        .unwrap();
    assert!(section.contains("@0.2.0") && section.contains("capability-grants"));
    assert_eq!(
        section.matches(". ").count() + usize::from(section.ends_with('.')),
        1,
        "one sentence: {section}"
    );
    for needle in [
        "a19f40ab9e160cd522c110efd50c30b2f5e0d708",
        "03121286b0c7ec716304d51a41b866477f4fc21671d95ebeafb8bf9da70e8960",
        "9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0",
        "7f93939d0cc4b87d0bb6ef9af145a8b68a060ab918dc96ffc6247086a2b25dd2",
        "docs/contracts/serverless/functions-api/openapi.v1.yaml",
    ] {
        assert!(spec.contains(needle), "{needle}");
    }
    for (path, hash) in [
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
    ] {
        assert!(spec.contains(path) && spec.contains(hash), "{path}");
    }
}

// --- integers: the schema's `integer` type, bounded at 2^53 - 1 ---------------------------

/// Edits one field of a fixture and reports whether the schema and the runtime parser accept it.
fn integer_verdict(kind: &str, name: &str, pointer: &str, number: Value) -> (bool, bool) {
    let (mut document, pinned) = if kind == "result" {
        unwrap_result(&fixture(name))
    } else {
        (fixture(name), Value::Null)
    };
    *document.pointer_mut(pointer).unwrap() = number;
    let pinned = (kind == "result").then_some(&pinned);
    let (schema_valid, verdict) = judge(kind, &document, pinned);
    (
        schema_valid,
        matches!(verdict, Verdict::Accepted(_) | Verdict::Refused(_)),
    )
}

#[test]
fn integral_numbers_are_accepted_by_schema_and_parser_alike() {
    let max: Value = serde_json::from_str("9007199254740991").unwrap();
    let past_max: Value = serde_json::from_str("9007199254740992").unwrap();
    let past_u64: Value = serde_json::from_str("18446744073709551616").unwrap();
    // (message, fixture, field, whether zero is allowed)
    let fields = [
        (
            "request",
            "request/valid/wasm-no-base",
            "/attempt_number",
            false,
        ),
        (
            "request",
            "request/valid/wasm-no-base",
            "/projection/budgets/fuel",
            false,
        ),
        ("update", "update/valid/progress", "/sequence", false),
        (
            "update",
            "update/valid/progress-open-ended",
            "/update/completed",
            true,
        ),
        (
            "result",
            "result/valid/success-none-base",
            "/attempt_number",
            false,
        ),
    ];
    for (kind, name, pointer, zero_allowed) in fields {
        let cases = [
            ("1.0", json!(2.0), true),
            ("1e1", json!(1e1), true),
            ("max", max.clone(), true),
            ("past max", past_max.clone(), false),
            ("past u64", past_u64.clone(), false),
            ("fraction", json!(2.5), false),
            ("negative", json!(-1), false),
            ("zero", json!(0), zero_allowed),
        ];
        for (label, number, expected) in cases {
            let (schema, parser) = integer_verdict(kind, name, pointer, number);
            assert_eq!(schema, expected, "{name} {pointer} {label}: schema");
            assert_eq!(parser, expected, "{name} {pointer} {label}: parser");
        }
    }
}

#[test]
fn a_status_code_is_an_integral_number_from_100_to_599() {
    for (label, number, expected) in [
        ("whole", json!(200), true),
        ("float whole", json!(200.0), true),
        ("below", json!(99.0), false),
        ("above", json!(600), false),
        ("fraction", json!(200.5), false),
    ] {
        let (schema, parser) = integer_verdict(
            "result",
            "result/valid/success-none-base",
            "/result/statusCode",
            number,
        );
        assert_eq!(schema, expected, "{label}: schema");
        assert_eq!(parser, expected, "{label}: parser");
    }
}

#[test]
fn a_304_result_with_a_body_is_malformed_in_schema_and_parser() {
    let (document, pinned) = unwrap_result(&fixture("result/invalid/body-on-304"));
    let (schema, verdict) = judge("result", &document, Some(&pinned));
    assert!(!schema, "the schema accepts a 304 with a body");
    assert_eq!(verdict, Verdict::Malformed);
    // The same body is allowed on any other status.
    let (mut document, pinned) = unwrap_result(&fixture("result/invalid/body-on-304"));
    document["result"]["statusCode"] = json!(200);
    let (schema, verdict) = judge("result", &document, Some(&pinned));
    assert!(schema);
    assert!(matches!(verdict, Verdict::Accepted(_)));
}

#[test]
fn transport_and_control_refusals_use_the_typed_failure_envelope() {
    for (status, code, reason, message) in [
        (
            400,
            "invalid_request",
            "malformed_request",
            "The request is malformed.",
        ),
        (
            404,
            "not_found",
            "not_found",
            "The operation or attempt was not found.",
        ),
        (
            409,
            "stale_update",
            "stale_update",
            "The update sequence is stale.",
        ),
    ] {
        let value = json!({"status":status,"code":code,"message":message,
            "detail":{"reason":reason},"retryable":false});
        let refusal: api::Refusal = serde_json::from_value(value.clone()).expect("typed refusal");
        assert_eq!(refusal.status(), status);
        assert_eq!(serde_json::to_value(refusal).unwrap(), value);
    }
}
