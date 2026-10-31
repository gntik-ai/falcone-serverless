//! Published manifest shape only; no runtime parsing, authorization or secret resolution.

use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::PathBuf};

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts")
}

fn read_json(path: impl AsRef<std::path::Path>) -> Value {
    let path = path.as_ref();
    serde_json::from_str(
        &fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn schema() -> Value {
    let schema = read_json(contracts().join("revision-runtime-manifest.schema.json"));
    check_schema(&schema);
    schema
}

// Deliberately small ECMA-262 subset, documented in the contract. Compile before
// evaluating any instance so unsupported rules cannot hide in unvisited branches.
#[derive(Debug)]
struct Atom {
    bytes: Vec<u8>,
    min: usize,
    max: usize,
}

#[derive(Debug)]
struct Pattern {
    anchored_start: bool,
    anchored_end: bool,
    alternatives: Vec<Vec<Atom>>,
}

fn compile_pattern(pattern: &str) -> Pattern {
    let anchored_start = pattern.starts_with('^');
    let anchored_end = pattern.ends_with('$');
    let body = pattern.strip_prefix('^').unwrap_or(pattern);
    let body = body.strip_suffix('$').unwrap_or(body);
    let grouped = body.starts_with('(') && body.ends_with(')');
    let body = if grouped {
        &body[1..body.len() - 1]
    } else {
        body
    };
    assert!(grouped || !body.contains('|'), "ungrouped alternation");
    let alternatives = body.split('|').map(parse_sequence).collect();
    Pattern {
        anchored_start,
        anchored_end,
        alternatives,
    }
}

fn parse_sequence(mut text: &str) -> Vec<Atom> {
    assert!(!text.is_empty(), "empty pattern branch");
    let mut atoms = Vec::new();
    while !text.is_empty() {
        let (bytes, consumed) = if text.starts_with("[_.-]") {
            (b"_.-".to_vec(), 5)
        } else if text.starts_with("[0-9a-f]") {
            (b"0123456789abcdef".to_vec(), 8)
        } else if text.starts_with('[') {
            let bytes = text.as_bytes();
            assert!(bytes.len() >= 4 && bytes[3] == b']', "unsupported class");
            assert!(
                bytes[1].is_ascii_uppercase() && bytes[2] == bytes[1].to_ascii_lowercase(),
                "unsupported class"
            );
            (bytes[1..3].to_vec(), 4)
        } else {
            let byte = text.as_bytes()[0];
            assert!(
                byte.is_ascii_alphanumeric() || byte == b':',
                "unsupported regex construct: {text}"
            );
            (vec![byte], 1)
        };
        text = &text[consumed..];
        let (min, max) = if let Some(rest) = text.strip_prefix('*') {
            assert_eq!(bytes, b"_.-", "star only supports separators");
            text = rest;
            (0, usize::MAX)
        } else if let Some(rest) = text.strip_prefix("{64}") {
            assert_eq!(
                bytes, b"0123456789abcdef",
                "repeat only supports digest hex"
            );
            text = rest;
            (64, 64)
        } else {
            (1, 1)
        };
        atoms.push(Atom { bytes, min, max });
    }
    atoms
}

impl Pattern {
    fn matches(&self, text: &str) -> bool {
        let starts = if self.anchored_start {
            0..=0
        } else {
            0..=text.len()
        };
        starts.into_iter().any(|start| {
            self.alternatives.iter().any(|atoms| {
                let mut positions = BTreeSet::from([start]);
                for atom in atoms {
                    let mut next = BTreeSet::new();
                    for pos in positions {
                        let mut end = pos;
                        while end < text.len()
                            && end - pos < atom.max
                            && atom.bytes.contains(&text.as_bytes()[end])
                        {
                            end += 1;
                        }
                        if end - pos >= atom.min {
                            next.extend(pos + atom.min..=end);
                        }
                    }
                    positions = next;
                }
                positions
                    .iter()
                    .any(|end| !self.anchored_end || *end == text.len())
            })
        })
    }
}

fn check_schema(schema: &Value) {
    if schema.is_boolean() {
        return;
    }
    let object = schema
        .as_object()
        .expect("schema must be object or boolean");
    for (keyword, rule) in object {
        match keyword.as_str() {
            "$schema" => assert_eq!(rule, "https://json-schema.org/draft/2020-12/schema"),
            "$id" | "title" | "description" | "$ref" => {
                rule.as_str().expect("string rule");
            }
            "type" => {
                let types = rule
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| vec![rule.clone()]);
                assert!(!types.is_empty());
                for ty in types {
                    assert!(matches!(
                        ty.as_str(),
                        Some("object" | "array" | "string" | "integer" | "number" | "boolean")
                    ));
                }
            }
            "const" => {
                rule.as_str().expect("only string constants are used");
            }
            "enum" | "required" => {
                let entries = rule.as_array().expect("array rule");
                assert!(!entries.is_empty());
                let names: BTreeSet<_> = entries
                    .iter()
                    .map(|v| v.as_str().expect("string entry"))
                    .collect();
                assert_eq!(names.len(), entries.len());
            }
            "minimum" => {
                rule.as_f64().expect("numeric minimum");
            }
            "minLength" | "minItems" => {
                rule.as_u64().expect("nonnegative limit");
            }
            "pattern" => {
                compile_pattern(rule.as_str().expect("pattern string"));
            }
            "$defs" | "properties" => {
                for child in rule.as_object().expect("schema map").values() {
                    check_schema(child);
                }
            }
            "items" | "propertyNames" | "additionalProperties" | "not" | "if" | "then" => {
                check_schema(rule)
            }
            "allOf" | "oneOf" => {
                let children = rule.as_array().expect("schema array");
                assert!(!children.is_empty());
                for child in children {
                    check_schema(child);
                }
            }
            _ => panic!("unchecked JSON Schema keyword: {keyword}"),
        }
    }
    assert!(!object.contains_key("then") || object.contains_key("if"));
    if let Some(reference) = object.get("$ref") {
        assert!(
            reference.as_str().unwrap().starts_with("#/$defs/"),
            "local definitions only"
        );
    }
}

fn has_type(value: &Value, ty: &str) -> bool {
    match ty {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => value.is_boolean(),
        _ => panic!("unchecked type: {ty}"),
    }
}

fn accepts(root: &Value, schema: &Value, value: &Value) -> bool {
    if let Some(verdict) = schema.as_bool() {
        return verdict;
    }
    let object = schema.as_object().unwrap();
    for (keyword, rule) in object {
        let valid = match keyword.as_str() {
            "$schema" | "$id" | "title" | "description" | "$defs" => true,
            "$ref" => accepts(
                root,
                root.pointer(&rule.as_str().unwrap()[1..])
                    .expect("resolved local ref"),
                value,
            ),
            "type" => match rule.as_str() {
                Some(ty) => has_type(value, ty),
                None => rule
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|ty| has_type(value, ty.as_str().unwrap())),
            },
            "const" => value == rule,
            "enum" => rule.as_array().unwrap().contains(value),
            "not" => !accepts(root, rule, value),
            "allOf" => rule
                .as_array()
                .unwrap()
                .iter()
                .all(|s| accepts(root, s, value)),
            "oneOf" => {
                rule.as_array()
                    .unwrap()
                    .iter()
                    .filter(|s| accepts(root, s, value))
                    .count()
                    == 1
            }
            "if" => {
                !accepts(root, rule, value)
                    || object.get("then").is_none_or(|s| accepts(root, s, value))
            }
            "then" => true, // evaluated only with its sibling if
            "minimum" => value.as_f64().is_none_or(|n| n >= rule.as_f64().unwrap()),
            "minLength" => value
                .as_str()
                .is_none_or(|s| s.chars().count() as u64 >= rule.as_u64().unwrap()),
            "minItems" => value
                .as_array()
                .is_none_or(|a| a.len() as u64 >= rule.as_u64().unwrap()),
            "pattern" => value
                .as_str()
                .is_none_or(|s| compile_pattern(rule.as_str().unwrap()).matches(s)),
            "required" => value.as_object().is_none_or(|o| {
                rule.as_array()
                    .unwrap()
                    .iter()
                    .all(|key| o.contains_key(key.as_str().unwrap()))
            }),
            "properties" => value.as_object().is_none_or(|o| {
                rule.as_object()
                    .unwrap()
                    .iter()
                    .all(|(key, s)| o.get(key).is_none_or(|v| accepts(root, s, v)))
            }),
            "additionalProperties" => value.as_object().is_none_or(|o| {
                o.iter().all(|(key, v)| {
                    object.get("properties").and_then(|p| p.get(key)).is_some()
                        || accepts(root, rule, v)
                })
            }),
            "propertyNames" => value
                .as_object()
                .is_none_or(|o| o.keys().all(|key| accepts(root, rule, &json!(key)))),
            "items" => value
                .as_array()
                .is_none_or(|a| a.iter().all(|v| accepts(root, rule, v))),
            _ => panic!("unchecked JSON Schema keyword: {keyword}"),
        };
        if !valid {
            return false;
        }
    }
    true
}

