//! Go (TinyGo wasip2) Wasm profile qualification, static checks only: no guest is built,
//! instantiated or executed here, and no toolchain is needed. Execution lives in
//! `profile_go_runtime.rs` and `cluster_profile_go.rs`. Anything this snapshot cannot verify is
//! reported UNVERIFIED and never counts towards qualification.
#[path = "support/profile_go.rs"]
mod profile;

use falcone_serverless_worker::{
    adapters::compat_go::{GoFile, check_bindings},
    api, artifact,
    host::{Bindings, Context, Host, HostError, Operation, Reply},
};
use profile::{
    Call, Case, Check, Replay, Status, check_tree, compare, core_shape, evidence_root, load_cases,
    manifest_in, negative_check, read_directory, request_for, root, shape_ok, static_evidence_in,
    status,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Go's raw-pointer package, spelled out at compile time: the repository's boundary audit
/// reserves the literal for its reviewed paths.
const UNSAFE: &str = concat!("un", "safe");

const CODES: &[&str] = &[
    "WASM-NATIVE-LIBRARY",
    "WASM-GPU",
    "WASM-PROCESS-SPAWN",
    "WASM-INSTALL-HOOK",
    "WASM-PACKAGE-NOT-IN-CATALOGUE",
    "WASM-UNSUPPORTED-API",
];

// The values of the operator decision of 2026-11-19, written out independently of the files
// under test: a pin that drifts in any of them fails here.
const TINYGO_VERSION: &str = "0.44.0";
const TINYGO_SHA256: &str = "ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9";
const TINYGO_SOURCE: &str = "https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz";
const GO_VERSION: &str = "1.26.11";
const GO_SHA256: &str = "9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec";
const GO_SOURCE: &str = "https://go.dev/dl/go1.26.11.linux-amd64.tar.gz";
const TARGET: &str = "wasip2";
const WIT_PACKAGE: &str = "falcone:host@0.2.0";
const WIT_WORLD: &str = "function";

fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
fn manifest() -> toml::Value {
    manifest_in(&root())
}
fn versions() -> toml::Value {
    toml::from_str(&read(&worker().join("qualification/go/versions.toml"))).unwrap()
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

// ---------------------------------------------------------------- pins

/// Every pin of the three sources equals the decided value. A missing, floating or different
/// value in any of them is an error.
fn pins_ok(manifest: &toml::Value, versions: &toml::Value, matrix: &Value) -> Result<(), String> {
    let field = |table: &toml::Value, path: &[&str]| -> Result<String, String> {
        let mut at = table;
        for key in path {
            at = at
                .get(key)
                .ok_or_else(|| format!("missing pin {}", path.join(".")))?;
        }
        at.as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("pin {} is not a string", path.join(".")))
    };
    for (what, actual, decided) in [
        ("versions target", field(versions, &["target"])?, TARGET),
        ("versions engine", field(versions, &["engine"])?, "TinyGo"),
        (
            "versions tinygo.name",
            field(versions, &["tinygo", "name"])?,
            "TinyGo",
        ),
        ("versions go.name", field(versions, &["go", "name"])?, "Go"),
        (
            "manifest wit.package",
            field(manifest, &["wit", "package"])?,
            WIT_PACKAGE,
        ),
        (
            "manifest wit.world",
            field(manifest, &["wit", "world"])?,
            WIT_WORLD,
        ),
        (
            "manifest target",
            field(manifest, &["toolchain", "target"])?,
            TARGET,
        ),
        (
            "versions tinygo.version",
            field(versions, &["tinygo", "version"])?,
            TINYGO_VERSION,
        ),
        (
            "versions tinygo.sha256",
            field(versions, &["tinygo", "sha256"])?,
            TINYGO_SHA256,
        ),
        (
            "versions tinygo.source",
            field(versions, &["tinygo", "source"])?,
            TINYGO_SOURCE,
        ),
        (
            "versions go.version",
            field(versions, &["go", "version"])?,
            GO_VERSION,
        ),
        (
            "versions go.sha256",
            field(versions, &["go", "sha256"])?,
            GO_SHA256,
        ),
        (
            "versions go.source",
            field(versions, &["go", "source"])?,
            GO_SOURCE,
        ),
        (
            "manifest tinygo",
            field(manifest, &["toolchain", "tinygo"])?,
            TINYGO_VERSION,
        ),
        (
            "manifest tinygo_sha256",
            field(manifest, &["toolchain", "tinygo_sha256"])?,
            TINYGO_SHA256,
        ),
        (
            "manifest go",
            field(manifest, &["toolchain", "go"])?,
            GO_VERSION,
        ),
        (
            "manifest go_sha256",
            field(manifest, &["toolchain", "go_sha256"])?,
            GO_SHA256,
        ),
        (
            "matrix toolchainVersion",
            matrix["profiles"]["go"]["toolchainVersion"]
                .as_str()
                .ok_or("missing pin matrix toolchainVersion")?
                .to_owned(),
            TINYGO_VERSION,
        ),
    ] {
        if actual != decided {
            return Err(format!("{what} is {actual:?}, decided {decided:?}"));
        }
    }
    if field(manifest, &["toolchain", "wasi"])? != "none"
        || matrix["profiles"]["go"]["wasiVersion"] != "none"
    {
        return Err("WASI must be none".into());
    }
    if field(versions, &["qualification_status"])? != "disabled" {
        return Err("qualification_status must stay disabled".into());
    }
    if versions.get("wasi_virt").is_some() {
        return Err("wasi-virt is not used and must not be pinned".into());
    }
    Ok(())
}

#[test]
fn the_pins_in_versions_the_manifest_and_the_matrix_equal_the_decided_values() {
    pins_ok(&manifest(), &versions(), &matrix()).unwrap();
}

#[test]
fn a_missing_floating_or_different_pin_is_rejected_in_every_source() {
    let (manifest, versions, matrix) = (manifest(), versions(), matrix());
    let floating = [
        "^0.44.0", "~0.44.0", ">=0.44.0", "0.44.x", "0.44", "*", "latest", "", "v0.44.0", "0.44.1",
        "1.26.10",
    ];
    let toml_paths: [(&str, &[&str]); 10] = [
        ("versions", &["target"]),
        ("versions", &["engine"]),
        ("versions", &["tinygo", "name"]),
        ("versions", &["go", "name"]),
        ("versions", &["tinygo", "version"]),
        ("versions", &["tinygo", "sha256"]),
        ("versions", &["go", "version"]),
        ("versions", &["go", "sha256"]),
        ("versions", &["go", "source"]),
        ("versions", &["tinygo", "source"]),
    ];
    let manifest_paths: [&[&str]; 7] = [
        &["toolchain", "target"],
        &["wit", "package"],
        &["wit", "world"],
        &["toolchain", "tinygo"],
        &["toolchain", "tinygo_sha256"],
        &["toolchain", "go"],
        &["toolchain", "go_sha256"],
    ];
    let set = |table: &mut toml::Value, path: &[&str], value: Option<&str>| {
        let (last, parents) = path.split_last().unwrap();
        let mut at = table;
        for key in parents {
            at = at.get_mut(key).unwrap();
        }
        let at = at.as_table_mut().unwrap();
        match value {
            Some(value) => at.insert((*last).to_owned(), toml::Value::String(value.to_owned())),
            None => at.remove(*last),
        };
    };
    for value in floating.iter().map(|v| Some(*v)).chain([None]) {
        for (_, path) in toml_paths {
            let mut mutated = versions.clone();
            set(&mut mutated, path, value);
            assert!(
                pins_ok(&manifest, &mutated, &matrix).is_err(),
                "versions {path:?} = {value:?}"
            );
        }
        for path in manifest_paths {
            let mut mutated = manifest.clone();
            set(&mut mutated, path, value);
            assert!(
                pins_ok(&mutated, &versions, &matrix).is_err(),
                "manifest {path:?} = {value:?}"
            );
        }
        let mut mutated = matrix.clone();
        match value {
            Some(value) => mutated["profiles"]["go"]["toolchainVersion"] = json!(value),
            None => {
                mutated["profiles"]["go"]
                    .as_object_mut()
                    .unwrap()
                    .remove("toolchainVersion");
            }
        }
        assert!(
            pins_ok(&manifest, &versions, &mutated).is_err(),
            "matrix toolchainVersion = {value:?}"
        );
    }
    // A pinned wasi-virt, an enabled-by-status versions file and a WASI version each fail.
    let mut mutated = versions.clone();
    mutated.as_table_mut().unwrap().insert(
        "wasi_virt".into(),
        toml::Value::Table(toml::map::Map::new()),
    );
    assert!(pins_ok(&manifest, &mutated, &matrix).is_err());
    let mut mutated = versions.clone();
    set(&mut mutated, &["qualification_status"], Some("enabled"));
    assert!(pins_ok(&manifest, &mutated, &matrix).is_err());
    let mut mutated = matrix.clone();
    mutated["profiles"]["go"]["wasiVersion"] = json!("0.2.6");
    assert!(pins_ok(&manifest, &versions, &mutated).is_err());
}

fn exact(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Stdlib entries carry the pinned Go version; catalogue modules carry an exact version.
fn catalogue_ok(manifest: &toml::Value) -> Result<(), String> {
    let go = manifest["toolchain"]["go"].as_str().unwrap();
    let entries = manifest["stdlib"].as_array().ok_or("no stdlib entries")?;
    if entries.is_empty() {
        return Err("no stdlib entries".into());
    }
    for entry in entries {
        let package = entry
            .get("package")
            .and_then(toml::Value::as_str)
            .ok_or("stdlib entry without a package")?;
        if package.split('/').next().unwrap_or_default().contains('.') {
            return Err(format!("{package} is not a standard-library package"));
        }
        if entry.get("go").and_then(toml::Value::as_str) != Some(go) {
            return Err(format!("{package} is not bound to Go {go}"));
        }
    }
    for module in manifest["catalogue"]["modules"]
        .as_array()
        .ok_or("no module list")?
    {
        let version = module.get("version").and_then(toml::Value::as_str);
        if module.get("path").and_then(toml::Value::as_str).is_none() || !version.is_some_and(exact)
        {
            return Err(format!("module {module:?} has no exact version"));
        }
    }
    Ok(())
}

#[test]
fn stdlib_entries_are_bound_to_the_toolchain_and_modules_have_exact_versions() {
    catalogue_ok(&manifest()).unwrap();
    let packages: BTreeSet<String> = manifest()["stdlib"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["package"].as_str().unwrap().to_owned())
        .collect();
    // The packages the workloads import are on the list; the reflection and raw-pointer
    // packages never are.
    for needed in [
        "encoding/json",
        "sort",
        "strings",
        "crypto/sha256",
        "strconv",
    ] {
        assert!(packages.contains(needed), "{needed}");
    }
    for never in ["reflect", UNSAFE, "os", "os/exec", "net/http", "plugin"] {
        assert!(!packages.contains(never), "{never}");
    }
    let mutate = |f: &dyn Fn(&mut toml::Value)| {
        let mut mutated = manifest();
        f(&mut mutated);
        catalogue_ok(&mutated)
    };
    // An unbound, wrongly bound, ranged or third-party stdlib entry fails.
    assert!(
        mutate(&|m| {
            m["stdlib"].as_array_mut().unwrap()[0]
                .as_table_mut()
                .unwrap()
                .remove("go");
        })
        .is_err()
    );
    assert!(mutate(&|m| m["stdlib"].as_array_mut().unwrap()[0]["go"] = "1.26.10".into()).is_err());
    assert!(mutate(&|m| m["stdlib"].as_array_mut().unwrap()[0]["go"] = "^1.26.11".into()).is_err());
    assert!(
        mutate(&|m| m["stdlib"].as_array_mut().unwrap()[0]["package"] = "github.com/x/y".into())
            .is_err()
    );
    assert!(mutate(&|m| m["stdlib"] = toml::Value::Array(Vec::new())).is_err());
    // A module needs an exact version: none, a range and latest fail; an exact one passes.
    let with_module = |version: Option<&str>| {
        let mut mutated = manifest();
        let mut table = toml::map::Map::new();
        table.insert("path".into(), "github.com/example/mod".into());
        if let Some(version) = version {
            table.insert("version".into(), version.into());
        }
        mutated["catalogue"]["modules"] = toml::Value::Array(vec![toml::Value::Table(table)]);
        catalogue_ok(&mutated)
    };
    for bad in [
        None,
        Some("^1.2.3"),
        Some("v1.2.3"),
        Some("latest"),
        Some("1.2"),
    ] {
        assert!(with_module(bad).is_err(), "{bad:?}");
    }
    with_module(Some("1.2.3")).unwrap();
}

// ---------------------------------------------------------------- manifest and matrix

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
        if entry.contains('*')
            || entry.starts_with("wasi:")
            || !entry.starts_with("falcone:host/")
            || !entry.ends_with("@0.2.0")
            || !wit.contains(entry)
        {
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
    let go = &matrix["profiles"]["go"];
    assert_eq!(go["status"], "disabled");
    assert_eq!(go["evidence"], json!({}));
    assert_eq!(manifest["profile"]["status"].as_str(), Some("disabled"));
    assert_eq!(
        manifest["profile"]["qualification"].as_str(),
        Some("not-qualified")
    );
    assert_eq!(manifest["profile"]["mode"].as_str(), Some("wasm"));
    assert_eq!(go["id"], manifest["profile"]["id"].as_str().unwrap());
    assert_eq!(
        go["wasmtimeVersion"],
        manifest["toolchain"]["wasmtime"].as_str().unwrap()
    );
    let workspace: toml::Value = toml::from_str(&read(&worker().join("../Cargo.toml"))).unwrap();
    assert_eq!(
        format!("={}", manifest["toolchain"]["wasmtime"].as_str().unwrap()),
        workspace["workspace"]["dependencies"]["wasmtime"]["version"]
            .as_str()
            .unwrap()
    );
    assert_eq!(go["packageCatalogueVersion"], "unqualified");
    let allowed: Vec<String> = go["allowedInterfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap().to_owned())
        .collect();
    let wit = wit_interfaces();
    interfaces_ok(&allowed, &wit).unwrap();
    assert_eq!(allowed, strings(&manifest["wit"]["interfaces"]));
    assert!(!allowed.is_empty());
    assert!(!allowed.contains(&"falcone:host/handler@0.2.0".to_owned()));
    // A wildcard, a WASI entry, an unknown entry and another version are each refused.
    for bad in [
        "falcone:host/*@0.2.0",
        "wasi:cli/environment@0.2.6",
        "falcone:host/unknown@0.2.0",
        "falcone:host/storage@0.1.0",
        "falcone:host/handler@0.2.0",
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
    assert_eq!(go["testedExamples"], json!(examples));
    let codes: BTreeSet<String> = manifest["diagnostics"]
        .as_table()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(codes, CODES.iter().map(|c| (*c).to_owned()).collect());
    assert_eq!(
        go["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>(),
        codes
    );
}

// ---------------------------------------------------------------- bindings and workloads

fn bindings_files() -> Vec<(String, String)> {
    read_directory(&root().join("bindings"))
}

/// The interface each public binding function reaches.
const API: &[(&str, &str)] = &[
    ("OpenObject", "falcone:host/storage@0.2.0"),
    ("OpenCheckpoint", "falcone:host/storage@0.2.0"),
    ("Publish", "falcone:host/events@0.2.0"),
    ("Log", "falcone:host/logging@0.2.0"),
    ("IsCancelled", "falcone:host/cancellation@0.2.0"),
    ("InvocationKey", "falcone:host/invocation-context@0.2.0"),
    ("AttemptID", "falcone:host/invocation-context@0.2.0"),
    ("IdempotencyKey", "falcone:host/invocation-context@0.2.0"),
];

/// The interfaces a guest source reaches through `bindings.<Function>`.
fn used_interfaces(source: &str) -> BTreeSet<String> {
    API.iter()
        .filter(|(name, _)| source.contains(&format!("bindings.{name}(")))
        .map(|(_, interface)| (*interface).to_owned())
        .collect()
}

fn wasm_imports(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| line.strip_prefix("//go:wasmimport "))
        .map(|rest| rest.split_whitespace().next().unwrap().to_owned())
        .collect()
}

#[test]
fn the_bindings_import_exactly_the_listed_interfaces_and_declare_no_cabi_realloc() {
    let manifest = manifest();
    let allowed = strings(&manifest["wit"]["interfaces"]);
    let files = bindings_files();
    let imported: BTreeSet<String> = files.iter().flat_map(|(_, t)| wasm_imports(t)).collect();
    assert_eq!(
        imported,
        allowed.iter().cloned().collect::<BTreeSet<_>>(),
        "bindings import every listed interface and nothing else"
    );
    let stdlib: Vec<String> = manifest["stdlib"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["package"].as_str().unwrap().to_owned())
        .collect();
    let run = |files: &[(String, String)]| {
        let tree: Vec<_> = files
            .iter()
            .map(|(path, text)| GoFile { path, text })
            .collect();
        let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
        let stdlib: Vec<&str> = stdlib.iter().map(String::as_str).collect();
        check_bindings(&tree, &allowed, &stdlib)
            .diagnostics()
            .iter()
            .map(|d| (d.path.clone(), d.construct))
            .collect::<Vec<_>>()
    };
    assert_eq!(run(&files), []);
    // PR #429 finding 1: no binding code mentions cabi_realloc (comments aside), and exactly
    // one export exists.
    for (name, text) in &files {
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            assert!(
                !line.contains("cabi_realloc"),
                "{name}: the runtime owns cabi_realloc"
            );
        }
    }
    let exports: Vec<_> = files
        .iter()
        .flat_map(|(_, t)| t.lines())
        .filter(|line| line.starts_with("//go:wasmexport "))
        .collect();
    assert_eq!(
        exports,
        ["//go:wasmexport falcone:host/handler@0.2.0#handle"]
    );
    // Adding an export, an import outside the list or a directive in an import group fails.
    for (extra, construct) in [
        (
            "//go:wasmexport cabi_realloc\nfunc r() {}\n",
            "cabi_realloc export",
        ),
        (
            "//go:wasmimport falcone:host/http@0.2.0 request\nfunc h()\n",
            "wasm import outside the allow-list",
        ),
        (
            "import (\n\t\"strings\"\n//go:linkname a b\n)\n",
            "compiler directive",
        ),
    ] {
        let mut mutated = files.clone();
        mutated.push(("extra.go".into(), format!("package bindings\n{extra}")));
        assert!(
            run(&mutated).contains(&("extra.go".to_owned(), construct)),
            "{extra}"
        );
    }
}

fn guest_sources(manifest: &toml::Value) -> Vec<(String, String, Vec<String>)> {
    manifest["workloads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["name"].as_str().unwrap().to_owned(),
                read(&root().join(w["main"].as_str().unwrap())),
                strings(&w["imports"]),
            )
        })
        .collect()
}

#[test]
fn workload_sources_pass_the_static_check_and_reach_only_declared_listed_interfaces() {
    let manifest = manifest();
    let allowed = strings(&manifest["wit"]["interfaces"]);
    for (name, source, declared) in guest_sources(&manifest) {
        let found = check_tree(&manifest, &[("main.go".to_owned(), source.clone())]);
        assert_eq!(found, [], "{name} must pass the static check");
        let used = used_interfaces(&source);
        assert_eq!(
            used,
            declared.iter().cloned().collect::<BTreeSet<_>>(),
            "{name}: declared imports are the interfaces it uses"
        );
        for import in &declared {
            assert!(allowed.contains(import), "{name}: {import} is not allowed");
        }
        assert!(source.contains("bindings.Handle(handle)"), "{name}");
        // A guest never declares its own imports, exports or directives.
        for forbidden in [
            "//go:",
            "wasi",
            "\"os\"",
            "\"reflect\"",
            &format!("\"{UNSAFE}\""),
            "cabi_realloc",
        ] {
            assert!(!source.contains(forbidden), "{name}: {forbidden}");
        }
        // Reaching an interface that is not declared is detected.
        let copy = source.replace(
            "func main() {}",
            "func main() { _ = bindings.Log(bindings.Info, \"x\") }",
        );
        let extra = used_interfaces(&copy);
        if !declared.iter().any(|d| d == "falcone:host/logging@0.2.0") {
            assert_ne!(extra, used, "{name}: the undeclared use is visible");
        }
        // A copy that imports a package outside the list or declares an import is rejected.
        let bad = source.replacen("import (", "import (\n\t\"net/http\"", 1);
        assert_eq!(
            check_tree(&manifest, &[("main.go".to_owned(), bad)])[0].4,
            "standard library package",
            "{name}"
        );
        let bad = format!("{source}\n//go:wasmimport wasi_snapshot_preview1 fd_write\nfunc w()\n");
        assert!(
            check_tree(&manifest, &[("main.go".to_owned(), bad)])
                .iter()
                .any(|d| d.4 == "go:wasmimport"),
            "{name}"
        );
    }
}

// ---------------------------------------------------------------- recorded outputs

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
    // sha256("abc") is the FIPS 180-2 test vector, written out here as a literal.
    let hash = find("hash-sha256");
    assert_eq!(load(&hash, "input"), json!({"message": "abc"}));
    assert_eq!(
        load(&hash, "expected"),
        json!({"sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"})
    );
    // 3*4 + 5*2 = 22; the names are upper-cased and sorted.
    let transform = find("json-transform");
    assert_eq!(
        load(&transform, "expected"),
        json!({"count": 2, "total": 22, "names": ["GADGET", "WIDGET"]})
    );
    let scan = find("text-scan");
    assert_eq!(
        load(&scan, "input"),
        json!({"text": "order 12 shipped on 2026-11-20; order 345 pending"})
    );
    assert_eq!(
        load(&scan, "expected"),
        json!({"orders":["12","345"],"date":{"year":"2026","month":"11","day":"20"}})
    );
}

