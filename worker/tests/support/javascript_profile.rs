//! Shared source-only qualification contract, also used before cluster client creation.
use falcone_serverless_worker::adapters::compat::{Source, check};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

pub fn manifest() -> Value {
    serde_json::from_slice(&fs::read(profile_dir().join("profile.json")).unwrap()).unwrap()
}

pub fn pins_valid(manifest: &Value) -> bool {
    let pins: toml::Value =
        toml::from_str(&fs::read_to_string(profile_dir().join("versions.toml")).unwrap()).unwrap();
    let expected = serde_json::to_value(pins).unwrap();
    manifest["pins"] == expected && manifest["id"] == "javascript-wasi-v1"
}

pub fn imports_valid(manifest: &Value, declared: &[String]) -> bool {
    let mut resolve = wit_parser::Resolve::default();
    let (package, _) = resolve
        .push_path(profile_dir().join("../../wit/host.wit"))
        .unwrap();
    let package = &resolve.packages[package];
    let imports: std::collections::BTreeSet<_> = resolve.worlds[package.worlds["function"]]
        .imports
        .values()
        .filter_map(|item| match item {
            wit_parser::WorldItem::Interface { id, .. } => resolve.interfaces[*id]
                .name
                .as_deref()
                .map(|name| package.name.interface_id(name)),
            _ => None,
        })
        .collect();
    let Some(allowed) = manifest["allowedInterfaces"].as_array() else {
        return false;
    };
    if allowed.len() != 8 {
        return false;
    }
    let allowed: Option<std::collections::BTreeSet<_>> = allowed
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect();
    allowed.is_some_and(|allowed| {
        allowed == imports
            && allowed.len() == 8
            && declared.iter().all(|name| allowed.contains(name))
    })
}

pub fn profile_dir() -> PathBuf {
    std::env::var_os("FS_JS_PROFILE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("qualification/javascript")
        })
}

pub const STATIC_CHECKS: &[&str] = &[
    "pins",
    "interfaces",
    "sourceHashes",
    "compatibility",
    "sourceClosure",
    "componentImports",
    "negativeFixtures",
    "envelopeInventory",
];
pub const ENVELOPES: &[&str] = &[
    "401", "400", "429", "404", "403", "304", "200-json", "200-yaml",
];

pub fn verdict(checks: &BTreeMap<String, bool>, envelopes: &[bool]) -> &'static str {
    if STATIC_CHECKS
        .iter()
        .all(|key| checks.get(*key) == Some(&true))
        && checks.values().all(|passed| *passed)
        && envelopes.len() == ENVELOPES.len()
        && envelopes.iter().all(|passed| *passed)
    {
        "qualified"
    } else {
        "blocked"
    }
}

