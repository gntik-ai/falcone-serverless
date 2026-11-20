//! JavaScript Wasm profile qualification, static checks only: no guest is built, instantiated
//! or executed here. Cluster execution lives in `cluster_profile_javascript.rs`. Anything this
//! snapshot cannot verify is reported UNVERIFIED and never counts towards qualification.
#[path = "support/profile_javascript.rs"]
mod profile;

use falcone_serverless_worker::{
    adapters::compat::{TreeFile, check_tree},
    api,
    host::{Bindings, Context, HostError, Operation, Reply},
};
use profile::{Call, Case, Check, Replay, Status, compare, load_cases, request_for, status};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const CODES: &[&str] = &[
    "WASM-NATIVE-LIBRARY",
    "WASM-GPU",
    "WASM-PROCESS-SPAWN",
    "WASM-INSTALL-HOOK",
    "WASM-PACKAGE-NOT-IN-CATALOGUE",
    "WASM-UNSUPPORTED-API",
];

fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn root() -> PathBuf {
    worker().join("qualification/profiles/javascript")
}
fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
fn manifest() -> toml::Value {
    toml::from_str(&read(&root().join("manifest.toml"))).unwrap()
}
fn matrix() -> Value {
    serde_json::from_str(&read(
        &worker().join("../docs/contracts/language-profile-matrix.json"),
    ))
    .unwrap()
}
fn strings(value: &toml::Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn exact(version: &str) -> bool {
    let core = version.strip_prefix('v').unwrap_or(version);
    let parts: Vec<_> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}
fn digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// ---------------------------------------------------------------- manifest and matrix

#[test]
fn manifest_pins_are_exact_and_equal_the_recorded_toolchain() {
    for floating in [
        "^1.41.0", "~1.41.0", ">=1.41.0", "1.x", "1.41", "*", "latest", "",
    ] {
        assert!(!exact(floating), "{floating:?} must not count as exact");
    }
    let manifest = manifest();
    let toolchain = &manifest["toolchain"];
    for field in ["wasmtime", "jco", "componentize_js", "node"] {
        let version = toolchain[field].as_str().unwrap_or_default();
        assert!(
            exact(version),
            "{field}: {version:?} is not an exact version"
        );
    }
    // `none` is the WASI version: the final component imports no WASI at all.
    assert_eq!(toolchain["wasi"].as_str(), Some("none"));
    assert!(digest(toolchain["wasi_virt_rev"].as_str().unwrap(), 40));
    for field in ["jco_sha256", "componentize_js_sha256", "node_sha256"] {
        assert!(digest(toolchain[field].as_str().unwrap(), 64), "{field}");
    }
    let versions: toml::Value = toml::from_str(&read(
        &worker().join("qualification/javascript/versions.toml"),
    ))
    .unwrap();
    for (field, table, key) in [
        ("jco", "jco", "version"),
        ("jco_sha256", "jco", "sha256"),
        ("componentize_js", "componentize_js", "version"),
        ("componentize_js_sha256", "componentize_js", "sha256"),
        ("wasi_virt_rev", "wasi_virt", "rev"),
        ("node", "node", "version"),
        ("node_sha256", "node", "sha256"),
    ] {
        assert_eq!(
            toolchain[field], versions[table][key],
            "{field} must be copied exactly from versions.toml"
        );
    }
    let workspace: toml::Value = toml::from_str(&read(&worker().join("../Cargo.toml"))).unwrap();
    assert_eq!(
        format!("={}", toolchain["wasmtime"].as_str().unwrap()),
        workspace["workspace"]["dependencies"]["wasmtime"]["version"]
            .as_str()
            .unwrap()
    );
    // No third-party package is accepted, so there is no catalogue package to pin.
    assert!(
        manifest["catalogue"]["packages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        manifest["profile"]["engine"].as_str(),
        Some("StarlingMonkey")
    );
}

fn wit_interfaces() -> BTreeSet<String> {
    let mut resolve = wit_parser::Resolve::default();
    let (package, _) = resolve.push_path(worker().join("wit/host.wit")).unwrap();
    let package = &resolve.packages[package];
    resolve.worlds[package.worlds["function"]]
        .imports
        .values()
        .filter_map(|item| match item {
            wit_parser::WorldItem::Interface { id, .. } => resolve.interfaces[*id]
                .name
                .as_deref()
                .map(|name| package.name.interface_id(name)),
            _ => None,
        })
        .collect()
}

/// An allowed-interface list is explicit: no wildcard, no WASI, nothing outside the WIT.
fn interfaces_ok(list: &[String], wit: &BTreeSet<String>) -> Result<(), String> {
    for entry in list {
        if entry.contains('*') || entry.starts_with("wasi:") || !wit.contains(entry) {
            return Err(format!("not an explicit falcone:host interface: {entry}"));
        }
    }
    Ok(())
}

#[test]
fn manifest_and_both_matrix_copies_agree_with_the_wit() {
    let matrix = matrix();
    let manifest = manifest();
    assert_eq!(matrix["matrixVersion"], "2");
    assert_eq!(manifest["profile"]["matrix_revision"].as_str(), Some("2"));
    assert_eq!(
        fs::read(worker().join("src/adapters/language-profile-matrix.json")).unwrap(),
        fs::read(worker().join("../docs/contracts/language-profile-matrix.json")).unwrap(),
        "matrix copies are byte-identical"
    );
    let javascript = &matrix["profiles"]["javascript"];
    assert_eq!(javascript["status"], "disabled");
    assert_eq!(javascript["evidence"], json!({}));
    assert_eq!(manifest["profile"]["status"].as_str(), Some("disabled"));
    assert_eq!(
        javascript["id"],
        manifest["profile"]["id"].as_str().unwrap()
    );
    assert_eq!(javascript["wasiVersion"], "none");
    assert_eq!(
        javascript["toolchainVersion"],
        manifest["toolchain"]["jco"].as_str().unwrap()
    );
    assert_eq!(
        javascript["wasmtimeVersion"],
        manifest["toolchain"]["wasmtime"].as_str().unwrap()
    );
    let allowed: Vec<String> = javascript["allowedInterfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap().to_owned())
        .collect();
    let wit = wit_interfaces();
    interfaces_ok(&allowed, &wit).unwrap();
    assert_eq!(allowed, strings(&manifest["wit"]["interfaces"]));
    assert_eq!(
        allowed.iter().cloned().collect::<BTreeSet<_>>(),
        wit,
        "every function-world import of host.wit, and nothing else"
    );
    assert!(allowed.iter().all(|entry| entry.ends_with("@0.2.0")));
    assert!(!allowed.contains(&"falcone:host/handler@0.2.0".to_owned()));
    // A wildcard, a WASI entry and an unknown entry are each refused.
    for bad in [
        "falcone:host/*@0.2.0",
        "wasi:cli/environment@0.2.6",
        "falcone:host/unknown@0.2.0",
    ] {
        let mut mutated = allowed.clone();
        mutated.push(bad.to_owned());
        assert!(interfaces_ok(&mutated, &wit).is_err(), "{bad}");
    }
    let examples: Vec<String> = manifest["workloads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        javascript["testedExamples"],
        json!(examples),
        "tested examples are the manifest's workloads"
    );
    let codes: BTreeSet<String> = manifest["diagnostics"]
        .as_table()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(codes, CODES.iter().map(|c| (*c).to_owned()).collect());
    assert_eq!(
        javascript["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>(),
        codes
    );
}

// ---------------------------------------------------------------- workload sources

/// Import specifiers of a handler module, in order. Only static `import ... from '...'`.
fn import_specifiers(source: &str) -> Vec<String> {
    source
        .lines()
        .filter(|line| line.trim_start().starts_with("import "))
        .filter_map(|line| {
            let (_, specifier) = line.rsplit_once(" from ")?;
            Some(
                specifier
                    .trim()
                    .trim_end_matches(';')
                    .trim_matches('\'')
                    .to_owned(),
            )
        })
        .collect()
}
fn undeclared(source: &str, declared: &[String]) -> Vec<String> {
    import_specifiers(source)
        .into_iter()
        .filter(|specifier| !declared.contains(specifier))
        .collect()
}

#[test]
fn workload_imports_are_declared_allowed_and_host_only() {
    let manifest = manifest();
    let allowed = strings(&manifest["wit"]["interfaces"]);
    for workload in manifest["workloads"].as_array().unwrap() {
        let name = workload["name"].as_str().unwrap();
        let source = read(&root().join(workload["handler"].as_str().unwrap()));
        let declared = strings(&workload["imports"]);
        assert!(undeclared(&source, &declared).is_empty(), "{name}");
        assert_eq!(import_specifiers(&source).len(), declared.len(), "{name}");
        for import in &declared {
            assert!(allowed.contains(import), "{name}: {import} is not allowed");
        }
        for forbidden in [
            "node:",
            "wasi:",
            "require(",
            "import(",
            "process.",
            "eval(",
            "new Function",
        ] {
            assert!(!source.contains(forbidden), "{name}: {forbidden}");
        }
        assert!(
            source.contains("export const handler"),
            "{name}: real handler export"
        );
        // Adding an undeclared import to a test copy makes the check fail.
        let copy = format!("import {{ publish }} from 'falcone:host/events@0.2.0';\n{source}");
        if !declared.iter().any(|d| d == "falcone:host/events@0.2.0") {
            assert_eq!(
                undeclared(&copy, &declared),
                ["falcone:host/events@0.2.0"],
                "{name}"
            );
        }
        let copy = format!("import fs from 'node:fs';\n{source}");
        assert_eq!(undeclared(&copy, &declared), ["node:fs"], "{name}");
    }
}

#[test]
fn recorded_outputs_of_the_pure_workloads_match_independent_computation() {
    let manifest = manifest();
    let find = |name: &str| {
        manifest["workloads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["name"].as_str() == Some(name))
            .unwrap()
            .clone()
    };
    let load = |workload: &toml::Value, field: &str| -> Value {
        serde_json::from_str(&read(&root().join(workload[field].as_str().unwrap()))).unwrap()
    };
    // sha256("abc") is the FIPS 180-2 test vector, computed here by the sha2 crate.
    let hash = find("hash-sha256");
    let input = load(&hash, "input");
    assert_eq!(
        load(&hash, "expected")["sha256"],
        hex(input["message"].as_str().unwrap().as_bytes())
    );
    assert_eq!(
        load(&hash, "expected")["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let transform = find("json-transform");
    let items = load(&transform, "input")["items"]
        .as_array()
        .unwrap()
        .clone();
    let total: i64 = items
        .iter()
        .map(|i| i["price"].as_i64().unwrap() * i["quantity"].as_i64().unwrap())
        .sum();
    let mut names: Vec<String> = items
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_uppercase())
        .collect();
    names.sort();
    assert_eq!(
        load(&transform, "expected"),
        json!({"count": items.len(), "total": total, "names": names})
    );
    let regex = find("text-regex");
    assert_eq!(
        load(&regex, "expected"),
        json!({"orders":["12","345"],"date":{"year":"2026","month":"11","day":"20"}})
    );
}

// ---------------------------------------------------------------- legacy reference function

fn legacy_dir() -> PathBuf {
    root().join("legacy")
}

struct Reference {
    entry: String,
    files: Vec<(String, String)>,
}
fn reference() -> Reference {
    let table: toml::Value = toml::from_str(&read(&legacy_dir().join("sources.toml"))).unwrap();
    Reference {
        entry: table["entry"].as_str().unwrap().to_owned(),
        files: table["file"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["path"].as_str().unwrap().to_owned(),
                    f["sha256"].as_str().unwrap().to_owned(),
                )
            })
            .collect(),
    }
}

/// A present file must hash to its pin; an absent one is UNVERIFIED and never passes.
fn verify(bytes: Option<&[u8]>, pinned: &str) -> Check {
    match bytes {
        None => Check::Unverified("file absent".into()),
        Some(bytes) if hex(bytes) == pinned => Check::Passed,
        Some(_) => Check::Failed("sha256 mismatch".into()),
    }
}

fn reference_checks() -> Vec<(String, Check)> {
    reference()
        .files
        .iter()
        .map(|(path, pinned)| {
            let bytes = fs::read(legacy_dir().join(path)).ok();
            (path.clone(), verify(bytes.as_deref(), pinned))
        })
        .collect()
}

#[test]
fn reference_files_match_their_pinned_hashes_or_report_unverified() {
    let table = reference();
    assert_eq!(table.files.len(), 6);
    let checks = reference_checks();
    for (path, check) in &checks {
        match check {
            Check::Passed => {}
            Check::Unverified(reason) => println!("UNVERIFIED: {path}: {reason}"),
            Check::Failed(reason) => panic!("{path}: {reason}"),
        }
    }
    // The four files this snapshot carries are verified byte for byte.
    for path in [
        "actions/openapi-spec-serve.mjs",
        "src/config.mjs",
        "src/spec-version-repo.mjs",
        "src/spec-cache.mjs",
    ] {
        assert!(
            checks.iter().any(|(p, c)| p == path && *c == Check::Passed),
            "{path}"
        );
    }
    let all: Vec<Check> = checks.into_iter().map(|(_, check)| check).collect();
    if all.iter().any(|check| *check != Check::Passed) {
        assert_ne!(
            status(&all),
            Status::Qualified,
            "an absent file never qualifies"
        );
    }
    // A single altered byte, and an absent file, are not a pass.
    let real = fs::read(legacy_dir().join("src/spec-cache.mjs")).unwrap();
    let pinned = &table
        .files
        .iter()
        .find(|(p, _)| p == "src/spec-cache.mjs")
        .unwrap()
        .1;
    assert_eq!(verify(Some(&real), pinned), Check::Passed);
    let mut altered = real.clone();
    altered.push(b'\n');
    assert!(matches!(verify(Some(&altered), pinned), Check::Failed(_)));
    assert!(matches!(verify(None, pinned), Check::Unverified(_)));
    assert_eq!(status(&[verify(Some(&altered), pinned)]), Status::Blocked);
    assert_eq!(status(&[verify(None, pinned)]), Status::NotQualified);
}

fn present_files() -> Vec<(String, String)> {
    reference()
        .files
        .iter()
        .filter_map(|(path, _)| {
            fs::read_to_string(legacy_dir().join(path))
                .ok()
                .map(|text| (path.clone(), text))
        })
        .collect()
}

fn tree_report(files: &[(String, String)], entry: &str) -> Vec<Value> {
    let tree: Vec<_> = files
        .iter()
        .map(|(path, text)| TreeFile { path, text })
        .collect();
    check_tree(&tree, entry, None)
        .diagnostics()
        .iter()
        .map(|d| json!({"code": d.code, "construct": d.construct, "file": d.path, "line": d.line, "column": d.column}))
        .collect()
}

fn has(report: &[Value], code: &str, file: &str, line: u64) -> bool {
    report
        .iter()
        .any(|d| d["code"] == code && d["file"] == file && d["line"] == line)
}

#[test]
fn the_unmodified_reference_tree_is_blocked_with_the_recorded_diagnostics() {
    let entry = reference().entry;
    let files = present_files();
    let report = tree_report(&files, &entry);
    assert!(
        !report.is_empty(),
        "the unmodified reference tree is not supported"
    );
    let golden: Value =
        serde_json::from_str(&read(&legacy_dir().join("expected-diagnostics.json"))).unwrap();
    // The operator-specified diagnostics, each with file and line, are all produced.
    for required in golden["required"].as_array().unwrap() {
        assert!(
            has(
                &report,
                required["code"].as_str().unwrap(),
                required["file"].as_str().unwrap(),
                required["line"].as_u64().unwrap()
            ),
            "missing required diagnostic {required}"
        );
    }
    let env_lines: Vec<u64> = report
        .iter()
        .filter(|d| d["file"] == "src/config.mjs" && d["construct"] == "process.env")
        .map(|d| d["line"].as_u64().unwrap())
        .collect();
    assert_eq!(
        env_lines,
        [13, 39, 40, 41, 42, 43, 44, 45, 47, 50],
        "every process.env read"
    );
    // Relative imports are resolved and checked, never reported as catalogue packages.
    let packages: Vec<_> = report
        .iter()
        .filter(|d| d["code"] == "WASM-PACKAGE-NOT-IN-CATALOGUE")
        .collect();
    assert_eq!(packages.len(), 2, "only pg and kafkajs: {packages:?}");
    // Recorded results: the partial tree against its golden; the full tree once complete.
    let complete = present_files().len() == reference().files.len();
    if complete {
        match golden["status"].as_str() {
            Some("recorded") => assert_eq!(
                Value::Array(report.clone()),
                golden["diagnostics"],
                "full-tree golden"
            ),
            _ => println!(
                "UNVERIFIED: full-tree golden is pending; computed diagnostics: {}",
                Value::Array(report.clone())
            ),
        }
    } else {
        println!(
            "UNVERIFIED: {} of 6 reference files absent; the full-tree golden was not compared",
            6 - files.len()
        );
        let partial: Value = serde_json::from_str(&read(
            &legacy_dir().join("expected-diagnostics-partial.json"),
        ))
        .unwrap();
        let listed: Vec<&str> = partial["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap())
            .collect();
        let held: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            listed, held,
            "the partial golden describes exactly the files present"
        );
        assert_eq!(
            Value::Array(report.clone()),
            partial["diagnostics"],
            "computed: {}",
            Value::Array(report.clone())
        );
    }
    // The migration result is a recorded BLOCKED, and the check never reaches a build.
    assert_eq!(manifest()["legacy"]["result"].as_str(), Some("blocked"));
}

#[test]
fn mutating_the_reference_tree_changes_the_result() {
    let entry = reference().entry;
    let files = present_files();
    let baseline = tree_report(&files, &entry);
    // Removing the pg import removes exactly its diagnostic.
    let mutated: Vec<_> = files
        .iter()
        .map(|(path, text)| {
            let text = if path == &entry {
                text.replacen("import pg from 'pg';\n", "", 1)
            } else {
                text.clone()
            };
            (path.clone(), text)
        })
        .collect();
    let report = tree_report(&mutated, &entry);
    assert_ne!(report, baseline);
    assert!(has(&baseline, "WASM-PACKAGE-NOT-IN-CATALOGUE", &entry, 1));
    let packages = |report: &[Value]| {
        report
            .iter()
            .filter(|d| d["code"] == "WASM-PACKAGE-NOT-IN-CATALOGUE")
            .count()
    };
    assert_eq!((packages(&baseline), packages(&report)), (2, 1));
    // An injected node:fs import adds a WASM-UNSUPPORTED-API at its line.
    let injected: Vec<_> = files
        .iter()
        .map(|(path, text)| {
            let text = if path == "src/spec-version-repo.mjs" {
                format!("import fs from 'node:fs';\n{text}")
            } else {
                text.clone()
            };
            (path.clone(), text)
        })
        .collect();
    let report = tree_report(&injected, &entry);
    let node_module = |report: &[Value]| {
        report.iter().any(|d| {
            d["file"] == "src/spec-version-repo.mjs"
                && d["line"] == 1
                && d["construct"] == "node module"
        })
    };
    assert!(!node_module(&baseline));
    assert!(node_module(&report));
}

#[test]
fn the_recorded_access_event_describes_topic_payload_and_delivery() {
    let event: Value =
        serde_json::from_str(&read(&legacy_dir().join("access-event.json"))).unwrap();
    assert_eq!(event["topic"], "console.openapi.spec.accessed");
    assert_eq!(event["delivery"], "fire-and-forget");
    assert_eq!(event["payload"]["eventType"], "openapi.spec.accessed");
    assert_eq!(
        event["payload"]["fields"],
        json!([
            "timestamp",
            "workspaceId",
            "tenantId",
            "specVersion",
            "requesterId",
            "format"
        ])
    );
}

// ---------------------------------------------------------------- negative fixtures

fn tree_of(dir: &Path) -> (Vec<(String, String)>, Option<String>) {
    let mut files = Vec::new();
    let mut manifest = None;
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if name == "package.json" {
            manifest = Some(read(&path));
        } else {
            files.push((name, read(&path)));
        }
    }
    files.sort();
    (files, manifest)
}

fn negative_report(dir: &Path, entry: &str, suffix: &str) -> Vec<(String, &'static str, usize)> {
    let (files, manifest) = tree_of(dir);
    let files: Vec<(String, String)> = files
        .into_iter()
        .map(|(path, text)| (path, format!("{text}{suffix}")))
        .collect();
    let tree: Vec<_> = files
        .iter()
        .map(|(path, text)| TreeFile { path, text })
        .collect();
    check_tree(&tree, entry, manifest.as_deref())
        .diagnostics()
        .iter()
        .map(|d| (d.path.clone(), d.code, d.line))
        .collect()
}

#[test]
fn each_negative_fixture_yields_exactly_its_documented_code_with_file_and_line() {
    let manifest = manifest();
    let mut covered = BTreeSet::new();
    for fixture in manifest["negative"].as_array().unwrap() {
        let code = fixture["code"].as_str().unwrap();
        let report = negative_report(
            &root().join(fixture["path"].as_str().unwrap()),
            fixture["entry"].as_str().unwrap(),
            "",
        );
        assert_eq!(
            report,
            [(
                fixture["file"].as_str().unwrap().to_owned(),
                CODES.iter().find(|c| **c == code).copied().unwrap(),
                fixture["line"].as_integer().unwrap() as usize
            )],
            "{code}"
        );
        let message = manifest["diagnostics"][code]["message"].as_str().unwrap();
        assert!(!message.is_empty(), "{code}: a stable message");
        covered.insert(code.to_owned());
    }
    assert_eq!(
        covered,
        CODES.iter().map(|c| (*c).to_owned()).collect(),
        "one fixture per code"
    );
}

#[test]
fn diagnostics_never_set_or_change_the_execution_mode() {
    let manifest = manifest();
    let messages: Vec<String> = manifest["diagnostics"]
        .as_table()
        .unwrap()
        .values()
        .map(|d| d["message"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("OCI")),
        "diagnostics may name OCI as text"
    );
    // OCI mentioned in the source text changes nothing: same diagnostics, same wasm mode.
    for fixture in manifest["negative"].as_array().unwrap() {
        let dir = root().join(fixture["path"].as_str().unwrap());
        let entry = fixture["entry"].as_str().unwrap();
        let plain = negative_report(&dir, entry, "");
        let mentions = negative_report(
            &dir,
            entry,
            "\n// Run this in OCI mode instead; switch the runtime mode to oci.\n",
        );
        assert_eq!(
            mentions
                .iter()
                .map(|d| (d.0.as_str(), d.1))
                .collect::<Vec<_>>(),
            plain
                .iter()
                .map(|d| (d.0.as_str(), d.1))
                .collect::<Vec<_>>(),
            "OCI in the text must not change the result"
        );
    }
    assert_eq!(manifest["profile"]["mode"].as_str(), Some("wasm"));
    // A diagnostic carries no mode at all; the request alone decides it, and it stays wasm.
    let request = read(&root().join("examples/events-handle/request.json"));
    assert!(!api::refuses_runtime_mode(request.as_bytes()));
    let mut oci: Value = serde_json::from_str(&request).unwrap();
    oci["projection"]["runtime_mode"] = json!("oci");
    assert!(api::refuses_runtime_mode(
        &serde_json::to_vec(&oci).unwrap()
    ));
    let parsed = api::parse_request(request.as_bytes()).unwrap();
    assert_eq!(
        serde_json::to_value(&parsed).unwrap()["projection"]["runtime_mode"],
        "wasm"
    );
}

// ---------------------------------------------------------------- precompiled artifacts

fn precompiled_artifacts(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            precompiled_artifacts(&path, found);
            continue;
        }
        let name = path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        let bytes = fs::read(&path).unwrap();
        let magic = bytes.starts_with(b"\0asm")
            || bytes.starts_with(b"\x7fELF")
            || bytes.starts_with(b"MZ");
        let extension = [
            ".wasm", ".cwasm", ".wasmu", ".cmu", ".so", ".dll", ".dylib", ".node", ".a", ".o",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix));
        if magic || extension {
            found.push(path);
        }
    }
}