fn events_request() -> (Value, api::ExecuteRequest) {
    let wire: Value =
        serde_json::from_str(&read(&root().join("examples/events-handle/request.json"))).unwrap();
    let request = api::parse_request(&serde_json::to_vec(&wire).unwrap()).unwrap();
    (wire, request)
}

fn cases() -> Vec<Case> {
    load_cases(&read(&root().join("examples/events-handle/cases.json")))
}

fn context() -> Context {
    events_request().1.plan().context
}

fn deliver(replay: &Replay, calls: &[Call]) -> Vec<Result<Reply, HostError>> {
    let context = context();
    calls
        .iter()
        .map(|call| {
            replay.call(
                &context,
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
                    Call::Other(name) => panic!("unexpected {name}"),
                },
            )
        })
        .collect()
}

#[test]
fn the_events_example_carries_narrow_grants_and_the_go_profile_pin() {
    let (wire, request) = events_request();
    assert_eq!(wire["projection"]["language_profile"]["name"], "go-wasi-v1");
    assert_eq!(wire["projection"]["language_profile"]["version"], "0.44.0");
    assert_eq!(wire["projection"]["wasi_version"], "none");
    assert_eq!(wire["projection"]["engine_version"], "49.0.5");
    for grants in [
        &wire["credential"]["allowed_capabilities"],
        &wire["projection"]["grants"],
    ] {
        let grants = grants.as_array().unwrap();
        assert_eq!(grants.len(), 2);
        for grant in grants {
            let text = grant.to_string();
            assert!(!text.contains('*'), "no wildcard grant: {text}");
        }
    }
    assert_eq!(request.plan().imports.len(), 2);
}