fn fixture(kind: &str, name: &str) -> Value {
    read_json(
        contracts()
            .join("fixtures/revision-runtime-manifest")
            .join(kind)
            .join(format!("{name}.json")),
    )
}

#[test]
fn published_schema_accepts_and_rejects_every_fixture_and_detects_weakened_rules() {
    let original = schema();
    for (kind, expected) in [("valid", true), ("invalid", false)] {
        let mut paths: Vec<_> = fs::read_dir(
            contracts()
                .join("fixtures/revision-runtime-manifest")
                .join(kind),
        )
        .expect("fixture directory")
        .map(|e| e.unwrap().path())
        .collect();
        paths.sort();
        assert!(!paths.is_empty(), "empty {kind}/ fixture directory");
        for path in paths {
            assert_eq!(path.extension().and_then(|s| s.to_str()), Some("json"));
            let actual = accepts(&original, &original, &read_json(&path));
            println!(
                "{}: {}",
                path.file_name().unwrap().to_string_lossy(),
                if actual { "accepted" } else { "rejected" }
            );
            assert_eq!(actual, expected, "{}", path.display());
        }
    }
    // Each independently named fixture must expose its corresponding weakened rule.
    for (name, pointer, replacement) in [
        ("unknown-top", "/additionalProperties", Some(json!(true))),
        ("missing-artifactDigest", "/required", None),
        (
            "configuration-db-password",
            "/properties/effectiveConfiguration/propertyNames",
            None,
        ),
        (
            "interface-unknown-package",
            "/$defs/grant/allOf/1/then/properties/interface",
            Some(json!({"type":"string"})),
        ),
        ("cross-http-storage", "/$defs/grant/allOf", None),
    ] {
        let bad = fixture("invalid", name);
        assert!(
            !accepts(&original, &original, &bad),
            "baseline must reject {name}"
        );
        let mut mutated = original.clone();
        if let Some(replacement) = replacement {
            *mutated.pointer_mut(pointer).expect("mutation anchor") = replacement;
        } else if pointer == "/required" {
            let required = mutated["required"].as_array_mut().unwrap();
            let before = required.len();
            required.retain(|v| v != "artifactDigest");
            assert_eq!(required.len() + 1, before);
        } else if pointer == "/$defs/grant/allOf" {
            // Remove only scope coupling; keep the family/interface pins intact.
            for branch in mutated
                .pointer_mut(pointer)
                .unwrap()
                .as_array_mut()
                .unwrap()
            {
                assert!(
                    branch["then"]["properties"]
                        .as_object_mut()
                        .unwrap()
                        .remove("scope")
                        .is_some()
                );
            }
        } else {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            assert!(
                mutated
                    .pointer_mut(parent)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(key)
                    .is_some()
            );
        }
        check_schema(&mutated);
        assert!(
            accepts(&mutated, &mutated, &bad),
            "mutation did not admit {name}"
        );
    }
}

