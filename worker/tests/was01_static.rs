//! WAS-01 qualification inputs; validates source only, never executes a guest.
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use toml::Value;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

fn document(path: &str) -> Value {
    toml::from_str(&fs::read_to_string(root().join(path)).unwrap()).unwrap()
}

fn inventory_valid(root: &Path, path: &str, digest: &str) -> bool {
    let path = root.join(path);
    path.is_file()
        && fs::read(path).is_ok_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == digest)
}

#[test]
fn pinned_versions_and_inventory() {
    let versions = document("worker/qualification/was-01/versions.toml");
    let workspace = document("Cargo.toml");
    let dependency = &workspace["workspace"]["dependencies"]["wasmtime"];
    let requirement = dependency["version"].as_str().unwrap();
    assert!(requirement.starts_with('='));
    assert_eq!(dependency["default-features"].as_bool(), Some(false));
    assert_eq!(
        dependency["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["cranelift", "runtime", "std", "wat", "component-model"]
    );
    assert_eq!(
        document("worker/Cargo.toml")["dependencies"]["wasmtime"]["workspace"].as_bool(),
        Some(true)
    );
    let lock = document("Cargo.lock");
    let packages: Vec<_> = lock["package"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"].as_str() == Some("wasmtime"))
        .collect();
    assert_eq!(packages.len(), 1);
    assert_eq!(
        packages[0]["version"].as_str(),
        requirement.strip_prefix('=')
    );
    assert_eq!(
        versions["wasmtime_version"].as_str(),
        requirement.strip_prefix('=')
    );
    // The qualification inventory records the exact engine build, so a feature change is visible there.
    assert_eq!(versions["wasmtime_default_features"].as_bool(), Some(false));
    assert_eq!(versions["wasmtime_features"], dependency["features"]);
    for (key, expected) in [
        ("compilation_backend", "cranelift"),
        ("wasi_interface", "none"),
        ("qualification_status", "unqualified"),
        ("target_arch", std::env::consts::ARCH),
        ("target_os", std::env::consts::OS),
        ("kubernetes_version", "unverified"),
        ("openshift_version", "unverified"),
        ("namespaced_permissions", "unverified"),
    ] {
        assert_eq!(versions[key].as_str(), Some(expected), "{key}");
    }
    assert_eq!(
        versions["rust_toolchain"],
        document("rust-toolchain.toml")["toolchain"]["channel"]
    );
    let path = versions["contract_inventory_path"].as_str().unwrap();
    let digest = versions["contract_inventory_sha256"].as_str().unwrap();
    assert!(inventory_valid(root(), path, digest));
    // The v1 inventory digest is retired: the pin follows the v1.2 inventory (OPS-01.t1).
    let v1_digest = "c9c4921595cfe5e0662e082d1af860453e4c40e49e473f4baa213fa11f871973";
    assert_ne!(digest, v1_digest);
    assert!(!inventory_valid(root(), path, v1_digest));
    assert!(!inventory_valid(root(), path, &"0".repeat(64)));
    assert!(!inventory_valid(root(), "missing-was01-inventory", digest));
    assert!(!inventory_valid(root(), "docs/contracts", digest));
    assert!(!inventory_valid(root(), path, &digest.to_uppercase()));
}

fn fixture_names_valid(names: &[String]) -> bool {
    let mut names = names.to_vec();
    names.sort();
    names
        == [
            "capability-checkpoint.wat",
            "capability-context.wat",
            "capability-events.wat",
            "capability-handles.wat",
            "capability-http.wat",
            "capability-import.wat",
            "capability-logging.wat",
            "capability-storage.wat",
            "capability-table.wat",
            "capability-workflows.wat",
            "infinite_loop.wat",
            "valid.wat",
        ]
}

fn fixture_valid(name: &str, source: &str) -> bool {
    // Module::new uses the locked wat parser and wasmparser validator. No instance
    // is created and no guest executes. Exact source tokens also freeze the bodies.
    let engine = wasmtime::Engine::default();
    let Ok(module) = wasmtime::Module::new(&engine, source) else {
        return false;
    };
    let expected = match name {
        "valid.wat" => "(module (func (export \"run\") (result i32) i32.const 42))",
        "infinite_loop.wat" => "(module (func (export \"run\") (loop br 0)))",
        _ => return false,
    };
    let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    module.imports().len() == 0 && compact(source) == compact(expected)
}

#[test]
fn embedded_fixtures_are_exact_and_import_free() {
    let dir = root().join("worker/src/qualification/fixtures");
    let mut names = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assert!(fixture_names_valid(&names));
    for required in names.iter() {
        let missing = names
            .iter()
            .filter(|name| *name != required)
            .cloned()
            .collect::<Vec<_>>();
        assert!(!fixture_names_valid(&missing), "missing {required}");
    }
    names.push("extra.wat".into());
    assert!(!fixture_names_valid(&names));
    for name in ["valid.wat", "infinite_loop.wat"] {
        let source = fs::read_to_string(dir.join(name)).unwrap();
        assert!(fixture_valid(name, &source), "{name}");
        let imported = source.replacen("(module", "(module (import \"host\" \"call\" (func))", 1);
        assert!(!fixture_valid(name, &imported));
        assert!(!fixture_valid(
            name,
            &source.replace("42", "41").replace("br 0", "br 1")
        ));
    }
}

/// Wasmtime entry points that load precompiled native code. No file under `worker/src` may
/// name one, except `cache/load.rs`, the single trusted-cache loader.
const ARTIFACT_LOAD_APIS: [&str; 6] = [
    "Module::deserialize",
    "Component::deserialize",
    "deserialize_file",
    "deserialize_raw",
    "deserialize_open_file",
    "deserialize_open",
];

/// The hard rule is that a tenant-supplied precompiled artifact is never deserialized. The
/// guard therefore rejects the word `deserialize` in every source except `cache/load.rs`,
/// with one narrow allowance: `worker/src/api/` parses JSON requests with serde, so serde
/// deserialization (`Deserialize`, a hand-written `fn deserialize`, `deserialize_with`) is
/// allowed there. Every file, `worker/src/api/` included, still fails if it names a Wasmtime
/// artifact-loading API, and the `wasmtime::` boundary below keeps Wasmtime out of the module.
fn deserialize_allowed(path: &Path, source: &str) -> bool {
    let load = path == Path::new("cache/load.rs");
    let api = path.starts_with("api");
    load || (api && !ARTIFACT_LOAD_APIS.iter().any(|name| source.contains(name)))
}

fn source_valid(path: &Path, source: &str) -> bool {
    let engine = path == Path::new("qualification/engine.rs");
    let bin = path == Path::new("bin/qualification_runner.rs");
    let outcome = path == Path::new("qualification/outcome.rs");
    let runner = path == Path::new("qualification/runner.rs");
    let capability = [
        "host/mod.rs",
        "host/net.rs",
        "host/runtime.rs",
        "host/execute.rs",
        "main.rs",
        "qualification/execute_cases.rs",
        "qualification/capability_http.rs",
        "qualification/capability_invocations.rs",
    ]
    .iter()
    .any(|allowed| path == Path::new(allowed));
    (!source.contains("deserialize") || deserialize_allowed(path, source))
        && (path == Path::new("cache/load.rs")
            || !ARTIFACT_LOAD_APIS.iter().any(|name| source.contains(name)))
        && !source.contains("tests/")
        && (!source.contains("wasmtime::") || engine || bin || capability)
        && (!source.contains("Status::Passed") || outcome)
        && (!(runner || bin) || (!source.contains("env::args") && !source.contains("Engine::new")))
}

fn scan_sources(dir: &Path, prefix: &Path) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan_sources(&path, prefix);
        } else {
            let bytes = fs::read(&path).unwrap();
            assert!(
                source_valid(
                    path.strip_prefix(prefix).unwrap(),
                    &String::from_utf8_lossy(&bytes)
                ),
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn qualification_sources_obey_boundaries() {
    let src = root().join("worker/src");
    scan_sources(&src, &src);
    let engine = fs::read_to_string(src.join("qualification/engine.rs")).unwrap();
    assert!(engine.contains(".increment_epoch()"));
    assert!(engine.contains("Module::new("));
    assert!(engine.contains("include_str!(\"fixtures/valid.wat\")"));
    assert!(engine.contains("include_str!(\"fixtures/infinite_loop.wat\")"));
    let bin = fs::read_to_string(src.join("bin/qualification_runner.rs")).unwrap();
    assert!(bin.contains("run_checks("));
    for (path, source) in [
        ("qualification/engine.rs", "deserialize"),
        ("config.rs", "wasmtime::Engine"),
        ("host/other.rs", "wasmtime::Engine"),
        ("host/execute.rs.bak", "wasmtime::Engine"),
        ("qualification/execute_other.rs", "wasmtime::Engine"),
        ("host/nested/runtime.rs", "wasmtime::Engine"),
        ("qualification/capability_other.rs", "wasmtime::Engine"),
        (
            "qualification/capability_invocations.rs.bak",
            "wasmtime::Engine",
        ),
        ("host/runtime.rs", "deserialize"),
        ("host/runtime.rs", "Status::Passed"),
        ("qualification/runner.rs", "Status::Passed"),
        ("qualification/runner.rs", "env::args()"),
        ("bin/qualification_runner.rs", "Engine::new()"),
        (
            "qualification/compile_cache_runner.rs",
            "include_bytes!(\"../../tests/fixtures/was-02/native.elf\")",
        ),
    ] {
        assert!(!source_valid(Path::new(path), source), "{path}: {source}");
    }
}

#[test]
fn native_load_exception_is_exactly_one_source_path() {
    assert!(source_valid(Path::new("cache/load.rs"), "deserialize"));
    for sibling in [
        "cache/other.rs",
        "cache/nested/load.rs",
        "artifact/load.rs",
        "qualification/load.rs",
        "cache/load.rs.bak",
    ] {
        assert!(
            !source_valid(Path::new(sibling), "deserialize"),
            "{sibling}"
        );
    }
}

#[test]
fn api_may_parse_json_with_serde_but_never_load_artifacts() {
    let serde_code = "fn deserialize<'de, D: Deserializer<'de>>(d: D) {} #[serde(deserialize_with = \"present\")]";
    for allowed in ["api/mod.rs", "api/request.rs", "api/nested/deep.rs"] {
        assert!(source_valid(Path::new(allowed), serde_code), "{allowed}");
    }
    for denied in [
        "Module::deserialize(&engine, bytes)",
        "Component::deserialize_file(&engine, path)",
        "engine.deserialize_raw(bytes)",
        "wasmtime::Module::new(&engine, bytes)",
        "Module::deserialize_open_file(&engine, file)",
    ] {
        assert!(
            !source_valid(Path::new("api/request.rs"), denied),
            "{denied}"
        );
        assert!(
            !source_valid(Path::new("api/nested/deep.rs"), denied),
            "{denied}"
        );
    }
    // The same serde code anywhere else, `cache/load.rs` aside, still fails.
    for elsewhere in [
        "config.rs",
        "api.rs",
        "api_extra/mod.rs",
        "cache/other.rs",
        "host/runtime.rs",
        "checkpoint.rs",
    ] {
        assert!(
            !source_valid(Path::new(elsewhere), serde_code),
            "{elsewhere}"
        );
    }
    // The trusted loader keeps its exception, and only it may name the loading APIs.
    assert!(source_valid(
        Path::new("cache/load.rs"),
        "Module::deserialize(&e, b)"
    ));
    for path in [
        "host/runtime.rs",
        "qualification/engine.rs",
        "cache/other.rs",
    ] {
        assert!(
            !source_valid(Path::new(path), "Module::deserialize_file(&e, p)"),
            "{path}"
        );
    }
}