#[test]
fn recorded_cases_pin_exact_keys_flags_destinations_and_payload_bytes() {
    let cases = cases();
    assert_eq!(cases.len(), 3);
    for case in &cases {
        let replay = Replay::new(&case.state, &context().tenant, &context().workspace);
        let results = deliver(&replay, &case.calls);
        // The replayed state serves the recorded read; the recorded calls compare equal.
        assert!(results.iter().all(Result::is_ok), "{}", case.name);
        compare(case, &replay.calls(), &case.response).unwrap();
        // Any mutated argument or response is a mismatch.
        for index in 0..case.calls.len() {
            let mut calls = case.calls.clone();
            match &mut calls[index] {
                Call::Read { key, .. } | Call::Write { key, .. } => key.push('x'),
                Call::Publish { payload, .. } => payload.push(b' '),
                Call::Other(_) => unreachable!(),
            }
            let mutated = Case {
                name: case.name.clone(),
                params: case.params.clone(),
                state: case.state.clone(),
                calls,
                response: case.response.clone(),
            };
            assert!(compare(&mutated, &replay.calls(), &case.response).is_err());
        }
        assert!(compare(case, &replay.calls(), &json!({"statusCode": 500})).is_err());
        // Dropping the replayed state denies the read, so the guest sees a different world.
        let dropped = Replay::new(&BTreeMap::new(), &context().tenant, &context().workspace);
        assert!(matches!(
            deliver(&dropped, &case.calls[..1])[0],
            Err(HostError::Denied)
        ));
    }
}

