//! Published contract only: no runtime loader, qualification or enablement.
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const LANGUAGES: &[&str] = &["javascript", "python", "go"];
const DIAGNOSTICS: &[&str] = &[
    "WASM-NATIVE-LIBRARY",
    "WASM-GPU",
    "WASM-PROCESS-SPAWN",
    "WASM-INSTALL-HOOK",
];
const EVIDENCE: &[&str] = &["compatibility", "adversarial", "leakage", "crashRecovery"];
const VERSIONS: &[&str] = &[
    "wasmtimeVersion",
    "wasiVersion",
    "toolchainVersion",
    "packageCatalogueVersion",
];
const SECTIONS: &[&str] = &[
    "Versioning and compatibility",
    "Fields",
    "Diagnostics",
    "Enablement evidence",
    "Seeded profiles",
];
const FIELDS: &[&str] = &[
    "id",
    "status",
    "wasmtimeVersion",
    "wasiVersion",
    "toolchainVersion",
    "packageCatalogueVersion",
    "allowedInterfaces",
    "testedExamples",
    "diagnostics",
    "eolPolicy",
    "evidence",
];

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts")
}

fn read_json(path: PathBuf) -> Value {
    serde_json::from_str(
        &fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn schema() -> Value {
    let schema = read_json(contracts().join("language-profile-matrix.schema.json"));
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    local_refs(&schema, &schema);
    schema
}

fn local_refs(root: &Value, node: &Value) {
    match node {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                let reference = reference.as_str().expect("string $ref");
                assert!(
                    reference.starts_with("#/$defs/"),
                    "local $refs only: {reference}"
                );
                assert!(
                    root.pointer(&reference[1..]).is_some(),
                    "unresolved local $ref: {reference}"
                );
            }
            for child in object.values() {
                local_refs(root, child);
            }
        }
        Value::Array(children) => {
            for child in children {
                local_refs(root, child);
            }
        }
        _ => {}
    }
}

fn validate(schema: &Value, matrix: &Value) -> Result<(), String> {
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(schema)
        .map_err(|e| format!("schema: {e}"))?;
    let errors: Vec<_> = validator
        .iter_errors(matrix)
        .map(|e| {
            format!(
                "schema rule {} at {}: {e}; constraint {}",
                e.schema_path(),
                e.instance_path(),
                schema
                    .pointer(&e.schema_path().to_string())
                    .unwrap_or(&Value::Null)
            )
        })
        .collect();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    let mut resolve = wit_parser::Resolve::default();
    let (package, _) = resolve
        .push_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wit/host.wit"))
        .map_err(|e| format!("allowedInterfaces: parse authoritative WIT: {e}"))?;
    let package = &resolve.packages[package];
    let interfaces: BTreeSet<_> = package
        .interfaces
        .keys()
        .map(|name| package.name.interface_id(name))
        .collect();
    let mut ids = BTreeSet::new();
    for (language, profile) in matrix["profiles"].as_object().unwrap() {
        if !ids.insert(profile["id"].as_str().unwrap()) {
            return Err(format!("unique profile id: {language}"));
        }
        for interface in profile["allowedInterfaces"].as_array().unwrap() {
            let interface = interface.as_str().unwrap();
            if interface.contains('*') || !interfaces.contains(interface) {
                return Err(format!(
                    "allowedInterfaces: {language}: unknown or wildcard WIT interface {interface}"
                ));
            }
        }
    }
    Ok(())
}

fn fixture(kind: &str, name: &str) -> Value {
    read_json(
        contracts()
            .join("fixtures/language-profile-matrix")
            .join(kind)
            .join(format!("{name}.json")),
    )
}

fn rejected(schema: &Value, name: &str, rule: &str) {
    let error = validate(schema, &fixture("invalid", name)).expect_err(name);
    assert!(error.contains(rule), "{name}: expected {rule}, got {error}");
}

