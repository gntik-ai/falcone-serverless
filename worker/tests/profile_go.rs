//! Local profile checks. No Go fixture is built or executed by this suite.
use std::{fs, path::PathBuf};

fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn root() -> PathBuf {
    worker().join("qualification/profiles/go")
}
fn read(path: impl AsRef<std::path::Path>) -> String {
    fs::read_to_string(path).unwrap()
}
fn manifest() -> toml::Value {
    toml::from_str(&read(root().join("manifest.toml"))).unwrap()
}

#[test]
fn operator_pins_are_exact_in_both_files() {
    let versions: toml::Value =
        toml::from_str(&read(worker().join("qualification/go/versions.toml"))).unwrap();
    let manifest = manifest();
    for (key, name, version, source, archive, sha256) in [
        (
            "tinygo",
            "TinyGo",
            "0.44.0",
            "https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz",
            "tinygo0.44.0.linux-amd64.tar.gz",
            "ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9",
        ),
        (
            "go",
            "Go",
            "1.26.11",
            "https://go.dev/dl/go1.26.11.linux-amd64.tar.gz",
            "go1.26.11.linux-amd64.tar.gz",
            "9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec",
        ),
    ] {
        for (field, expected) in [
            ("name", name),
            ("version", version),
            ("source", source),
            ("archive", archive),
            ("sha256", sha256),
        ] {
            assert_eq!(
                versions[key][field].as_str(),
                Some(expected),
                "{key}.{field}"
            );
            assert_eq!(
                manifest["toolchain"][key][field], versions[key][field],
                "{key}.{field} drift"
            );
        }
    }
    assert_eq!(manifest["toolchain"]["target"].as_str(), Some("wasip2"));
    assert_eq!(manifest["toolchain"]["wasi"].as_str(), Some("none"));
    assert_eq!(
        manifest["stdlib"]["toolchain"].as_str(),
        Some("Go 1.26.11 / TinyGo 0.44.0")
    );
    assert!(
        manifest["catalogue"]["packages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn manifest_interfaces_examples_and_matrix_agree() {
    let manifest = manifest();
    let interfaces = manifest["wit"]["interfaces"].as_array().unwrap();
    assert!(!interfaces.is_empty());
    assert_eq!(
        manifest["wit"]["package"].as_str(),
        Some("falcone:host@0.2.0")
    );
    assert_eq!(manifest["wit"]["world"].as_str(), Some("function"));
    let mut resolve = wit_parser::Resolve::default();
    let (package, _) = resolve.push_path(worker().join("wit/host.wit")).unwrap();
    let package = &resolve.packages[package];
    let wit: std::collections::BTreeSet<_> = resolve.worlds[package.worlds["function"]]
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
    let names: std::collections::BTreeSet<_> = interfaces
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), interfaces.len(), "duplicate interface");
    assert_eq!(names, wit);
    assert_eq!(
        names,
        falcone_serverless_worker::artifact::validate::ALLOWED_IMPORTS
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let workloads = manifest["workloads"].as_array().unwrap();
    assert_eq!(workloads.len(), 3);
    for workload in workloads {
        assert!(example_imports_allowed(workload, interfaces));
        let mut mutated = workload.clone();
        mutated["imports"] =
            toml::Value::Array(vec![toml::Value::String("wasi:cli/run@0.2.0".into())]);
        assert!(!example_imports_allowed(&mutated, interfaces));
    }
    let bytes = fs::read(worker().join("../docs/contracts/language-profile-matrix.json")).unwrap();
    assert_eq!(
        bytes,
        fs::read(worker().join("src/adapters/language-profile-matrix.json")).unwrap()
    );
    let matrix: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let go = &matrix["profiles"]["go"];
    assert_eq!(go["id"], manifest["profile"]["id"].as_str().unwrap());
    assert_eq!(go["status"], "disabled");
    assert_eq!(
        manifest["profile"]["qualification"].as_str(),
        Some("not-qualified")
    );
    assert_eq!(go["evidence"], serde_json::json!({}));
    assert_eq!(go["wasiVersion"], "none");
    assert_eq!(go["toolchainVersion"], "0.44.0");
    assert_eq!(
        go["wasmtimeVersion"],
        manifest["toolchain"]["wasmtime"].as_str().unwrap()
    );
    assert_eq!(
        go["allowedInterfaces"],
        serde_json::json!(
            interfaces
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(
        go["testedExamples"],
        serde_json::json!(
            workloads
                .iter()
                .map(|w| w["name"].as_str().unwrap())
                .collect::<Vec<_>>()
        )
    );
    let codes = go["diagnostics"].as_array().unwrap();
    assert_eq!(codes.len(), 6);
    assert_eq!(manifest["diagnostics"].as_table().unwrap().len(), 6);
    for code in codes {
        assert!(
            manifest["diagnostics"]
                .get(code.as_str().unwrap())
                .is_some()
        );
    }
}
fn example_imports_allowed(workload: &toml::Value, interfaces: &[toml::Value]) -> bool {
    workload["imports"]
        .as_array()
        .unwrap()
        .iter()
        .all(|name| interfaces.contains(name))
}

use falcone_serverless_worker::adapters::{compat::TreeFile, compat_go::check_tree};

#[test]
fn native_import_is_rejected_at_its_source_location() {
    let verdict = check_tree(&[TreeFile {
        path: "main.go",
        text: "package main\nimport \"C\"\nfunc main() {}\n",
    }]);
    let diagnostics = verdict.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "WASM-NATIVE-LIBRARY");
    assert_eq!(diagnostics[0].path, "main.go");
    assert_eq!(diagnostics[0].line, 2);
}

fn assert_directive_positions(directive: &str, code: &str) {
    for (source, line) in [
        (
            format!("package main\nimport (\n//{directive}\n\"strings\"\n)\nfunc main() {{}}"),
            3,
        ),
        (
            format!(
                "package main\nimport \"strings\"\n//{directive}\nimport \"strconv\"\nfunc main() {{}}"
            ),
            3,
        ),
        (format!("package main\n//{directive}\nfunc main() {{}}"), 2),
        (
            format!("package main\nimport (\n/* {directive} */\n\"strings\"\n)\nfunc main() {{}}"),
            3,
        ),
        (
            format!("/* comment\n{directive}\n*/\npackage main\nfunc main() {{}}"),
            2,
        ),
    ] {
        let verdict = check_tree(&[TreeFile {
            path: "main.go",
            text: &source,
        }]);
        assert_eq!(
            verdict.diagnostics().len(),
            1,
            "{directive} at line {line}: {verdict:?}"
        );
        let d = &verdict.diagnostics()[0];
        assert_eq!((d.code, d.path.as_str(), d.line), (code, "main.go", line));
        assert!(!d.construct.contains("forbidden"));
    }
}

#[test]
fn directives_generate_are_checked_independently_of_import_traversal() {
    assert_directive_positions("go:generate echo forbidden", "WASM-INSTALL-HOOK");
}
#[test]
fn directives_linkname_are_checked_independently_of_import_traversal() {
    assert_directive_positions("go:linkname local remote", "WASM-UNSUPPORTED-API");
}
#[test]
fn directives_wasmimport_are_checked_independently_of_import_traversal() {
    assert_directive_positions("go:wasmimport arbitrary name", "WASM-UNSUPPORTED-API");
}
#[test]
fn ordinary_comments_and_directive_text_in_strings_are_eligible() {
    let valid = "package main\nimport (/* ordinary comment */ \"strings\")\nvar s = `//go:generate ignored`\nfunc main() {}";
    assert!(
        check_tree(&[TreeFile {
            path: "main.go",
            text: valid
        }])
        .supported()
    );
}

#[test]
fn six_negative_fixtures_have_exact_documented_diagnostics_without_execution() {
    let manifest = manifest();
    let fixtures = manifest["negative"].as_array().unwrap();
    assert_eq!(fixtures.len(), 6);
    for fixture in fixtures {
        let file = fixture["file"].as_str().unwrap();
        let source = read(root().join(fixture["path"].as_str().unwrap()).join(file));
        let verdict = check_tree(&[TreeFile {
            path: file,
            text: &source,
        }]);
        assert_eq!(verdict.diagnostics().len(), 1, "{fixture:?}: {verdict:?}");
        let d = &verdict.diagnostics()[0];
        assert_eq!(d.code, fixture["code"].as_str().unwrap());
        assert_eq!(d.path, file);
        assert_eq!(d.line as i64, fixture["line"].as_integer().unwrap());
        assert!(d.column > 0);
    }
}

#[test]
fn build_tags_cgo_preambles_and_tinygo_limits_cannot_hide_in_other_files() {
    for (text, code) in [
        ("//go:build ignored\npackage main", "WASM-UNSUPPORTED-API"),
        (
            "package main\n/* #cgo LDFLAGS: -lnative */",
            "WASM-NATIVE-LIBRARY",
        ),
        (
            "package main\nfunc main() { go work() }",
            "WASM-PROCESS-SPAWN",
        ),
        (
            "package main\nimport alias `net/http`",
            "WASM-UNSUPPORTED-API",
        ),
        (
            concat!("package main\nimport _ \"un", "safe\""),
            "WASM-UNSUPPORTED-API",
        ),
        (
            "package main\nimport \"\\x6f\\x73/exec\"",
            "WASM-UNSUPPORTED-API",
        ),
    ] {
        let verdict = check_tree(&[
            TreeFile {
                path: "main.go",
                text: "package main\nfunc main() {}",
            },
            TreeFile {
                path: "other.go",
                text,
            },
        ]);
        assert!(
            verdict
                .diagnostics()
                .iter()
                .any(|d| d.code == code && d.path == "other.go"),
            "{verdict:?}"
        );
    }
}

fn binding_exports_allocator(source: &str) -> bool {
    source.lines().any(|line| {
        let words: Vec<_> = line.split_whitespace().collect();
        words.len() >= 2
            && ["//go:export", "//export", "//go:wasmexport"].contains(&words[0])
            && words[1] == "cabi_realloc"
    })
}
#[test]
fn bindings_leave_cabi_realloc_to_the_tinygo_runtime() {
    let bindings = root().join("bindings");
    let sources: Vec<_> = fs::read_dir(bindings)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "go"))
        .collect();
    assert!(!sources.is_empty());
    for source in sources {
        assert!(
            !binding_exports_allocator(&read(&source)),
            "duplicate runtime allocator export: {}",
            source.display()
        );
    }
    for pragma in ["//export", "//go:export", "//go:wasmexport"] {
        assert!(binding_exports_allocator(&format!(
            "package bindings\n{pragma} cabi_realloc\nfunc alloc() {{}}"
        )));
    }
}