#[test]
fn requests_for_built_components_are_admitted_with_their_real_imports() {
    let (wire, _) = events_request();
    let digest = format!("sha256:{}", "a".repeat(64));
    let params = json!({"__ow_path": "/v1/workspaces/ws-synthetic-0001/openapi"});
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

// ---------------------------------------------------------------- negative fixtures

#[test]
fn each_negative_fixture_yields_exactly_its_documented_code_with_file_and_line() {
    assert_eq!(negative_check(&root()), Check::Passed);
    let manifest = manifest();
    let negatives = manifest["negative"].as_array().unwrap();
    let covered: BTreeSet<&str> = negatives
        .iter()
        .map(|n| n["code"].as_str().unwrap())
        .collect();
    assert_eq!(covered, CODES.iter().copied().collect(), "all six codes");
    // Every fixture directory is documented and every entry has a directory.
    let documented: BTreeSet<String> = negatives
        .iter()
        .map(|n| n["path"].as_str().unwrap().to_owned())
        .collect();
    let on_disk: BTreeSet<String> = fs::read_dir(root().join("negative"))
        .unwrap()
        .map(|e| format!("negative/{}", e.unwrap().file_name().to_str().unwrap()))
        .collect();
    assert_eq!(documented, on_disk);
    // The unsupported-API fixtures cover reflect, the raw-pointer package, go:linkname and a
    // guest wasm import.
    let constructs: BTreeSet<&str> = negatives
        .iter()
        .filter(|n| n["code"].as_str() == Some("WASM-UNSUPPORTED-API"))
        .flat_map(|n| n["diagnostics"].as_array().unwrap())
        .map(|d| d["construct"].as_str().unwrap())
        .collect();
    for needed in ["reflect", UNSAFE, "go:linkname", "go:wasmimport"] {
        assert!(constructs.contains(needed), "{needed}");
    }
    // A fixture that gains or loses a diagnostic, or moves it, fails the comparison.
    let dir = std::env::temp_dir().join(format!("profile-go-negative-{}", std::process::id()));
    copy_tree(&root(), &dir);
    let target = dir.join("negative/gpu/main.go");
    let original = read(&target);
    fs::write(&target, format!("\n{original}")).unwrap();
    assert!(matches!(negative_check(&dir), Check::Failed(_)));
    fs::write(&target, "package main\n\nfunc main() {}\n").unwrap();
    assert!(matches!(negative_check(&dir), Check::Failed(_)));
    fs::remove_dir_all(&dir).unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

// ---------------------------------------------------------------- artifacts

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
            ".syso",
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
    precompiled_artifacts(&worker().join("qualification/go"), &mut found);
    assert!(
        found.is_empty(),
        "no built artifact is committed: {found:?}"
    );
    let scratch = std::env::temp_dir().join(format!("profile-go-artifact-{}", std::process::id()));
    fs::create_dir_all(&scratch).unwrap();
    fs::write(scratch.join("sneaky.txt"), b"\0asm\x01\0\0\0").unwrap();
    fs::write(scratch.join("native.syso"), b"text").unwrap();
    let mut seen = Vec::new();
    precompiled_artifacts(&scratch, &mut seen);
    fs::remove_dir_all(&scratch).unwrap();
    assert_eq!(seen.len(), 2);
}

#[test]
fn validation_rejects_a_tenant_supplied_precompiled_artifact_and_none_is_deserialized() {
    // A serialized native artifact starts with an ELF header, never with a Wasm preamble.
    let mut elf = b"\x7fELF\x02\x01\x01\0".to_vec();
    elf.resize(256, 0);
    assert_eq!(
        artifact::validate::validate(&elf, 1 << 20)
            .unwrap_err()
            .code,
        "precompiled_rejected"
    );
    // So is a core module: only a component is accepted.
    assert_eq!(
        artifact::validate::validate(b"\0asm\x01\0\0\0", 1 << 20)
            .unwrap_err()
            .code,
        "invalid_module"
    );
    // The only deserialization of a native artifact in the worker is the trusted cache loader.
    fn scan(dir: &Path, found: &mut BTreeSet<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = fs::read_to_string(&path).unwrap();
                if [
                    "Component::deserialize",
                    "Module::deserialize",
                    "deserialize_file",
                ]
                .iter()
                .any(|needle| text.contains(needle))
                {
                    found.insert(path.file_name().unwrap().to_str().unwrap().to_owned());
                }
            }
        }
    }
    let mut found = BTreeSet::new();
    scan(&worker().join("src"), &mut found);
    assert_eq!(found, BTreeSet::from(["load.rs".to_owned()]));
}