#[test]
fn no_fixture_is_or_contains_a_precompiled_or_native_artifact() {
    let mut found = Vec::new();
    precompiled_artifacts(&root(), &mut found);
    assert!(
        found.is_empty(),
        "no precompiled artifact may be committed: {found:?}"
    );
    // The scanner does catch one, by magic bytes and by name.
    let scratch = std::env::temp_dir().join(format!("profile-js-artifact-{}", std::process::id()));
    fs::create_dir_all(&scratch).unwrap();
    fs::write(scratch.join("sneaky.txt"), b"\0asm\x01\0\0\0").unwrap();
    fs::write(scratch.join("native.node"), b"text").unwrap();
    let mut seen = Vec::new();
    precompiled_artifacts(&scratch, &mut seen);
    fs::remove_dir_all(&scratch).unwrap();
    assert_eq!(seen.len(), 2);
}

// ---------------------------------------------------------------- status computation

#[test]
fn any_failed_check_computes_blocked_and_only_all_passed_qualifies() {
    assert_eq!(status(&[Check::Passed, Check::Passed]), Status::Qualified);
    assert_eq!(status(&[]), Status::NotQualified);
    assert_eq!(
        status(&[Check::Passed, Check::Unverified("no cluster".into())]),
        Status::NotQualified
    );
    for forced in [
        vec![Check::Passed, Check::Failed("static".into())],
        vec![
            Check::Failed("cluster".into()),
            Check::Unverified("x".into()),
        ],
    ] {
        assert_eq!(status(&forced), Status::Blocked);
    }
    // The checks available here never qualify: the cluster and the two absent files are UNVERIFIED.
    let mut checks: Vec<Check> = reference_checks().into_iter().map(|(_, c)| c).collect();
    checks.push(Check::Unverified("cluster suite not run".into()));
    assert_eq!(status(&checks), Status::NotQualified);
    assert_eq!(
        manifest()["profile"]["qualification"].as_str(),
        Some("not-qualified")
    );
    checks.push(Check::Failed("forced".into()));
    assert_eq!(status(&checks), Status::Blocked);
}

