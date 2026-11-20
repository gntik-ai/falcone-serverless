//! Static profile qualification; never builds a guest toolchain or executes a guest.
use std::{fs, path::PathBuf};

fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn manifest() -> toml::Value {
    toml::from_str(
        &fs::read_to_string(worker().join("qualification/profiles/go/manifest.toml")).unwrap(),
    )
    .unwrap()
}

#[test]
fn go_profile_records_the_exact_operator_toolchains() {
    let manifest = manifest();
    let versions: toml::Value = toml::from_str(
        &fs::read_to_string(worker().join("qualification/go/versions.toml")).unwrap(),
    )
    .unwrap();
    for (name, version, source, sha) in [
        (
            "tinygo",
            "0.44.0",
            "https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz",
            "ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9",
        ),
        (
            "go",
            "1.26.11",
            "https://go.dev/dl/go1.26.11.linux-amd64.tar.gz",
            "9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec",
        ),
    ] {
        for (field, expected) in [("version", version), ("source", source), ("sha256", sha)] {
            assert_eq!(
                versions[name][field].as_str(),
                Some(expected),
                "{name}/{field}"
            );
            assert_eq!(
                manifest["toolchains"][name][field].as_str(),
                Some(expected),
                "manifest {name}/{field}"
            );
        }
    }
}

use falcone_serverless_worker::adapters::{compat::TreeFile, compat_go};
const MANIFEST_PATH: &str = "qualification/profiles/go/manifest.toml";
fn report(text: &str) -> Vec<falcone_serverless_worker::adapters::compat::TreeDiagnostic> {
    compat_go::check_tree(
        &[TreeFile {
            path: "main.go",
            text,
        }],
        &fs::read_to_string(worker().join(MANIFEST_PATH)).unwrap(),
    )
}

#[test]
fn directives_are_checked_independently_of_import_traversal() {
    for (directive, code) in [
        ("//go:generate forbidden secret-value", "WASM-INSTALL-HOOK"),
        ("//go:linkname symbol secret-value", "WASM-UNSUPPORTED-API"),
    ] {
        for (source, line) in [
            (
                format!("package main\nimport (\n{directive}\n\"encoding/json\"\n)\n"),
                3,
            ),
            (
                format!(
                    "package main\nimport \"encoding/json\"\n{directive}\nimport \"strings\"\n"
                ),
                3,
            ),
            (format!("{directive}\npackage main\n"), 1),
        ] {
            let diagnostics = report(&source);
            assert!(
                diagnostics.iter().any(|d| d.code == code
                    && d.path == "main.go"
                    && d.line == line
                    && d.column == 1),
                "{diagnostics:?}"
            );
            assert!(!format!("{diagnostics:?}").contains("secret-value"));
        }
    }
    assert!(report("package main\nvar s = `//go:generate not-a-comment`\n").is_empty());
}

#[test]
fn goroutines_are_rejected_without_reporting_comment_or_string_contents() {
    let diagnostics = report("package main\nfunc main() {\n go func() {}()\n}\n");
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.code, "WASM-PROCESS-SPAWN");
    assert_eq!(diagnostic.construct, "goroutine");
    assert_eq!(diagnostic.path, "main.go");
    assert_eq!((diagnostic.line, diagnostic.column), (3, 2));
    assert!(report("package main\n// go secret-value\nvar s = `go secret-value`\n").is_empty());
}