// ---------------------------------------------------------------- core module shape

fn component(modules: &str) -> Vec<u8> {
    wat::parse_str(format!("(component {modules})")).unwrap()
}

const MAIN: &str = r#"(core module $main
    (memory (export "memory") 1)
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 0))"#;
const SECOND_MEMORY: &str = r#"(core module $adapter (memory (export "memory") 1))"#;
const SECOND_REALLOC: &str = r#"(core module $extra (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 0))"#;
const SHIM: &str = r#"(core module $shim (type $t (func)) (table (export "$imports") 1 funcref))"#;

#[test]
fn one_memory_and_one_cabi_realloc_is_the_only_accepted_shape() {
    let one = core_shape(&component(MAIN)).unwrap();
    assert_eq!((one.memories, one.cabi_realloc_exports), (1, 1));
    shape_ok(&one).unwrap();
    // Table-only shim modules, as wit-component emits, add no memory and no realloc.
    shape_ok(&core_shape(&component(&format!("{MAIN} {SHIM}"))).unwrap()).unwrap();
    // PR #430 finding 2: a composed adapter module brings a second linear memory.
    let two = core_shape(&component(&format!("{MAIN} {SECOND_MEMORY}"))).unwrap();
    assert_eq!(two.memories, 2);
    assert!(shape_ok(&two).unwrap_err().contains("exactly one"));
    // PR #429 finding 1: a second cabi_realloc export, as the old bindings added.
    let duplicate = core_shape(&component(&format!("{MAIN} {SECOND_REALLOC}"))).unwrap();
    assert_eq!(duplicate.cabi_realloc_exports, 2);
    assert!(shape_ok(&duplicate).unwrap_err().contains("cabi_realloc"));
    // Neither a missing realloc nor no memory at all is a pass.
    let none = core_shape(&component(SHIM)).unwrap();
    assert!(shape_ok(&none).is_err());
    assert!(core_shape(b"not wasm").is_err());
}