const REQUIRED: &[&str] = &[
    "manifestVersion",
    "artifactDigest",
    "languageProfile",
    "engineVersion",
    "wasiVersion",
    "interfaceVersion",
    "imports",
    "budgets",
    "grants",
    "effectiveConfiguration",
    "provenance",
    "sbom",
];
const FAMILIES: &[(&str, &str, &str)] = &[
    ("storage", "storage", "prefixes"),
    ("http", "http", "hosts"),
    ("event", "events", "topics"),
    ("workflow", "workflows", "targets"),
];
const EXACT_DENYLIST: &[&str] = &[
    "value",
    "plaintext",
    "secret",
    "secretvalue",
    "password",
    "passwd",
    "token",
    "apikey",
    "privatekey",
    "credential",
    "credentials",
    "clientsecret",
    "accesskey",
];
const CONFIGURATION_DENYLIST: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "apikey",
    "privatekey",
    "credential",
    "accesstoken",
    "authtoken",
    "bearertoken",
];

#[test]
fn required_fields_and_family_scope_cases_have_single_defect_witnesses() {
    let schema = schema();
    let baseline = fixture("valid", "storage");
    let required: BTreeSet<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required, REQUIRED.iter().copied().collect());
    for key in REQUIRED {
        let mut expected = baseline.clone();
        expected
            .as_object_mut()
            .unwrap()
            .remove(*key)
            .expect("required baseline field");
        let actual = fixture("invalid", &format!("missing-{key}"));
        assert_eq!(actual, expected, "omission must be the only defect: {key}");
        assert!(!accepts(&schema, &schema, &actual));
    }
    for (family, _, scope_key) in FAMILIES {
        let baseline = fixture("valid", family);
        assert!(accepts(&schema, &schema, &baseline));
        for defect in ["missing-scope", "empty-list", "bare-star"] {
            let mut expected = baseline.clone();
            let grant = &mut expected["grants"][0];
            match defect {
                "missing-scope" => {
                    grant.as_object_mut().unwrap().remove("scope").unwrap();
                }
                "empty-list" => grant["scope"][scope_key] = json!([]),
                "bare-star" => grant["scope"][scope_key] = json!(["*"]),
                _ => unreachable!(),
            }
            let actual = fixture("invalid", &format!("{family}-{defect}"));
            assert_eq!(actual, expected, "one scope defect: {family}-{defect}");
            assert!(!accepts(&schema, &schema, &actual));
        }
    }
    for (location, pointer) in [
        ("top", ""),
        ("grant", "/grants/0"),
        ("secret-ref", "/grants/0/secretRef"),
    ] {
        for key in ["Password", "API_KEY", "client.secret", "value"] {
            let mut expected = baseline.clone();
            expected
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.into(), json!("fixture"));
            let actual = fixture("invalid", &format!("sensitive-{location}-{key}"));
            assert_eq!(actual, expected, "one sensitive property defect");
            assert!(!accepts(&schema, &schema, &actual));
        }
    }
    for (name, key, value) in [
        ("db-password", "db-password", json!("fixture")),
        ("Bearer_Token", "Bearer_Token", json!("fixture")),
        ("awsSecretKey", "awsSecretKey", json!("fixture")),
        ("object", "nested", json!({})),
        ("array", "nested", json!([])),
    ] {
        let mut expected = baseline.clone();
        expected["effectiveConfiguration"]
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value);
        assert_eq!(
            fixture("invalid", &format!("configuration-{name}")),
            expected
        );
    }
}

