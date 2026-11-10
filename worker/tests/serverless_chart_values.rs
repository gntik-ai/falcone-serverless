//! Published chart-values validation only; no rendering or runtime enablement.
use serde_json::Value;
use std::{fs, path::PathBuf};

fn contract() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts/serverless-chart-values")
}

fn read_json(path: &std::path::Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.contains(&b'\r') || !bytes.ends_with(b"\n") || bytes.ends_with(b"\n\n") {
        return Err(format!(
            "{}: LF and one final newline required",
            path.display()
        ));
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn local_refs(root: &Value, node: &Value) -> Result<(), String> {
    match node {
        Value::Object(object) => {
            for keyword in ["$ref", "$dynamicRef"] {
                if let Some(reference) = object.get(keyword) {
                    let reference = reference.as_str().ok_or("non-string reference")?;
                    if !reference.starts_with("#/$defs/") || root.pointer(&reference[1..]).is_none()
                    {
                        return Err(format!("local resolved $defs refs only: {reference}"));
                    }
                }
            }
            if object.get("type").and_then(Value::as_str) == Some("object")
                && object.get("additionalProperties") != Some(&Value::Bool(false))
            {
                return Err("every object must have additionalProperties: false".into());
            }
            for child in object.values() {
                local_refs(root, child)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                local_refs(root, child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn compile(schema: &Value) -> Result<jsonschema::Validator, String> {
    if schema["$schema"] != "https://json-schema.org/draft/2020-12/schema"
        || schema["$id"] != "ctr-serverless-chart-values/v1"
    {
        return Err("contract must identify ctr-serverless-chart-values/v1, Draft 2020-12".into());
    }
    local_refs(schema, schema)?;
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(schema)
        .map_err(|e| format!("schema does not compile: {e}"))
}

fn fixtures(kind: &str) -> Result<Vec<(String, Value)>, String> {
    fixtures_at(&contract().join("fixtures").join(kind))
}

fn fixtures_at(dir: &std::path::Path) -> Result<Vec<(String, Value)>, String> {
    let mut fixtures = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            return Err(format!("unexpected fixture: {}", path.display()));
        }
        fixtures.push((
            path.file_stem().unwrap().to_str().unwrap().to_owned(),
            read_json(&path)?,
        ));
    }
    if fixtures.is_empty() {
        return Err(format!("empty fixture set: {}", dir.display()));
    }
    fixtures.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(fixtures)
}

#[test]
fn published_valid_values_are_accepted() {
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    let validator = compile(&schema).unwrap();
    let fixtures = fixtures("valid").unwrap();
    let names: std::collections::BTreeSet<_> =
        fixtures.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        std::collections::BTreeSet::from([
            "oci-absent",
            "oci-disabled",
            "oci-default-off",
            "oci-enabled",
            "docker-hub-normalized",
            "bounds-minimum",
            "bounds-maximum"
        ])
    );
    assert_eq!(
        schema["properties"]["oci"]["properties"]["enabled"]["default"],
        false
    );
    for (name, value) in fixtures {
        let errors: Vec<_> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
}

// Expected paths name the intended rule, so an unrelated rejection cannot pass.
const INVALID: &[(&str, &str, &str)] = &[
    ("oci-missing-namespaces", "", "then/required"),
    ("oci-missing-pull-secret", "", "then/required"),
    ("oci-empty-namespaces", "/watchedOCINamespaces", "minItems"),
    (
        "oci-duplicate-namespaces",
        "/watchedOCINamespaces",
        "uniqueItems",
    ),
    ("oci-bad-namespace", "/watchedOCINamespaces/0", "pattern"),
    (
        "oci-empty-pull-secret",
        "/platformRegistryPullSecretName",
        "minLength",
    ),
    (
        "oci-pull-secret-value",
        "/platformRegistryPullSecretName",
        "type",
    ),
    (
        "oci-mirror-registry",
        "/oci/intakeMirrorRepository",
        "pattern",
    ),
    ("oci-disabled-watch", "/watchedOCINamespaces", "maxItems"),
    ("oci-absent-watch", "/watchedOCINamespaces", "maxItems"),
    (
        "controller-replicas-below",
        "/controller/replicas",
        "minimum",
    ),
    (
        "controller-replicas-above",
        "/controller/replicas",
        "maximum",
    ),
    (
        "controller-replicas-fraction",
        "/controller/replicas",
        "type",
    ),
    ("worker-replicas-below", "/worker/replicas", "minimum"),
    ("worker-replicas-above", "/worker/replicas", "maximum"),
    ("worker-replicas-fraction", "/worker/replicas", "type"),
    ("compiler-replicas-below", "/compiler/replicas", "minimum"),
    ("compiler-replicas-above", "/compiler/replicas", "maximum"),
    ("compiler-replicas-fraction", "/compiler/replicas", "type"),
    (
        "poolLimits-maxPools-below",
        "/poolLimits/maxPools",
        "minimum",
    ),
    (
        "poolLimits-maxPools-above",
        "/poolLimits/maxPools",
        "maximum",
    ),
    (
        "poolLimits-maxPools-fraction",
        "/poolLimits/maxPools",
        "type",
    ),
    (
        "poolLimits-maxReplicasPerPool-below",
        "/poolLimits/maxReplicasPerPool",
        "minimum",
    ),
    (
        "poolLimits-maxReplicasPerPool-above",
        "/poolLimits/maxReplicasPerPool",
        "maximum",
    ),
    (
        "poolLimits-maxReplicasPerPool-fraction",
        "/poolLimits/maxReplicasPerPool",
        "type",
    ),
    ("tag-only", "/controller/image", "required"),
    ("missing-digest", "/worker/image", "required"),
    ("tag-key", "/compiler/image", "additionalProperties"),
    ("ghcr", "/controller/image/repository", "pattern"),
    ("quay", "/worker/image/repository", "pattern"),
    ("localhost", "/compiler/image/repository", "pattern"),
    ("registry-port", "/controller/image/repository", "pattern"),
    ("registry-dot", "/controller/image/repository", "pattern"),
    (
        "unqualified-nested-path",
        "/controller/image/repository",
        "pattern",
    ),
    ("docker-port", "/controller/image/repository", "pattern"),
    ("bare-localhost", "/controller/image/repository", "pattern"),
    ("uppercase", "/controller/image/repository", "pattern"),
    ("bad-component", "/controller/image/repository", "pattern"),
    ("repository-tag", "/controller/image/repository", "pattern"),
    (
        "repository-digest",
        "/controller/image/repository",
        "pattern",
    ),
    ("short-digest", "/controller/image/digest", "pattern"),
    ("uppercase-digest", "/controller/image/digest", "pattern"),
    ("privileged", "", "additionalProperties"),
    ("hostNetwork", "", "additionalProperties"),
    ("clusterRole", "", "additionalProperties"),
    ("runtimeClassName", "", "additionalProperties"),
    ("namespace-create", "/namespaceMode", "const"),
    ("namespace-shared", "/namespaceMode", "const"),
    ("unknown-top-level", "", "additionalProperties"),
    ("nested-privileged", "/worker", "additionalProperties"),
    ("secret-value", "", "additionalProperties"),
    ("http-endpoint", "/falconeStatusEndpoint", "pattern"),
    (
        "endpoint-credentials",
        "/falconeDesiredStateEndpoint",
        "pattern",
    ),
];

fn check_invalid(schema: &Value) -> Result<(), String> {
    let validator = compile(schema)?;
    let fixtures = fixtures("invalid")?;
    let names: std::collections::BTreeSet<_> = fixtures.iter().map(|(n, _)| n.as_str()).collect();
    if names != INVALID.iter().map(|(n, _, _)| *n).collect() {
        return Err("invalid fixtures must match the intended-rule inventory".into());
    }
    for (name, value) in fixtures {
        let (_, path, keyword) = INVALID.iter().find(|(n, _, _)| *n == name).unwrap();
        let errors: Vec<_> = validator.iter_errors(&value).collect();
        if !errors.iter().any(|e| {
            e.instance_path().to_string() == *path
                && e.schema_path()
                    .to_string()
                    .ends_with(&format!("/{keyword}"))
        }) {
            return Err(format!(
                "{name}: expected {path} /{keyword}, got {errors:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn each_invalid_fixture_fails_on_its_intended_rule() {
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    check_invalid(&schema).unwrap();
}

#[test]
fn unconditional_required_errors_do_not_satisfy_oci_rules() {
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    for property in ["watchedOCINamespaces", "platformRegistryPullSecretName"] {
        let mut misplaced = schema.clone();
        misplaced["then"]["required"]
            .as_array_mut()
            .unwrap()
            .retain(|value| value != property);
        misplaced["required"]
            .as_array_mut()
            .unwrap()
            .push(property.into());
        assert!(
            check_invalid(&misplaced).is_err(),
            "root required must not substitute for OCI then/required: {property}"
        );
    }
}

#[test]
fn weakened_security_rules_are_detected() {
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    for (pointer, keyword, witness) in [
        ("/$defs/image", "required", "missing-digest"),
        ("/$defs/repository", "pattern", "ghcr"),
        ("", "additionalProperties", "unknown-top-level"),
    ] {
        let mut weakened = schema.clone();
        weakened
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(keyword)
            .unwrap();
        let value =
            read_json(&contract().join(format!("fixtures/invalid/{witness}.json"))).unwrap();
        // The fixture witnesses the exact rule removed, independently of the harness.
        assert!(!jsonschema::validator_for(&schema).unwrap().is_valid(&value));
        assert!(
            jsonschema::validator_for(&weakened)
                .unwrap()
                .is_valid(&value),
            "weakening witness: {witness}"
        );
        assert!(
            check_invalid(&weakened).is_err(),
            "undetected weakening: {witness}"
        );
    }
}

#[test]
fn external_or_unresolved_refs_and_invalid_schemas_fail_closed() {
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    for reference in [
        "https://fs-test.invalid/image.json",
        "file:///image.json",
        "#/$defs/missing",
    ] {
        let mut broken = schema.clone();
        broken["$defs"]["worker"]["properties"]["image"]["$ref"] = reference.into();
        let Err(error) = compile(&broken) else {
            panic!("reference must fail before resolution");
        };
        assert!(error.contains("local resolved $defs refs only"), "{error}");
    }
    let mut broken = schema.clone();
    broken["$defs"]["repository"]["pattern"] = "[".into();
    assert!(
        compile(&broken)
            .err()
            .unwrap()
            .contains("schema does not compile")
    );
}

#[test]
fn missing_empty_and_unparsable_fixtures_fail_closed() {
    let dir = std::env::temp_dir().join(format!("chart-values-fixtures-{}", std::process::id()));
    assert!(!dir.exists(), "isolated scratch directory");
    assert!(fixtures_at(&dir).is_err());
    fs::create_dir(&dir).unwrap();
    assert!(fixtures_at(&dir).unwrap_err().contains("empty fixture set"));
    fs::write(dir.join("broken.json"), b"{\n").unwrap();
    assert!(fixtures_at(&dir).unwrap_err().contains("broken.json"));
    fs::remove_dir_all(dir).unwrap();
}
