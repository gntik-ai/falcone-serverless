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
    assert!(!inventory_valid(root(), path, &"0".repeat(64)));
    assert!(!inventory_valid(root(), "missing-was01-inventory", digest));
    assert!(!inventory_valid(root(), "docs/contracts", digest));
    assert!(!inventory_valid(root(), path, &digest.to_uppercase()));
}

fn fixture_names_valid(names: &[String]) -> bool {
    let mut names = names.to_vec();
    names.sort();
    names == ["infinite_loop.wat", "valid.wat"]
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

fn source_valid(path: &Path, source: &str) -> bool {
    let engine = path == Path::new("qualification/engine.rs");
    let bin = path == Path::new("bin/qualification_runner.rs");
    let outcome = path == Path::new("qualification/outcome.rs");
    let runner = path == Path::new("qualification/runner.rs");
    !source.contains("deserialize")
        && (!source.contains("wasmtime::") || engine || bin)
        && (!source.contains("Status::Passed") || outcome)
        && (!(runner || bin) || (!source.contains("env::args") && !source.contains("Engine::new")))
}

fn scan_sources(dir: &Path, prefix: &Path) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan_sources(&path, prefix);
        } else {
            assert!(
                source_valid(
                    path.strip_prefix(prefix).unwrap(),
                    &fs::read_to_string(&path).unwrap()
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
        ("qualification/runner.rs", "Status::Passed"),
        ("qualification/runner.rs", "env::args()"),
        ("bin/qualification_runner.rs", "Engine::new()"),
    ] {
        assert!(!source_valid(Path::new(path), source), "{path}: {source}");
    }
}