#[test]
fn denylists_match_the_independent_name_policy() {
    let schema = schema();
    let safe = &schema["$defs"]["safeName"];
    let configuration = &schema["properties"]["effectiveConfiguration"]["propertyNames"];
    for word in EXACT_DENYLIST {
        for name in [
            word.to_string(),
            word.to_ascii_uppercase(),
            word.chars()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join("_.-"),
            format!("._-{word}-_."),
        ] {
            assert!(
                !accepts(&schema, safe, &json!(name)),
                "global denylist missed {name}"
            );
            assert!(
                !accepts(&schema, configuration, &json!(name)),
                "configuration missed {name}"
            );
        }
    }
    for word in CONFIGURATION_DENYLIST {
        let separated = word
            .chars()
            .map(|c| c.to_ascii_uppercase().to_string())
            .collect::<Vec<_>>()
            .join(".-_");
        for name in [format!("db{word}Key"), format!("db{separated}Key")] {
            assert!(
                !accepts(&schema, configuration, &json!(name)),
                "substring denylist missed {name}"
            );
        }
    }
    for name in [
        "timeoutMs",
        "maxTokens",
        "logLevel",
        "secretRef",
        "guestGrantId",
        "tokenizer",
        "monkey",
    ] {
        assert!(
            accepts(&schema, safe, &json!(name)),
            "exact denylist overmatched {name}"
        );
    }
    for name in [
        "timeoutMs",
        "maxTokens",
        "logLevel",
        "enabled",
        "tokenizer",
        "monkey",
    ] {
        assert!(
            accepts(&schema, configuration, &json!(name)),
            "substring denylist overmatched {name}"
        );
    }
}