#[test]
fn positive_examples_are_eligible_and_have_recorded_inputs_and_outputs() {
    let manifest = manifest();
    for workload in manifest["workloads"].as_array().unwrap() {
        let path = workload["source"].as_str().unwrap();
        let source = read(root().join(path));
        assert!(
            check_tree(&[TreeFile {
                path,
                text: &source
            }])
            .supported(),
            "{path}"
        );
        for field in ["input", "expected"] {
            serde_json::from_str::<serde_json::Value>(&read(
                root().join(workload[field].as_str().unwrap()),
            ))
            .unwrap();
        }
    }
}

#[test]
fn trusted_build_script_refuses_to_run_without_its_image_gate() {
    let output = std::process::Command::new("bash")
        .arg(worker().join("qualification/go/build-profile.sh"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("requires the trusted compiler image")
    );
}

#[test]
fn trusted_build_rejects_bad_archive_sha_before_using_tools_or_creating_output() {
    let scratch = std::env::temp_dir().join(format!("go-bad-cache-{}", std::process::id()));
    fs::create_dir(&scratch).unwrap();
    fs::write(
        scratch.join("tinygo0.44.0.linux-amd64.tar.gz"),
        b"not the pinned archive",
    )
    .unwrap();
    let output = std::process::Command::new("bash")
        .arg(worker().join("qualification/go/build-profile.sh"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("FS_TRUSTED_COMPILER_IMAGE", "1")
        .env("FS_TOOLCHAIN_CACHE", &scratch)
        .env("FS_PROFILE_COMPONENTS", scratch.join("output"))
        .env("TMPDIR", &scratch)
        .output()
        .unwrap();
    let created_output = scratch.join("output").exists();
    fs::remove_dir_all(&scratch).unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("toolchain archive sha256 mismatch: tinygo"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!created_output);
}

#[path = "support/profile_go.rs"]
mod built;

#[test]
fn go_component_intake_rejects_tenant_native_cache_and_wasi() {
    // Supply a real Wasmtime native artifact from a trusted synthetic empty component.
    // No fixture is built or instantiated, and no native artifact is deserialized.
    let raw = wat::parse_str("(component)").unwrap();
    let native = wasmtime::Engine::default()
        .precompile_component(&raw)
        .unwrap();
    assert_eq!(
        built::validate_component(&native),
        Err("precompiled_rejected".into())
    );
    let wasi =
        wat::parse_str("(component (import \"wasi:cli/environment@0.2.0\" (instance)))").unwrap();
    assert_eq!(
        built::validate_component(&wasi),
        Err("undeclared_import".into())
    );
}

#[test]
#[ignore = "UNVERIFIED: requires FS_TOOLCHAIN_CACHE and trusted-built Go components"]
fn trusted_components_have_one_allocator_and_only_declared_imports() {
    let Some(components) = built::components() else {
        return;
    };
    let manifest: toml::Value = toml::from_str(built::MANIFEST).unwrap();
    for workload in manifest["workloads"].as_array().unwrap() {
        let name = workload["name"].as_str().unwrap();
        let bytes = fs::read(components.join(format!("{name}.wasm")))
            .expect("missing trusted-built component");
        built::validate_component(&bytes).unwrap();
        println!("{name}: valid component, exactly one core cabi_realloc, allowed imports only");
    }
}

#[test]
fn documentation_matches_manifest_pins_interfaces_diagnostics_and_limits() {
    let m = manifest();
    let doc = read(worker().join("../docs/qualification/profiles/go.md"));
    for key in ["tinygo", "go"] {
        let pin = &m["toolchain"][key];
        assert!(doc.contains(&format!(
            "| {} | `{}` | `{}` | {} |",
            pin["name"].as_str().unwrap(),
            pin["version"].as_str().unwrap(),
            pin["sha256"].as_str().unwrap(),
            pin["source"].as_str().unwrap()
        )));
    }
    for (field, expected) in [
        ("id", "go-wasi-v1"),
        ("status", "disabled"),
        ("qualification", "not-qualified"),
    ] {
        assert_eq!(m["profile"][field].as_str(), Some(expected));
        assert!(doc.contains(&format!("`{expected}`")));
    }
    for field in ["target", "wasi", "wasmtime"] {
        assert!(doc.contains(&format!(
            "| {field} | `{}` |",
            m["toolchain"][field].as_str().unwrap()
        )));
    }
    for interface in m["wit"]["interfaces"].as_array().unwrap() {
        assert!(doc.contains(&format!("- `{}`", interface.as_str().unwrap())));
    }
    for (code, diagnostic) in m["diagnostics"].as_table().unwrap() {
        assert!(doc.contains(&format!(
            "| `{code}` | {} |",
            diagnostic["message"].as_str().unwrap()
        )));
    }
    let bound = m["stdlib"]["toolchain"].as_str().unwrap();
    for package in m["stdlib"]["packages"].as_array().unwrap() {
        assert!(doc.contains(&format!("| `{}` | {bound} |", package.as_str().unwrap())));
    }
    for (limit, description) in m["limits"].as_table().unwrap() {
        assert!(doc.contains(&format!("| {limit} | {} |", description.as_str().unwrap())));
    }
    for fixture in m["negative"].as_array().unwrap() {
        assert!(doc.contains(&format!(
            "| `{}` | `{}` | `{}:{}` |",
            fixture["path"].as_str().unwrap(),
            fixture["code"].as_str().unwrap(),
            fixture["file"].as_str().unwrap(),
            fixture["line"].as_integer().unwrap()
        )));
    }
    assert!(doc.contains("Third-party catalogue: empty"));
    assert!(doc.contains("UNVERIFIED"));
}