#[test]
fn missing_floating_module_and_unbound_stdlib_pins_are_rejected() {
    let good = manifest();
    for path in [
        vec!["toolchains", "go", "version"],
        vec!["toolchains", "tinygo", "version"],
        vec!["runtime", "wasmtime"],
        vec!["runtime", "wasi"],
        vec!["runtime", "target"],
        vec!["wit", "package"],
        vec!["wit", "world"],
    ] {
        for value in [None, Some("latest"), Some("^1.26.11"), Some("1.26")] {
            let mut bad = good.clone();
            let mut table = &mut bad;
            for part in &path[..path.len() - 1] {
                table = &mut table[*part];
            }
            let field = path[path.len() - 1];
            if let Some(value) = value {
                table[field] = toml::Value::String(value.into());
            } else {
                table.as_table_mut().unwrap().remove(field);
            }
            assert!(
                !compat_go::check_tree(&[], &render(&bad)).is_empty(),
                "{path:?} {value:?}"
            );
        }
    }
    for version in ["", "latest", "^v1.2.3", "v1.2"] {
        let mut bad = good.clone();
        bad["catalogue"]["modules"] =
            toml::Value::Array(vec![toml::Value::Table(toml::map::Map::from_iter([
                (
                    "path".into(),
                    toml::Value::String("example.invalid/module".into()),
                ),
                ("version".into(), toml::Value::String(version.into())),
            ]))]);
        assert!(!compat_go::check_tree(&[], &render(&bad)).is_empty());
    }
    let mut bad = good;
    bad["stdlib"]["encoding/json"] = toml::Value::String("latest".into());
    assert!(!compat_go::check_tree(&[], &render(&bad)).is_empty());
}