fn visit_schema(schema: &Value, path: &str, inspect: &mut impl FnMut(&Value, &str)) {
    inspect(schema, path);
    let Some(object) = schema.as_object() else {
        return;
    };
    for (key, value) in object {
        match key.as_str() {
            "$defs" | "properties" => {
                for (name, child) in value.as_object().unwrap() {
                    visit_schema(child, &format!("{path}/{key}/{name}"), inspect);
                }
            }
            "allOf" | "oneOf" => {
                for (index, child) in value.as_array().unwrap().iter().enumerate() {
                    visit_schema(child, &format!("{path}/{key}/{index}"), inspect);
                }
            }
            "items" | "additionalProperties" | "propertyNames" | "not" | "if" | "then" => {
                visit_schema(value, &format!("{path}/{key}"), inspect);
            }
            _ => {}
        }
    }
}

#[test]
fn documentation_wit_and_schema_are_one_versioned_contract() {
    let schema = schema();
    let document = fs::read_to_string(contracts().join("revision-runtime-manifest.md"))
        .expect("canonical contract document");
    let versions: Vec<_> = document
        .lines()
        .filter_map(|line| line.strip_prefix("manifestVersion: "))
        .collect();
    assert_eq!(
        versions,
        [schema["properties"]["manifestVersion"]["const"]
            .as_str()
            .unwrap()]
    );
    for field in REQUIRED {
        assert!(
            document.contains(&format!("`{field}`")),
            "undocumented required field: {field}"
        );
    }
    for word in EXACT_DENYLIST.iter().chain(CONFIGURATION_DENYLIST) {
        assert!(
            document.contains(&format!("`{word}`")),
            "undocumented denylist word: {word}"
        );
    }
    for rule in [
        "FR-DEP-03",
        "FR-STO-04",
        "FR-WAS-03",
        "shape only",
        "verified scopes",
        "WAS-02.t4",
        "new manifestVersion",
    ] {
        assert!(document.contains(rule), "missing contract boundary: {rule}");
    }
    let mut resolve = wit_parser::Resolve::default();
    let (package, _) = resolve
        .push_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wit/host.wit"))
        .expect("parse authoritative WIT");
    let package = &resolve.packages[package];
    let version = package
        .name
        .version
        .as_ref()
        .expect("versioned package")
        .to_string();
    assert_eq!(schema["properties"]["interfaceVersion"]["const"], version);
    let grant = &schema["$defs"]["grant"];
    assert_eq!(
        grant["properties"]["family"]["enum"],
        json!(FAMILIES.iter().map(|(f, _, _)| *f).collect::<Vec<_>>())
    );
    assert_eq!(grant["allOf"].as_array().unwrap().len(), FAMILIES.len());
    for ((family, interface, _), branch) in FAMILIES.iter().zip(grant["allOf"].as_array().unwrap())
    {
        let id = package
            .interfaces
            .get(*interface)
            .expect("family interface exists in WIT");
        assert_eq!(resolve.interfaces[*id].name.as_deref(), Some(*interface));
        let expected = format!(
            "{}:{}/{interface}@{version}",
            package.name.namespace, package.name.name
        );
        assert_eq!(branch["if"]["properties"]["family"]["const"], *family);
        assert_eq!(
            branch["then"]["properties"]["interface"]["enum"],
            json!([expected])
        );
        assert_eq!(
            branch["then"]["properties"]["scope"]["$ref"],
            format!("#/$defs/{family}Scope")
        );
    }
    let mut open_objects = Vec::new();
    visit_schema(&schema, "", &mut |node, path| {
        if node["type"] == "object" {
            assert!(
                node.get("propertyNames").is_some(),
                "missing propertyNames at {path}"
            );
            if node.get("additionalProperties") != Some(&json!(false)) {
                open_objects.push(path.to_string());
            }
        }
        if let Some(properties) = node.get("properties") {
            for name in properties.as_object().unwrap().keys() {
                let normalized: String = name
                    .chars()
                    .filter(|c| !matches!(c, '_' | '.' | '-'))
                    .map(|c| c.to_ascii_lowercase())
                    .collect();
                assert!(
                    !EXACT_DENYLIST.contains(&normalized.as_str()),
                    "declared sensitive name: {path}/{name}"
                );
            }
        }
    });
    assert_eq!(open_objects, ["/properties/effectiveConfiguration"]);
}