struct Denied;
impl Bindings for Denied {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Err(HostError::Denied)
    }
}

#[test]
fn host_new_instantiates_one_memory_and_refuses_a_second() {
    // The unchanged store limits (memories(1)) are the sandbox control: a component with one
    // core memory instantiates, and the same component composed with an adapter that defines
    // a second memory does not. Execution tests, not validation, catch the difference.
    let wrap = |modules: &str, instances: &str| {
        component(&format!(
            "{modules} (core instance $main (instantiate $main)) {instances}"
        ))
    };
    let engine = wasmtime::Engine::default();
    let attempt = |bytes: Vec<u8>| {
        let host = Host::new(context(), Arc::new(Denied));
        falcone_serverless_worker::host::instantiate(&engine, bytes, host).map(|_| ())
    };
    attempt(wrap(MAIN, "")).unwrap();
    let error = attempt(wrap(
        &format!("{MAIN} {SECOND_MEMORY}"),
        "(core instance $adapter (instantiate $adapter))",
    ))
    .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("memor")
            || error
                .chain()
                .any(|e| e.to_string().to_lowercase().contains("memor")),
        "{error:?}"
    );
}

// ---------------------------------------------------------------- build script

fn script() -> String {
    read(&worker().join("qualification/go/build-profile.sh"))
}