pub fn diagnostics(sources: &[Source<'_>], package: &str) -> Value {
    json!(
        check(sources, package)
            .diagnostics()
            .iter()
            .map(|d| json!({
                "code":d.code, "construct":d.construct, "source":d.source,
                "line":d.line, "column":d.column,
            }))
            .collect::<Vec<_>>()
    )
}

pub fn reference_diagnostics(manifest: &Value) -> Value {
    let paths = manifest["reference"]["sources"]
        .as_array()
        .expect("reference source receipts");
    let texts: Vec<_> = paths
        .iter()
        .map(|p| {
            fs::read_to_string(
                profile_dir()
                    .join("reference")
                    .join(p["path"].as_str().unwrap()),
            )
            .unwrap()
        })
        .collect();
    let sources: Vec<_> = texts.iter().map(|text| Source { text }).collect();
    let files: BTreeMap<_, _> = paths
        .iter()
        .zip(&texts)
        .map(|(p, text)| {
            (
                p["path"].as_str().unwrap(),
                diagnostics(&[Source { text }], "{}"),
            )
        })
        .collect();
    json!({"bundle":diagnostics(&sources, "{}"), "files":files})
}

pub fn hashes_valid(manifest: &Value) -> bool {
    manifest["reference"]["sources"]
        .as_array()
        .is_some_and(|sources| {
            sources.len() == 4
                && sources.iter().all(|source| {
                    source["path"]
                        .as_str()
                        .and_then(|path| fs::read(profile_dir().join("reference").join(path)).ok())
                        .is_some_and(|bytes| {
                            source["sha256"] == format!("{:x}", Sha256::digest(bytes))
                        })
                })
        })
}

pub fn negative_fixtures_valid(manifest: &Value) -> bool {
    let Some(paths) = manifest["negativeFixtures"].as_array() else {
        return false;
    };
    let mut codes = BTreeSet::new();
    paths.len() == 6
        && paths.iter().all(|p| {
            let Some(fixture) = p
                .as_str()
                .and_then(|path| fs::read(profile_dir().join(path)).ok())
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            else {
                return false;
            };
            let (Some(text), Some(package), Some(code)) = (
                fixture["source"].as_str(),
                fixture["packageManifest"].as_str(),
                fixture["expected"]["code"].as_str(),
            ) else {
                return false;
            };
            codes.insert(code.to_owned())
                && diagnostics(&[Source { text }], package) == json!([fixture["expected"]])
        })
        && manifest["diagnostics"].as_array().is_some_and(|expected| {
            expected.len() == 6
                && expected
                    .iter()
                    .all(|c| c.as_str().is_some_and(|c| codes.contains(c)))
        })
}

pub fn envelope_inventory_valid(manifest: &Value) -> bool {
    manifest["envelopes"].as_array().is_some_and(|cases| {
        cases.len() == 8
            && cases.iter().zip(ENVELOPES).all(|(case, name)| {
                let expected =
                    format!("../../../docs/contracts/fixtures/legacy-envelope/valid/{name}.json");
                case["id"] == *name
                    && case["fixture"] == expected
                    && profile_dir().join(expected).is_file()
                    && case["expectedCalls"].is_array()
            })
    })
}

pub fn static_checks(manifest: &Value) -> BTreeMap<String, bool> {
    let diagnostics = reference_diagnostics(manifest);
    let compatibility = diagnostics["bundle"].as_array().is_some_and(Vec::is_empty)
        && diagnostics["files"].as_object().is_some_and(|files| {
            files
                .values()
                .all(|v| v.as_array().is_some_and(Vec::is_empty))
        });
    // Resolve local static imports from the public sources. No module is executed.
    let closure = manifest["reference"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|source| {
            let path = profile_dir()
                .join("reference")
                .join(source["path"].as_str().unwrap());
            fs::read_to_string(&path).unwrap().lines().all(|line| {
                line.split_once(" from '")
                    .and_then(|(_, rest)| rest.split_once('\''))
                    .is_none_or(|(module, _)| {
                        !module.starts_with('.') || path.parent().unwrap().join(module).is_file()
                    })
            })
        });
    // A receipt alone cannot prove imports. Inspect the raw component through the existing
    // validator, without compilation, native-cache loading or guest instantiation.
    let component = &manifest["component"];
    let component_imports = component["path"]
        .as_str()
        .and_then(|path| fs::read(profile_dir().join(path)).ok())
        .is_some_and(|bytes| {
            component["artifactId"]
                .as_str()
                .is_some_and(|id| !id.trim().is_empty())
                && component["sha256"] == format!("{:x}", Sha256::digest(&bytes))
                && falcone_serverless_worker::artifact::validate::validate(&bytes, 64 * 1024 * 1024)
                    .is_ok_and(|validated| imports_valid(manifest, &validated.imports))
        });
    [
        ("pins", pins_valid(manifest)),
        ("interfaces", imports_valid(manifest, &[])),
        ("sourceHashes", hashes_valid(manifest)),
        ("compatibility", compatibility),
        ("sourceClosure", closure),
        ("componentImports", component_imports),
        ("negativeFixtures", negative_fixtures_valid(manifest)),
        ("envelopeInventory", envelope_inventory_valid(manifest)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}

pub fn envelope_matches(case: &Value, envelope: &Value, calls: &Value) -> bool {
    let fixture: Value = serde_json::from_slice(
        &fs::read(profile_dir().join(case["fixture"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    *envelope == fixture["expected"] && *calls == case["expectedCalls"]
}