#[test]
fn regex_subset_has_positive_negative_and_unsupported_cases() {
    for (pattern, yes, no) in [
        (
            "^([Aa][_.-]*[Bb]|[Cc])$",
            vec!["ab", "A__.-b", "C"],
            vec!["xab", "abx", "a b", "", "ac"],
        ),
        (
            "([Ss][_.-]*[Ee][_.-]*[Cc])",
            vec!["prefixSECret", "s.-_e_c", "xsecx"],
            vec!["s e c", "safe", "", "sxec"],
        ),
        ("^[Aa]$", vec!["a", "A"], vec!["aa", "a\n", "\na", "é"]),
    ] {
        let compiled = compile_pattern(pattern);
        for text in yes {
            assert!(compiled.matches(text), "{pattern} missed {text:?}");
        }
        for text in no {
            assert!(!compiled.matches(text), "{pattern} overmatched {text:?}");
        }
    }
    let digest = compile_pattern("^sha256:[0-9a-f]{64}$");
    assert!(
        digest.matches("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
    );
    for text in [
        "sha256:abc",
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg",
    ] {
        assert!(!digest.matches(text));
    }
    for pattern in [
        "",
        "^$",
        ".*",
        "(?i)secret",
        "[a-z]",
        "[aA]",
        "[Aa]+",
        "[Aa]?",
        "[Aa]*",
        "[Aa]{2}",
        "[0-9a-f]{63}",
        "a|b",
        "(a(b))",
        "a\\b",
        "[Aa]$x",
        "[Aa]|",
    ] {
        assert!(
            std::panic::catch_unwind(|| compile_pattern(pattern)).is_err(),
            "unsupported regex accepted: {pattern}"
        );
    }
}

#[test]
fn evaluator_enforces_keyword_semantics_and_fails_closed() {
    for (schema, accepted, rejected) in [
        (
            json!({"type":"integer", "minimum":1}),
            json!([1, 1.0, 2]),
            json!([0, -1, 1.5, "1", null]),
        ),
        (
            json!({"type":"string", "minLength":2}),
            json!(["ab", "é好"]),
            json!(["é", "", 2]),
        ),
        (
            json!({"oneOf":[{"type":"number"},{"type":"integer"}]}),
            json!([1.5]),
            json!([1, "1"]),
        ),
        (
            json!({"type":"object","properties":{"x":{"const":"yes"}},"required":["x"],"additionalProperties":false}),
            json!([{"x":"yes"}]),
            json!([{}, {"x":"no"}, {"x":"yes","y":1}]),
        ),
        (
            json!({"type":"array","minItems":1,"items":{"enum":["a","b"]}}),
            json!([["a"], ["b", "a"]]),
            json!([[], ["c"], ["a", 2]]),
        ),
        (
            json!({"if":{"const":"a"},"then":false}),
            json!(["b", 1]),
            json!(["a"]),
        ),
        (
            json!({"type":"object","additionalProperties":{"type":["string","number","integer","boolean"]},"propertyNames":{"not":{"const":"bad"}}}),
            json!([{"ok":true},{"ok":1.5},{"ok":"s"}]),
            json!([{"bad":1},{"ok":{}},{"ok":[]},{"ok":null}]),
        ),
    ] {
        check_schema(&schema);
        for value in accepted.as_array().unwrap() {
            assert!(
                accepts(&schema, &schema, value),
                "must accept {value} under {schema}"
            );
        }
        for value in rejected.as_array().unwrap() {
            assert!(
                !accepts(&schema, &schema, value),
                "must reject {value} under {schema}"
            );
        }
    }
    for unsupported in [
        json!({"format":"uri"}),
        json!({"$defs":{"unused":{"patternProperties":{}}}}),
        json!({"if":false,"then":{"unevaluatedProperties":false}}),
        json!({"type":"null"}),
        json!({"pattern":".*"}),
    ] {
        assert!(
            std::panic::catch_unwind(|| check_schema(&unsupported)).is_err(),
            "unchecked schema: {unsupported}"
        );
    }
}