// ---------------------------------------------------------------- events example

fn events_request() -> (Value, api::ExecuteRequest) {
    let text = read(&root().join("examples/events-handle/request.json"));
    (
        serde_json::from_str(&text).unwrap(),
        api::parse_request(text.as_bytes()).unwrap(),
    )
}
const TOPIC: &str = "console.openapi.spec.accessed";
const KEY: &str = "tenants/synthetic/rate/ws-synthetic-0001";

#[test]
fn the_events_example_carries_narrow_grants_in_the_credential_and_the_projection() {
    let (wire, request) = events_request();
    let grants = &request.plan().context.grants;
    assert_eq!(grants.topics, [TOPIC]);
    assert_eq!(grants.prefixes, ["tenants/synthetic/"]);
    assert!(grants.hosts.is_empty() && grants.targets.is_empty());
    assert!(grants.allows_key(KEY));
    assert!(!grants.allows_key("tenants/other/rate/ws"));
    for family in ["credential", "projection"] {
        let list = if family == "credential" {
            &wire["credential"]["allowed_capabilities"]
        } else {
            &wire["projection"]["grants"]
        };
        let text = list.to_string();
        assert!(text.contains(TOPIC), "{family}: the event topic grant");
        assert!(!text.contains('*'), "{family}: no wildcard");
        assert!(
            !text.contains("secretRef"),
            "{family}: no credential material"
        );
    }
    assert_eq!(
        wire["projection"]["imports"],
        json!(["falcone:host/events@0.2.0", "falcone:host/storage@0.2.0"])
    );
    assert_eq!(
        wire["projection"]["language_profile"]["name"],
        manifest()["profile"]["id"].as_str().unwrap()
    );
    // The component imports events, so the host context must carry an event topic. Dropping
    // the grant from either side leaves none and the import would be rejected at instantiation.
    use falcone_serverless_worker::host::import_allowed;
    assert!(import_allowed("falcone:host/events@0.2.0", grants));
    for (side, pointer) in [
        ("credential", "/credential/allowed_capabilities"),
        ("projection", "/projection/grants"),
    ] {
        let mut mutated = wire.clone();
        mutated
            .pointer_mut(pointer)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .retain(|g| g["family"] != "event");
        let parsed = api::parse_request(&serde_json::to_vec(&mutated).unwrap());
        let allowed = parsed
            .is_ok_and(|r| import_allowed("falcone:host/events@0.2.0", &r.plan().context.grants));
        assert!(
            !allowed,
            "without the {side} grant the events import is not allowed"
        );
    }
}