// Small TOML writer for mutation tests, independent of the production validator.
fn render(value: &toml::Value) -> String {
    fn scalar(value: &toml::Value) -> String {
        match value {
            toml::Value::String(s) => serde_json::to_string(s).unwrap(),
            toml::Value::Array(a) => {
                format!("[{}]", a.iter().map(scalar).collect::<Vec<_>>().join(","))
            }
            toml::Value::Table(t) => format!(
                "{{{}}}",
                t.iter()
                    .map(|(k, v)| format!("{} = {}", serde_json::to_string(k).unwrap(), scalar(v)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            _ => panic!("unsupported mutation value"),
        }
    }
    fn tables(value: &toml::Value, path: &str, out: &mut String) {
        if !path.is_empty() {
            out.push_str(&format!("[{path}]\n"));
        }
        for (k, v) in value.as_table().unwrap() {
            if !v.is_table() {
                out.push_str(&format!(
                    "{} = {}\n",
                    serde_json::to_string(k).unwrap(),
                    scalar(v)
                ));
            }
        }
        for (k, v) in value.as_table().unwrap() {
            if v.is_table() {
                tables(
                    v,
                    &if path.is_empty() {
                        k.to_owned()
                    } else {
                        format!("{path}.{k}")
                    },
                    out,
                );
            }
        }
    }
    let mut out = String::new();
    tables(value, "", &mut out);
    out
}

#[test]
fn import_and_api_diagnostics_name_the_construct_and_location_without_source_data() {
    for (construct, code) in [
        ("C", "WASM-NATIVE-LIBRARY"),
        ("os/exec", "WASM-PROCESS-SPAWN"),
        ("example.invalid/gpu", "WASM-GPU"),
        (
            "example.invalid/secret-value",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
        ),
        ("reflect", "WASM-UNSUPPORTED-API"),
        ("syscall", "WASM-UNSUPPORTED-API"),
        ("net/http", "WASM-UNSUPPORTED-API"),
    ] {
        for import in [
            format!("import \"{construct}\""),
            format!("import (\n _ `{construct}`\n)"),
        ] {
            let source = format!("package main\n{import}\n");
            let diagnostics = report(&source);
            assert!(
                diagnostics.iter().any(|d| d.code == code
                    && d.path == "main.go"
                    && d.line >= 2
                    && d.column > 0
                    && !d.construct.is_empty()),
                "{code}: {diagnostics:?}"
            );
            assert!(!format!("{diagnostics:?}").contains("secret-value"));
        }
    }
    for source in [
        "package main\nimport \"\\x43\"\n", // escaped cgo import
        "package main\n//go:wasmimport wasi:cli/environment@0.2.0 get-environment\nfunc f()\n",
        "package main\n//go:wasmimport falcone:host/unlisted@0.2.0 f\nfunc f()\n",
    ] {
        assert!(!report(source).is_empty(), "{source}");
    }
    assert!(report("package main\nimport (\n\"encoding/json\"\n\"strings\"\n)\n").is_empty());
}

fn contract(manifest: &toml::Value) -> serde_json::Value {
    serde_json::json!({
        "id": manifest["profile"]["id"], "status": manifest["profile"]["status"],
        "runtime": manifest["runtime"], "toolchains": manifest["toolchains"],
        "wit": manifest["wit"], "stdlib": manifest["stdlib"], "catalogue": manifest["catalogue"],
        "testedExamples": manifest["workloads"].as_array().unwrap().iter().map(|w| w["name"].as_str().unwrap()).collect::<Vec<_>>(),
        "diagnostics": manifest["diagnostics"],
    })
}

#[test]
fn trusted_build_world_imports_exactly_the_manifest_interfaces() {
    use std::collections::BTreeSet;
    let manifest = manifest();
    let mut resolve = wit_parser::Resolve::default();
    let (package, _) = resolve
        .push_dir(worker().join("qualification/profiles/go/wit"))
        .expect("resolve the trusted Go build's WIT package and vendored dependencies");
    let world = &resolve.worlds
        [resolve.packages[package].worlds[manifest["wit"]["world"].as_str().unwrap()]];
    let interface_name = |item: &wit_parser::WorldItem| match item {
        wit_parser::WorldItem::Interface { id, .. } => {
            let interface = &resolve.interfaces[*id];
            resolve.packages[interface.package.unwrap()]
                .name
                .interface_id(interface.name.as_deref().unwrap())
        }
        _ => panic!("the profile world permits only interface imports and exports"),
    };
    let imports: BTreeSet<_> = world.imports.values().map(interface_name).collect();
    let declared: BTreeSet<_> = manifest["wit"]["interfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(imports, declared);
    assert_eq!(
        world
            .exports
            .values()
            .map(interface_name)
            .collect::<Vec<_>>(),
        ["falcone:host/handler@0.2.0"]
    );
    assert_eq!(
        fs::read(worker().join("qualification/profiles/go/wit/deps/falcone-host/host.wit"))
            .unwrap(),
        fs::read(worker().join("wit/host.wit")).unwrap(),
        "vendored Falcone WIT must track the canonical contract"
    );
    let random = resolve
        .interfaces
        .iter()
        .find_map(|(_, interface)| {
            (interface.name.as_deref() == Some("random")).then_some(interface)
        })
        .unwrap();
    assert_eq!(
        random
            .functions
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["get-random-bytes", "get-random-u64"])
    );
}

#[test]
fn matrix_and_documentation_match_the_manifest_and_exact_host_interface_set() {
    use std::collections::BTreeSet;
    let manifest = manifest();
    assert!(
        compat_go::check_tree(
            &[],
            &fs::read_to_string(worker().join(MANIFEST_PATH)).unwrap()
        )
        .is_empty()
    );
    let matrix: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(worker().join("src/adapters/language-profile-matrix.json")).unwrap(),
    )
    .unwrap();
    let go = &matrix["profiles"]["go"];
    assert_eq!(go["toolchainVersion"], "0.44.0");
    assert_eq!(go["wasiVersion"], "0.2.0");
    assert_eq!(go["wasmtimeVersion"], "49.0.5");
    assert_eq!(
        go["toolchains"],
        serde_json::to_value(&manifest["toolchains"]).unwrap()
    );
    assert_eq!(go["status"], "disabled");
    assert_eq!(go["evidence"], serde_json::json!({}));
    assert_eq!(go["testedExamples"], contract(&manifest)["testedExamples"]);
    assert_eq!(
        fs::read(worker().join("src/adapters/language-profile-matrix.json")).unwrap(),
        fs::read(worker().join("../docs/contracts/language-profile-matrix.json")).unwrap()
    );
    let mut resolve = wit_parser::Resolve::default();
    let (pkg, _) = resolve.push_path(worker().join("wit/host.wit")).unwrap();
    let pkg = &resolve.packages[pkg];
    let mut allowed: BTreeSet<_> = resolve.worlds[pkg.worlds["function"]]
        .imports
        .values()
        .filter_map(|item| match item {
            wit_parser::WorldItem::Interface { id, .. } => resolve.interfaces[*id]
                .name
                .as_deref()
                .map(|n| pkg.name.interface_id(n)),
            _ => None,
        })
        .collect();
    assert_eq!(allowed.len(), 8);
    allowed.insert("wasi:random/random@0.2.0".into());
    let declared: BTreeSet<_> = manifest["wit"]["interfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(declared, allowed);
    assert_eq!(
        go["allowedInterfaces"],
        serde_json::to_value(&manifest["wit"]["interfaces"]).unwrap()
    );
    let codes: BTreeSet<_> = go["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        manifest["diagnostics"]
            .as_table()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    );
    let docs = fs::read_to_string(worker().join("../docs/qualification/profiles/go.md")).unwrap();
    let recorded: serde_json::Value = serde_json::from_str(
        docs.split_once("```json\n")
            .unwrap()
            .1
            .split_once("\n```")
            .unwrap()
            .0,
    )
    .unwrap();
    assert_eq!(recorded, contract(&manifest), "go.md contract drift");
    let mut drift = recorded.clone();
    drift["runtime"]["wasi"] = "none".into();
    assert_ne!(drift, contract(&manifest));
}

#[test]
fn committed_examples_are_eligible_and_each_negative_fixture_is_rejected() {
    let m = manifest();
    let text = fs::read_to_string(worker().join(MANIFEST_PATH)).unwrap();
    for workload in m["workloads"].as_array().unwrap() {
        let path = workload["source"].as_str().unwrap();
        let source =
            fs::read_to_string(worker().join("qualification/profiles/go").join(path)).unwrap();
        assert!(
            compat_go::check_tree(
                &[TreeFile {
                    path,
                    text: &source
                }],
                &text
            )
            .is_empty()
        );
        for import in workload["imports"].as_array().unwrap() {
            assert!(m["wit"]["interfaces"].as_array().unwrap().contains(import));
        }
        // A real example with an undeclared host interface must fail eligibility.
        let mutated = format!("//go:wasmimport falcone:host/unlisted@0.2.0 f\n{source}");
        assert!(
            !compat_go::check_tree(
                &[TreeFile {
                    path,
                    text: &mutated
                }],
                &text
            )
            .is_empty()
        );
    }
    for (name, code) in [
        ("native-library", "WASM-NATIVE-LIBRARY"),
        ("process-spawn", "WASM-PROCESS-SPAWN"),
        ("install-hook", "WASM-INSTALL-HOOK"),
        ("gpu", "WASM-GPU"),
        ("package-not-in-catalogue", "WASM-PACKAGE-NOT-IN-CATALOGUE"),
        ("unsupported-api", "WASM-UNSUPPORTED-API"),
    ] {
        let path = format!("negative/{name}/main.go");
        let source =
            fs::read_to_string(worker().join("qualification/profiles/go").join(&path)).unwrap();
        let diagnostics = compat_go::check_tree(
            &[TreeFile {
                path: &path,
                text: &source,
            }],
            &text,
        );
        assert!(
            diagnostics.iter().any(|d| d.code == code),
            "{name}: {diagnostics:?}"
        );
    }
}

#[test]
fn trusted_builder_rejects_an_archive_hash_mismatch_before_running_tools() {
    let directory = std::env::temp_dir().join(format!("go-build-preflight-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    fs::write(
        directory.join("tinygo0.44.0.linux-amd64.tar.gz"),
        b"wrong archive",
    )
    .unwrap();
    let output = std::process::Command::new("bash")
        .arg(worker().join("qualification/go/build-profile.sh"))
        .env("FS_TRUSTED_COMPILER_IMAGE", "1")
        .env("FS_TOOLCHAIN_CACHE", &directory)
        .env("FS_PROFILE_COMPONENTS", directory.join("output"))
        .env_remove("TMPDIR")
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("toolchain archive sha256 mismatch: tinygo")
    );
}
