//! ctr-serverless-chart-values v1: validate raw values, without normalisation.
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn contract() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts/serverless-chart-values")
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn local_refs(root: &Value, node: &Value) -> Result<(), String> {
    match node {
        Value::Object(object) => {
            for key in ["$ref", "$dynamicRef", "$recursiveRef"] {
                if let Some(reference) = object.get(key) {
                    let reference = reference.as_str().ok_or("non-string reference")?;
                    if !reference.starts_with("#/$defs/") || root.pointer(&reference[1..]).is_none()
                    {
                        return Err(format!("non-local or unresolved {key}: {reference}"));
                    }
                }
            }
            for child in object.values() {
                local_refs(root, child)?;
            }
        }
        Value::Array(children) => {
            for child in children {
                local_refs(root, child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn compile(schema: &Value) -> Result<jsonschema::Validator, String> {
    if schema["$schema"] != "https://json-schema.org/draft/2020-12/schema" {
        return Err("expected JSON Schema 2020-12".into());
    }
    local_refs(schema, schema)?;
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(schema)
        .map_err(|e| format!("schema: {e}"))
}

fn fixtures(path: &Path) -> Result<Vec<(PathBuf, Value)>, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    entries.sort();
    if entries.is_empty() {
        return Err(format!("{}: empty fixture directory", path.display()));
    }
    entries
        .into_iter()
        .map(|path| {
            if path.extension().and_then(|s| s.to_str()) != Some("json") || !path.is_file() {
                return Err(format!("{}: expected JSON fixture file", path.display()));
            }
            let value = read_json(&path)?;
            Ok((path, value))
        })
        .collect()
}

fn check_fixtures(schema: &Value, root: &Path) -> Result<(), String> {
    let validator = compile(schema)?;
    for (kind, accepted) in [("valid", true), ("invalid", false)] {
        for (path, value) in fixtures(&root.join(kind))? {
            let errors: Vec<_> = validator
                .iter_errors(&value)
                .map(|e| format!("schema {} at {}: {e}", e.schema_path(), e.instance_path()))
                .collect();
            if errors.is_empty() != accepted {
                return Err(format!(
                    "{}: expected accepted={accepted}: {}",
                    path.display(),
                    errors.join("\n")
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn raw_chart_values_fixtures_obey_contract() {
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    check_fixtures(&schema, &contract().join("fixtures")).unwrap();
}

fn leaf_fields(
    root: &Value,
    node: &Value,
    prefix: &str,
    depth: usize,
    fields: &mut std::collections::BTreeSet<String>,
) {
    assert!(depth < 32, "cyclic or excessively nested schema");
    if let Some(reference) = node["$ref"].as_str() {
        leaf_fields(
            root,
            root.pointer(&reference[1..]).unwrap(),
            prefix,
            depth + 1,
            fields,
        );
    } else if let Some(properties) = node["properties"].as_object() {
        for (name, child) in properties {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            leaf_fields(root, child, &path, depth + 1, fields);
        }
    } else {
        assert!(fields.insert(prefix.to_owned()), "duplicate field {prefix}");
    }
}

#[test]
fn schema_fields_exactly_match_install_values_table() {
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    compile(&schema).unwrap();
    let mut fields = std::collections::BTreeSet::new();
    leaf_fields(&schema, &schema, "", 0, &mut fields);
    let document =
        fs::read_to_string(contract().join("../../operations/install-prerequisites.md")).unwrap();
    let table = document
        .split_once("## Values the controller reads\n")
        .unwrap()
        .1
        .split("\n## ")
        .next()
        .unwrap();
    let mut documented = std::collections::BTreeSet::new();
    for line in table.lines().filter(|line| line.starts_with("| `")) {
        let field = line.split('|').nth(1).unwrap().trim().trim_matches('`');
        assert!(
            documented.insert(field.to_owned()),
            "duplicate documented field {field}"
        );
    }
    assert_eq!(
        fields, documented,
        "schema and install values table must not drift"
    );
    // The preflight names are fixed, even if both docs and schema are edited.
    for field in [
        "watchedPoolNamespaces",
        "controllerNamespace",
        "namespaceMode",
        "prerequisites.gatewayRoute.name",
        "prerequisites.storage.name",
        "prerequisites.broker.name",
        "prerequisites.rbac.controllerRoleBinding",
        "prerequisites.networkPolicyEnforcement.name",
        "prerequisites.existingSCC.name",
    ] {
        assert!(fields.contains(field), "missing Go consumer field {field}");
    }
    let config = fs::read_to_string(contract().join("../controller-config.md")).unwrap();
    for line in config
        .split_once("## Configuration\n")
        .unwrap()
        .1
        .split("\n## ")
        .next()
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("| `"))
    {
        let field = line.split('|').nth(1).unwrap().trim().trim_matches('`');
        assert!(
            schema["properties"].get(field).is_some(),
            "missing controller-config key {field}"
        );
    }
}

#[test]
fn documented_numeric_bounds_match_schema() {
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    let readme = fs::read_to_string(contract().join("README.md")).unwrap();
    let table = readme
        .split_once("## Numeric bounds\n")
        .unwrap()
        .1
        .split("\n## ")
        .next()
        .unwrap();
    let mut count = 0;
    for line in table.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<_> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(cells.len(), 3);
        let path = cells[0].trim_matches('`');
        let mut node = &schema;
        for part in path.split('.') {
            node = &node["properties"][part];
        }
        assert_eq!(node["type"], "integer", "{path}");
        for (key, cell) in [("minimum", cells[1]), ("maximum", cells[2])] {
            assert_eq!(node[key], cell.parse::<u64>().unwrap(), "{path}: {key}");
        }
        count += 1;
    }
    assert_eq!(count, 10, "all numeric settings have documented bounds");
}

#[test]
fn every_object_rejects_unknown_fields() {
    fn closed(node: &Value) {
        match node {
            Value::Object(object) => {
                if node["type"] == "object" {
                    assert_eq!(
                        node["additionalProperties"], false,
                        "object must fail closed: {node}"
                    );
                }
                for child in object.values() {
                    closed(child);
                }
            }
            Value::Array(children) => {
                for child in children {
                    closed(child);
                }
            }
            _ => {}
        }
    }
    closed(&read_json(&contract().join("chart-values.v1.schema.json")).unwrap());
}

#[test]
fn fixtures_detect_schema_policy_weakening() {
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    for mutation in ["digest", "unknown-fields", "registry"] {
        let mut weakened = schema.clone();
        match mutation {
            "digest" => weakened["$defs"]["image"]["required"]
                .as_array_mut()
                .unwrap()
                .retain(|value| value != "digest"),
            "unknown-fields" => weakened["additionalProperties"] = Value::Bool(true),
            "registry" => {
                weakened["$defs"]["image"]["properties"]["repository"]["pattern"] =
                    Value::String("^.*$".into())
            }
            _ => unreachable!(),
        }
        let validator = compile(&weakened).unwrap();
        let witnesses: Vec<_> = fixtures(&contract().join("fixtures/invalid"))
            .unwrap()
            .into_iter()
            .filter(|(_, value)| validator.is_valid(value))
            .collect();
        assert!(
            !witnesses.is_empty(),
            "no invalid fixture detects {mutation}"
        );
        let error = check_fixtures(&weakened, &contract().join("fixtures")).unwrap_err();
        assert!(
            error.contains("expected accepted=false"),
            "{mutation}: {error}"
        );
    }
}

#[test]
fn fixture_harness_fails_closed() {
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let scratch =
        Scratch(std::env::temp_dir().join(format!("chart-values-harness-{}", std::process::id())));
    fs::create_dir(&scratch.0).unwrap();
    let root = &scratch.0;
    let schema = read_json(&contract().join("chart-values.v1.schema.json")).unwrap();
    assert!(check_fixtures(&schema, root).unwrap_err().contains("valid"));
    fs::create_dir(root.join("valid")).unwrap();
    assert!(
        check_fixtures(&schema, root)
            .unwrap_err()
            .contains("empty fixture directory")
    );
    fs::copy(
        contract().join("fixtures/valid/docker-hub.json"),
        root.join("valid/good.json"),
    )
    .unwrap();
    assert!(
        check_fixtures(&schema, root)
            .unwrap_err()
            .contains("invalid")
    );
    fs::create_dir(root.join("invalid")).unwrap();
    assert!(
        check_fixtures(&schema, root)
            .unwrap_err()
            .contains("empty fixture directory")
    );
    fs::write(root.join("invalid/malformed.json"), b"{not JSON}").unwrap();
    assert!(
        check_fixtures(&schema, root)
            .unwrap_err()
            .contains("malformed.json")
    );
    fs::remove_file(root.join("invalid/malformed.json")).unwrap();
    fs::copy(
        contract().join("fixtures/invalid/ghcr.json"),
        root.join("invalid/bad.json"),
    )
    .unwrap();
    check_fixtures(&schema, root).unwrap();
    let mut broken = schema.clone();
    broken["properties"]["images"]["type"] = Value::String("not-a-schema-type".into());
    assert!(
        check_fixtures(&broken, root)
            .unwrap_err()
            .contains("schema:")
    );
    for key in ["$ref", "$dynamicRef", "$recursiveRef"] {
        for reference in [
            "https://example.invalid/schema.json",
            "file:///forbidden.json",
            "#/$defs/missing",
        ] {
            let mut broken = schema.clone();
            broken["$defs"]["image"][key] = Value::String(reference.into());
            assert!(
                check_fixtures(&broken, root)
                    .unwrap_err()
                    .contains("non-local or unresolved")
            );
        }
    }
}