fn context() -> Context {
    events_request().1.plan().context
}

/// Drive the recorded calls through the replaying bindings, as the host would deliver them.
fn deliver(replay: &Replay, calls: &[Call]) -> Vec<Result<Reply, HostError>> {
    calls
        .iter()
        .map(|call| {
            replay.call(
                &context(),
                match call {
                    Call::Read { key, checkpoint } => Operation::Read {
                        key: key.clone(),
                        checkpoint: *checkpoint,
                    },
                    Call::Write {
                        key,
                        checkpoint,
                        data,
                    } => Operation::Write {
                        key: key.clone(),
                        checkpoint: *checkpoint,
                        data: data.clone(),
                    },
                    Call::Publish {
                        destination,
                        payload,
                    } => Operation::Publish {
                        destination: destination.clone(),
                        payload: payload.clone(),
                    },
                    Call::Other(_) => unreachable!(),
                },
            )
        })
        .collect()
}

fn cases() -> Vec<Case> {
    load_cases(&read(&root().join("examples/events-handle/cases.json")))
}
fn replay_for(case: &Case) -> Replay {
    let context = context();
    Replay::new(&case.state, &context.tenant, &context.workspace)
}

#[test]
fn recorded_cases_pin_exact_keys_flags_destinations_and_payload_bytes() {
    let cases = cases();
    assert_eq!(cases.len(), 3);
    let event: Value =
        serde_json::from_str(&read(&legacy_dir().join("access-event.json"))).unwrap();
    for case in &cases {
        assert!(
            case.state.contains_key(KEY),
            "{}: state is replayed for the bucket key",
            case.name
        );
        assert_eq!(
            case.calls[0],
            Call::Read {
                key: KEY.into(),
                checkpoint: false
            },
            "{}",
            case.name
        );
        // Replaying the recorded calls reproduces them exactly, in order.
        let replay = replay_for(case);
        let delivered = deliver(&replay, &case.calls);
        assert!(delivered.iter().all(Result::is_ok), "{}", case.name);
        compare(case, &replay.calls(), &case.response).unwrap();
        for call in &case.calls {
            if let Call::Publish {
                destination,
                payload,
            } = call
            {
                assert_eq!(destination, TOPIC);
                let body: Value = serde_json::from_slice(payload).unwrap();
                let fields: BTreeSet<&str> = body
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                let documented: BTreeSet<&str> = event["payload"]["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f.as_str().unwrap())
                    .chain(["eventType"])
                    .collect();
                assert_eq!(fields, documented, "{}: payload fields", case.name);
                assert_eq!(body["eventType"], event["payload"]["eventType"]);
            }
        }
    }
    // The params agree with what was recorded: same workspace, and the stored bucket's minute
    // is the params' clock divided into minutes.
    for case in &cases {
        assert_eq!(
            case.params["workspaceId"], "ws-synthetic-0001",
            "{}",
            case.name
        );
        assert!(
            case.params["__ow_path"]
                .as_str()
                .unwrap()
                .contains("ws-synthetic-0001")
        );
        let minute = case.params["now"].as_u64().unwrap() / 60_000;
        for call in &case.calls {
            if let Call::Write { data, .. } = call {
                let bucket: Value = serde_json::from_slice(data).unwrap();
                assert_eq!(bucket["minute"], minute, "{}", case.name);
            }
        }
    }
    // The full bucket case reads the replayed state and does nothing else.
    assert_eq!(cases[2].calls.len(), 1);
    assert_eq!(cases[2].response["statusCode"], 429);
}

fn mutations(case: &Case) -> Vec<(String, Vec<Call>, Value)> {
    let mut out = Vec::new();
    for index in 0..case.calls.len() {
        let mut calls = case.calls.clone();
        match &mut calls[index] {
            Call::Read { key, .. } => key.push('x'),
            Call::Write { key, .. } => key.push('x'),
            Call::Publish { destination, .. } => destination.push('x'),
            Call::Other(_) => {}
        }
        out.push((
            format!("key or destination of call {index}"),
            calls,
            case.response.clone(),
        ));
        let mut calls = case.calls.clone();
        match &mut calls[index] {
            Call::Read { checkpoint, .. } | Call::Write { checkpoint, .. } => {
                *checkpoint = !*checkpoint
            }
            Call::Publish { payload, .. } => payload.push(b' '),
            Call::Other(_) => {}
        }
        out.push((
            format!("flag or payload of call {index}"),
            calls,
            case.response.clone(),
        ));
        if let Call::Write { data, .. } = &case.calls[index] {
            let mut calls = case.calls.clone();
            if let Call::Write { data: changed, .. } = &mut calls[index] {
                *changed = data
                    .iter()
                    .map(|b| if *b == b'1' { b'9' } else { *b })
                    .collect();
            }
            out.push((
                format!("written bytes of call {index}"),
                calls,
                case.response.clone(),
            ));
        }
        let mut calls = case.calls.clone();
        calls.remove(index);
        out.push((
            format!("dropped call {index}"),
            calls,
            case.response.clone(),
        ));
    }
    let mut calls = case.calls.clone();
    calls.push(Call::Publish {
        destination: TOPIC.into(),
        payload: b"{}".to_vec(),
    });
    out.push(("an extra event".into(), calls, case.response.clone()));
    let mut response = case.response.clone();
    response["statusCode"] = json!(500);
    out.push(("changed response".into(), case.calls.clone(), response));
    out.push((
        "swapped order".into(),
        case.calls.iter().rev().cloned().collect(),
        case.response.clone(),
    ));
    out
}

#[test]
fn a_mutated_key_destination_payload_or_response_fails_and_keeps_the_status_not_qualified() {
    for case in &cases() {
        compare(case, &case.calls, &case.response).unwrap();
        for (what, calls, response) in mutations(case) {
            if calls == case.calls && response == case.response {
                continue; // a one-call case reversed is itself
            }
            let error =
                compare(case, &calls, &response).expect_err(&format!("{}: {what}", case.name));
            assert!(error.contains(&case.name));
            // One recorded mismatch is a failed check: the computed status is blocked, never qualified.
            assert_eq!(
                status(&[Check::Passed, Check::Failed(error)]),
                Status::Blocked,
                "{what}"
            );
        }
    }
}

#[test]
fn dropping_the_replayed_state_changes_what_the_host_returns() {
    let cases = cases();
    let full = &cases[2];
    // With the recorded full bucket, the read returns the recorded bytes.
    let replay = replay_for(full);
    let reply = replay.call(
        &context(),
        Operation::Read {
            key: KEY.into(),
            checkpoint: false,
        },
    );
    assert!(
        matches!(reply, Ok(Reply::Data(ref bytes)) if bytes == br#"{"minute":29906640,"count":2}"#)
    );
    // Without it, the same read is denied: the 429 response cannot be reproduced.
    let dropped = Replay::new(&BTreeMap::new(), &context().tenant, &context().workspace);
    let reply = dropped.call(
        &context(),
        Operation::Read {
            key: KEY.into(),
            checkpoint: false,
        },
    );
    assert!(matches!(reply, Err(HostError::Denied)));
    assert_eq!(
        dropped.calls(),
        [Call::Read {
            key: KEY.into(),
            checkpoint: false
        }]
    );
    // A write is observable by a later read, and an unrelated operation never matches a case.
    let writes = replay_for(&cases[0]);
    writes
        .call(
            &context(),
            Operation::Write {
                key: KEY.into(),
                checkpoint: false,
                data: b"{}".to_vec(),
            },
        )
        .map(drop)
        .unwrap();
    let reply = writes.call(
        &context(),
        Operation::Read {
            key: KEY.into(),
            checkpoint: false,
        },
    );
    assert!(matches!(reply, Ok(Reply::Data(ref bytes)) if bytes == b"{}"));
    let other = replay_for(full);
    let reply = other.call(
        &context(),
        Operation::Start {
            workflow: "w".into(),
            input: vec![],
        },
    );
    assert!(reply.is_err());
    assert!(matches!(other.calls()[0], Call::Other(_)));
    assert!(compare(full, &other.calls(), &full.response).is_err());
}

// ---------------------------------------------------------------- documentation

#[test]
fn published_documentation_matches_the_manifest_and_the_matrix() {
    let document = read(&worker().join("../docs/qualification/profiles/javascript.md"));
    let manifest = manifest();
    let toolchain = &manifest["toolchain"];
    let mut tokens: Vec<String> = vec![
        manifest["profile"]["id"].as_str().unwrap().to_owned(),
        manifest["profile"]["matrix_revision"]
            .as_str()
            .unwrap()
            .to_owned(),
        TOPIC.into(),
        "disabled".into(),
        "blocked".into(),
        "UNVERIFIED".into(),
    ];
    for field in [
        "wasmtime",
        "jco",
        "componentize_js",
        "node",
        "wasi",
        "wasi_virt_rev",
    ] {
        tokens.push(toolchain[field].as_str().unwrap().to_owned());
    }
    tokens.extend(strings(&manifest["wit"]["interfaces"]));
    tokens.extend(
        manifest["workloads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].as_str().unwrap().to_owned()),
    );
    tokens.extend(CODES.iter().map(|c| (*c).to_owned()));
    tokens.extend(
        reference()
            .files
            .into_iter()
            .flat_map(|(path, sha)| [path, sha]),
    );
    for token in tokens {
        assert!(
            document.contains(&token),
            "documentation is missing `{token}`"
        );
    }
    // A stale or wrong value is not accepted by the same check.
    assert!(!document.contains("49.0.4"));
    assert!(!document.contains("javascript-wasi-v0"));
}

#[test]
fn requests_for_built_components_are_admitted_with_their_real_imports() {
    let (wire, _) = events_request();
    let digest = format!("sha256:{}", "a".repeat(64));
    let params = json!({"__ow_path": "/v1/workspaces/ws-synthetic-0001/openapi"});
    // The pure workloads import nothing; the events example imports storage and events.
    for imports in [
        vec![],
        vec!["falcone:host/storage@0.2.0".to_owned()],
        strings(&manifest()["workloads"][3]["imports"]),
    ] {
        let request = request_for(&wire, &digest, &imports, &params);
        let parsed = api::parse_request(&serde_json::to_vec(&request).unwrap()).unwrap();
        assert_eq!(parsed.plan().imports, imports);
        assert_eq!(parsed.plan().params, serde_json::to_vec(&params).unwrap());
    }
}

#[test]
fn the_build_script_builds_only_the_positive_examples_after_the_static_checks() {
    let script = read(&worker().join("qualification/javascript/build-profile.sh"));
    for workload in manifest()["workloads"].as_array().unwrap() {
        let name = workload["name"].as_str().unwrap();
        assert!(script.contains(name), "{name} is built");
    }
    // The legacy reference function is a recorded BLOCKED result: it is never componentized.
    for legacy in [
        "openapi-spec-serve",
        "profiles/javascript/legacy",
        "spec-cache",
        "wrapper.mjs",
    ] {
        assert!(!script.contains(legacy), "{legacy} must not be built");
    }
    let static_checks = script
        .find("--test profile_javascript\n")
        .expect("static checks run");
    let first_build = script
        .find("\"$jco\" componentize")
        .expect("components are built");
    assert!(
        static_checks < first_build,
        "static checks precede any build"
    );
    assert!(script.contains("--deny-all") && script.contains("--disable all"));
    assert!(
        script.contains("FS_TRUSTED_COMPILER_IMAGE"),
        "operator opt-in"
    );
}