fn field_keys(matrix: &Value) -> BTreeSet<String> {
    fn collect(node: &Value, keys: &mut BTreeSet<String>) {
        match node {
            Value::Object(object) => {
                for (key, child) in object {
                    keys.insert(key.clone());
                    collect(child, keys);
                }
            }
            Value::Array(children) => {
                for child in children {
                    collect(child, keys);
                }
            }
            _ => {}
        }
    }
    let mut keys = BTreeSet::new();
    collect(matrix, &mut keys);
    // Evidence is intentionally absent from the seeds, but its fields need docs.
    keys.extend(
        EVIDENCE
            .iter()
            .chain(["artifactId", "digest"].iter())
            .map(|key| (*key).to_owned()),
    );
    keys
}

fn validate_document(matrix: &Value, path: &Path) -> Result<(), String> {
    let document = fs::read_to_string(path)
        .map_err(|e| format!("documentation file {}: {e}", path.display()))?;
    for section in SECTIONS {
        if !document.lines().any(|line| line == format!("## {section}")) {
            return Err(format!("documentation section: {section}"));
        }
    }
    for key in field_keys(matrix) {
        if !document.contains(&format!("`{key}`")) {
            return Err(format!("documentation field: {key}"));
        }
    }
    for code in DIAGNOSTICS {
        if !document.contains(&format!("`{code}`")) {
            return Err(format!("documentation diagnostic: {code}"));
        }
    }
    Ok(())
}

#[test]
fn language_profile_matrix_documentation_contract() {
    let matrix = read_json(contracts().join("language-profile-matrix.json"));
    validate_document(&matrix, &contracts().join("language-profile-matrix.md")).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contracts");
    validate_document(&matrix, &fixtures.join("language-profile-matrix-valid.md")).unwrap();
    for (category, items) in [
        (
            "section",
            SECTIONS.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        ),
        ("field", field_keys(&matrix).into_iter().collect()),
        (
            "diagnostic",
            DIAGNOSTICS.iter().map(|s| (*s).to_owned()).collect(),
        ),
    ] {
        for item in items {
            let name = format!(
                "language-profile-matrix-missing-{category}-{}.md",
                item.replace(' ', "-")
            );
            let error = validate_document(&matrix, &fixtures.join(&name)).expect_err(&name);
            assert!(
                error.contains(&format!("{category}: {item}")),
                "{name}: {error}"
            );
        }
    }
    // A nonexistent file is a read error, never an empty/optional contract.
    let absent = fixtures.join("language-profile-matrix-absent.md");
    assert!(!absent.exists(), "missing-file witness must be absent");
    assert!(
        validate_document(&matrix, &absent)
            .unwrap_err()
            .contains("documentation file")
    );
}