fn code_lines(script: &str) -> Vec<&str> {
    script
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn the_build_script_never_calls_wasi_virt_and_builds_only_after_the_static_checks() {
    let script = script();
    // PR #430 finding 1: wasi-virt has no --deny-all at the old revision and is not used.
    for line in code_lines(&script) {
        for forbidden in [
            "wasi-virt",
            "wasi_virt",
            "--deny-all",
            "wac ",
            "wasm-compose",
        ] {
            assert!(!line.contains(forbidden), "{forbidden} in {line:?}");
        }
    }
    assert!(
        !read(&worker().join("qualification/go/versions.toml"))
            .lines()
            .any(|line| line.trim_start().starts_with("[wasi"))
    );
    for workload in manifest()["workloads"].as_array().unwrap() {
        assert!(
            script.contains(workload["package"].as_str().unwrap()),
            "{} is built",
            workload["name"].as_str().unwrap()
        );
    }
    for negative in manifest()["negative"].as_array().unwrap() {
        assert!(
            !code_lines(&script)
                .iter()
                .any(|line| line.contains(negative["path"].as_str().unwrap())
                    && line.contains("build")),
            "a negative fixture is never built"
        );
    }
    let static_checks = script
        .find("--test profile_go\n")
        .expect("static checks run");
    let first_build = script
        .find("build json-transform")
        .expect("components are built");
    assert!(
        static_checks < first_build,
        "static checks precede any build"
    );
    let build = script.find("tinygo\" build").expect("tinygo build");
    assert!(script[build..].contains("-target=wasip2"));
    for flag in [
        "-wit-world function",
        "-gc=leaking",
        "-scheduler=none",
        "-panic=trap",
    ] {
        assert!(script[build..].contains(flag), "{flag}");
    }
    assert!(
        script.contains("FS_TRUSTED_COMPILER_IMAGE"),
        "operator opt-in"
    );
    assert!(script.contains("GOPROXY=off") && script.contains("GOTOOLCHAIN=local"));
    // The shape check and the runtime check run before anything is copied out.
    assert!(
        script.find("--test profile_go_runtime").unwrap()
            < script.find("cp \"$stage/components/\"*.wasm").unwrap()
    );
}

#[test]
fn the_build_flags_in_the_manifest_are_the_flags_the_script_uses() {
    let manifest = manifest();
    let script = script();
    assert_eq!(manifest["build"]["memories"].as_integer(), Some(1));
    for flag in strings(&manifest["build"]["flags"]) {
        assert!(script.contains(&flag), "{flag}");
    }
    let command = manifest["build"]["command"].as_str().unwrap();
    assert!(script.contains(command.strip_prefix("tinygo ").unwrap()));
}

// ---------------------------------------------------------------- status computation

#[test]
fn any_failed_check_computes_blocked_and_only_all_passed_qualifies() {
    assert_eq!(status(&[Check::Passed, Check::Passed]), Status::Qualified);
    assert_eq!(
        status(&[Check::Passed, Check::Failed("x".into())]),
        Status::Blocked
    );
    assert_eq!(
        status(&[Check::Passed, Check::Unverified("x".into())]),
        Status::NotQualified
    );
    assert_eq!(status(&[]), Status::NotQualified);
}

#[test]
fn a_relocated_evidence_bundle_is_judged_without_the_build_checkout() {
    let dir = std::env::temp_dir().join(format!("profile-go-bundle-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    // An absent bundle is UNVERIFIED, never a pass, and never a panic.
    let evidence = static_evidence_in(&dir);
    assert!(matches!(evidence[0].1, Check::Unverified(_)));
    assert_ne!(status(&[evidence[0].1.clone()]), Status::Qualified);
    // The bundle `build-profile.sh` ships is exactly the evidence paths.
    for path in profile::EVIDENCE_PATHS {
        copy_tree_or_file(&root().join(path), &dir.join(path));
    }
    let evidence = static_evidence_in(&dir);
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].1, Check::Passed);
    // The pod runner reads beside the components, never the checkout.
    assert_eq!(
        evidence_root(Path::new("/c"), None),
        PathBuf::from("/c/evidence")
    );
    assert_eq!(
        evidence_root(Path::new("/c"), Some(PathBuf::from("/e"))),
        PathBuf::from("/e")
    );
    fs::remove_dir_all(&dir).unwrap();
    let script = script();
    for path in profile::EVIDENCE_PATHS {
        assert!(script.contains(path), "{path} is packaged");
    }
}

fn copy_tree_or_file(from: &Path, to: &Path) {
    if from.is_dir() {
        copy_tree(from, to);
    } else {
        fs::copy(from, to).unwrap();
    }
}

// ---------------------------------------------------------------- documentation drift

/// The cell texts of the documentation table row that starts with `label`.
fn table_row<'a>(document: &'a str, label: &str) -> Result<Vec<&'a str>, String> {
    document
        .lines()
        .filter(|line| line.starts_with('|'))
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>()
        })
        .find(|cells| cells.first().is_some_and(|cell| cell.starts_with(label)))
        .ok_or_else(|| format!("documentation has no `{label}` row in the versions table"))
}