#[test]
fn language_profile_matrix_json_contract() {
    let schema = schema();
    let matrix = read_json(contracts().join("language-profile-matrix.json"));
    validate(&schema, &matrix).unwrap();
    assert!(
        !matrix["matrixVersion"].as_str().unwrap().trim().is_empty(),
        "matrixVersion"
    );
    let profiles = matrix["profiles"].as_object().unwrap();
    assert_eq!(
        profiles.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        LANGUAGES.iter().copied().collect()
    );
    let workspace: toml::Value =
        toml::from_str(&fs::read_to_string(contracts().join("../../Cargo.toml")).unwrap()).unwrap();
    let pin = workspace["workspace"]["dependencies"]["wasmtime"]["version"]
        .as_str()
        .unwrap()
        .strip_prefix('=')
        .expect("exact workspace Wasmtime pin");
    for language in LANGUAGES {
        let profile = &profiles[*language];
        assert_eq!(profile["status"], "disabled", "seed {language}: status");
        assert_eq!(
            profile["evidence"],
            serde_json::json!({}),
            "seed {language}: no evidence"
        );
        assert_eq!(
            profile["wasmtimeVersion"], pin,
            "seed {language}: workspace Wasmtime pin"
        );
        for field in FIELDS {
            assert!(profile.get(field).is_some(), "{language}: required {field}");
        }
        for field in FIELDS {
            rejected(&schema, &format!("missing-{language}-{field}"), field);
        }
        let codes = profile["diagnostics"].as_array().unwrap();
        assert_eq!(
            codes.len(),
            DIAGNOSTICS.len(),
            "{language}: unique diagnostics"
        );
        assert_eq!(
            codes
                .iter()
                .map(|code| code.as_str().unwrap())
                .collect::<BTreeSet<_>>(),
            DIAGNOSTICS.iter().copied().collect()
        );
        for code in DIAGNOSTICS {
            rejected(&schema, &format!("missing-{language}-{code}"), code);
        }
        rejected(
            &schema,
            &format!("duplicate-{language}-diagnostic"),
            "uniqueItems",
        );
        rejected(&schema, &format!("missing-profile-{language}"), language);
    }
    rejected(&schema, "missing-profiles", "profiles");
    rejected(&schema, "missing-matrixVersion", "matrixVersion");
    rejected(&schema, "empty-matrixVersion", "minLength");
    rejected(&schema, "blank-matrixVersion", "pattern");
    let enabled = fixture("valid", "enabled-all-evidence");
    validate(&schema, &enabled).unwrap();
    // The enablement gate applies to every language, not just the enabled fixture.
    for language in LANGUAGES {
        let mut candidate = matrix.clone();
        let mut profile = enabled["profiles"]["javascript"].clone();
        profile["id"] = matrix["profiles"][*language]["id"].clone();
        candidate["profiles"][*language] = profile;
        validate(&schema, &candidate).unwrap();
        candidate["profiles"][*language]["evidence"] = serde_json::json!({});
        assert!(
            validate(&schema, &candidate)
                .unwrap_err()
                .contains("evidence"),
            "{language}: enabled-requires-evidence"
        );
    }
    rejected(&schema, "enabled-seed-no-evidence", "evidence");
    rejected(&schema, "enabled-no-evidence", "evidence");
    for kind in EVIDENCE {
        rejected(&schema, &format!("enabled-missing-{kind}"), kind);
        for status in ["failed", "pending"] {
            rejected(&schema, &format!("enabled-{kind}-{status}"), kind);
        }
        for field in ["status", "artifactId", "digest"] {
            rejected(&schema, &format!("enabled-{kind}-missing-{field}"), field);
        }
    }
    rejected(&schema, "enabled-bad-sha256", "digest");
    rejected(&schema, "enabled-empty-artifactId", "artifactId");
    rejected(&schema, "enabled-empty-testedExamples", "testedExamples");
    for field in VERSIONS {
        rejected(&schema, &format!("enabled-unqualified-{field}"), field);
        rejected(&schema, &format!("enabled-floating-{field}"), field);
    }
    for name in [
        "wildcard-interface",
        "unknown-interface",
        "wrong-interface-version",
        "unversioned-interface",
        "duplicate-interface",
    ] {
        rejected(&schema, name, "allowedInterfaces");
    }
    rejected(&schema, "duplicate-profile-id", "unique profile id");

    // Prove the schema itself enforces evidence, independently of Rust rules.
    let no_evidence = fixture("invalid", "enabled-no-evidence");
    assert!(
        !jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&no_evidence)
    );
    let mut weakened = schema.clone();
    let removed = weakened["$defs"]["profile"]["allOf"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    assert_eq!(removed["description"], "enabled-requires-evidence");
    if weakened["$defs"]["profile"]["allOf"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        weakened["$defs"]["profile"]
            .as_object_mut()
            .unwrap()
            .remove("allOf");
    }
    assert!(
        jsonschema::validator_for(&weakened)
            .unwrap()
            .is_valid(&no_evidence),
        "enabled-requires-evidence weakening witness"
    );
}