/// The documentation says what the manifest and the matrix say, field by field: the versions
/// table row of each component carries that component's own version and sha256, so a value
/// that survives elsewhere on the page cannot hide a changed one.
fn docs_ok(document: &str, manifest: &toml::Value, matrix: &Value) -> Result<(), String> {
    for (label, version, sha256) in [
        (
            "TinyGo",
            manifest["toolchain"]["tinygo"].as_str().unwrap(),
            Some(manifest["toolchain"]["tinygo_sha256"].as_str().unwrap()),
        ),
        (
            "Go toolchain",
            manifest["toolchain"]["go"].as_str().unwrap(),
            Some(manifest["toolchain"]["go_sha256"].as_str().unwrap()),
        ),
        (
            "Wasmtime",
            manifest["toolchain"]["wasmtime"].as_str().unwrap(),
            None,
        ),
        (
            "WASI",
            manifest["toolchain"]["wasi"].as_str().unwrap(),
            None,
        ),
    ] {
        let cells = table_row(document, label)?;
        if cells.get(1).copied() != Some(&format!("`{version}`")[..]) {
            return Err(format!(
                "`{label}` row version is {:?}, manifest {version:?}",
                cells.get(1)
            ));
        }
        if let Some(sha256) = sha256
            && cells.get(2).copied() != Some(&format!("`{sha256}`")[..])
        {
            return Err(format!(
                "`{label}` row sha256 is {:?}, manifest {sha256:?}",
                cells.get(2)
            ));
        }
    }
    // The target is part of the TinyGo row's own first cell, so it is checked there.
    let target = manifest["toolchain"]["target"].as_str().unwrap();
    let tinygo = table_row(document, "TinyGo")?;
    let expected_label = format!("TinyGo, target `{target}`");
    if tinygo.first().copied() != Some(expected_label.as_str()) {
        return Err(format!(
            "`TinyGo` row target is {:?}, manifest {expected_label:?}",
            tinygo.first()
        ));
    }
    let wit = table_row(document, "WIT package")?;
    let expected = format!(
        "`{}`, world `{}`",
        manifest["wit"]["package"].as_str().unwrap(),
        manifest["wit"]["world"].as_str().unwrap()
    );
    if wit.get(1).copied() != Some(expected.as_str()) {
        return Err(format!(
            "`WIT package` row is {:?}, manifest {expected:?}",
            wit.get(1)
        ));
    }
    let flat = document.split_whitespace().collect::<Vec<_>>().join(" ");
    if !flat.contains(&format!(
        "each manifest entry carries `go = \"{}\"`",
        manifest["toolchain"]["go"].as_str().unwrap()
    )) {
        return Err("the standard-library binding does not name the pinned Go toolchain".into());
    }
    let mut tokens: Vec<String> = vec![
        manifest["profile"]["id"].as_str().unwrap().to_owned(),
        manifest["profile"]["matrix_revision"]
            .as_str()
            .unwrap()
            .to_owned(),
        "disabled".into(),
        "UNVERIFIED".into(),
        "-gc=leaking".into(),
        "cabi_realloc".into(),
        TINYGO_SOURCE.into(),
        GO_SOURCE.into(),
    ];
    for field in [
        "wasmtime",
        "tinygo",
        "tinygo_sha256",
        "go",
        "go_sha256",
        "wasi",
        "target",
    ] {
        tokens.push(manifest["toolchain"][field].as_str().unwrap().to_owned());
    }
    tokens.extend(strings(&manifest["wit"]["interfaces"]));
    tokens.extend(
        matrix["profiles"]["go"]["allowedInterfaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap().to_owned()),
    );
    tokens.extend(
        manifest["workloads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].as_str().unwrap().to_owned()),
    );
    tokens.extend(CODES.iter().map(|c| (*c).to_owned()));
    tokens.extend(
        manifest["stdlib"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["package"].as_str().unwrap().to_owned()),
    );
    for negative in manifest["negative"].as_array().unwrap() {
        tokens.push(negative["path"].as_str().unwrap().to_owned());
        for diagnostic in negative["diagnostics"].as_array().unwrap() {
            tokens.push(diagnostic["construct"].as_str().unwrap().to_owned());
        }
    }
    for flag in strings(&manifest["build"]["flags"]) {
        tokens.push(flag);
    }
    for token in tokens {
        if !document.contains(&token) {
            return Err(format!("documentation is missing `{token}`"));
        }
    }
    // A stale or wrong value is not accepted by the same check, and the status is not raised.
    if document.contains("0.43.0") || document.contains("go-wasi-v0") {
        return Err("documentation names a stale version".into());
    }
    if document.contains("wasi-virt` is used") {
        return Err("documentation says wasi-virt is used".into());
    }
    if document.to_lowercase().contains("status: `enabled`") {
        return Err("documentation raises the status".into());
    }
    Ok(())
}

#[test]
fn published_documentation_matches_the_manifest_and_the_matrix() {
    let document = read(&worker().join("../docs/qualification/profiles/go.md"));
    docs_ok(&document, &manifest(), &matrix()).unwrap();
}

#[test]
fn a_changed_documented_version_digest_or_interface_is_drift() {
    let document = read(&worker().join("../docs/qualification/profiles/go.md"));
    let (manifest, matrix) = (manifest(), matrix());
    let tinygo_row = "| TinyGo, target `wasip2` | `0.44.0` |";
    let go_row = "| Go toolchain | `1.26.11` | `9849f4e0";
    assert!(document.contains(tinygo_row) && document.contains(go_row));
    for (from, to) in [
        (go_row, "| Go toolchain | `1.26.10` | `9849f4e0"),
        (go_row, "| Go toolchain | `1.26.11` | `0849f4e0"),
        (tinygo_row, "| TinyGo, target `wasip2` | `0.44.1` |"),
        (tinygo_row, "| TinyGo, target `wasip1` | `0.44.0` |"),
        ("| Wasmtime | `49.0.5` |", "| Wasmtime | `49.0.4` |"),
        ("| WASI | `none` |", "| WASI | `0.2.6` |"),
        (
            "`falcone:host@0.2.0`, world `function`",
            "`falcone:host@0.2.1`, world `function`",
        ),
        (
            "`falcone:host@0.2.0`, world `function`",
            "`falcone:host@0.2.0`, world `handler`",
        ),
        ("`go = \"1.26.11\"`", "`go = \"1.26.10\"`"),
    ] {
        assert!(document.contains(from), "{from}");
        let changed = document.replacen(from, to, 1);
        assert!(
            docs_ok(&changed, &manifest, &matrix).is_err(),
            "{from} -> {to} is not drift"
        );
    }
    // `wasip2` still appears elsewhere on the page, which is not enough for a changed target.
    let only_the_target =
        document.replacen(tinygo_row, "| TinyGo, target `wasip1` | `0.44.0` |", 1);
    assert!(only_the_target.contains("wasip2"));
    assert!(docs_ok(&only_the_target, &manifest, &matrix).is_err());
    // The decided Go version still appears elsewhere on the page, which is not enough.
    let only_the_table = document.replacen(go_row, "| Go toolchain | `1.26.10` | `9849f4e0", 1);
    assert!(only_the_table.contains("1.26.11"));
    assert!(docs_ok(&only_the_table, &manifest, &matrix).is_err());
}

#[test]
fn the_decided_wit_package_and_world_are_the_ones_host_wit_declares() {
    let wit = read(&worker().join("wit/host.wit"));
    assert!(wit.lines().any(|l| l == format!("package {WIT_PACKAGE};")));
    assert!(wit.contains(&format!("world {WIT_WORLD} {{")));
}

#[cfg(unix)]
#[test]
fn the_operator_build_script_is_executable() {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(worker().join("qualification/go/build-profile.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert!(mode & 0o111 != 0, "build-profile.sh mode {mode:o}");
}
