//! Static Go qualification only. Never build or instantiate a guest here.
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

fn assert_pins(pins: toml::Value, manifest: toml::Value) {
    for (name, version, source, archive, sha) in [
        (
            "tinygo",
            "0.44.0",
            "https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz",
            "tinygo0.44.0.linux-amd64.tar.gz",
            "ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9",
        ),
        (
            "go",
            "1.26.11",
            "https://go.dev/dl/go1.26.11.linux-amd64.tar.gz",
            "go1.26.11.linux-amd64.tar.gz",
            "9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec",
        ),
    ] {
        for (field, expected) in [
            ("version", version),
            ("source", source),
            ("archive", archive),
            ("sha256", sha),
        ] {
            assert_eq!(pins[name][field].as_str(), Some(expected), "{name}.{field}");
        }
        assert_eq!(manifest["toolchain"][name], pins[name]["version"]);
        assert_eq!(
            manifest["toolchain"][format!("{name}_sha256")],
            pins[name]["sha256"]
        );
    }
    assert_eq!(pins["tinygo"]["name"].as_str(), Some("TinyGo"));
    assert_eq!(pins["go"]["name"].as_str(), Some("Go"));
    assert_eq!(pins["qualification_status"].as_str(), Some("disabled"));
    let js: toml::Value = toml::from_str(&read(
        worker().join("qualification/javascript/versions.toml"),
    ))
    .unwrap();
    assert_eq!(pins["wasi_virt"], js["wasi_virt"]);
    assert_eq!(
        manifest["toolchain"]["wasi_virt_rev"],
        pins["wasi_virt"]["rev"]
    );
    assert_eq!(manifest["toolchain"]["wasi"].as_str(), Some("none"));
    assert_eq!(manifest["toolchain"]["target"].as_str(), Some("wasip2"));
    assert_eq!(manifest["toolchain"]["wasmtime"].as_str(), Some("49.0.5"));
    let matrix_bytes = read(worker().join("../docs/contracts/language-profile-matrix.json"));
    assert_eq!(
        matrix_bytes,
        read(worker().join("src/adapters/language-profile-matrix.json"))
    );
    let matrix: serde_json::Value = serde_json::from_str(&matrix_bytes).unwrap();
    let go = &matrix["profiles"]["go"];
    for (field, value) in [
        ("toolchainVersion", "0.44.0"),
        ("wasmtimeVersion", "49.0.5"),
        ("wasiVersion", "none"),
        ("status", "disabled"),
        ("id", "go-wasi-v1"),
    ] {
        assert_eq!(go[field], value);
    }
    assert_eq!(matrix["matrixVersion"], "2");
    assert_eq!(go["evidence"], serde_json::json!({}));
    for (field, value) in [
        ("id", "go-wasi-v1"),
        ("status", "disabled"),
        ("qualification", "not-qualified"),
        ("mode", "wasm"),
        ("matrix_revision", "2"),
    ] {
        assert_eq!(manifest["profile"][field].as_str(), Some(value));
    }
    assert_eq!(
        manifest["wit"]["package"].as_str(),
        Some("falcone:host@0.2.0")
    );
    assert_eq!(manifest["wit"]["world"].as_str(), Some("function"));
    assert_eq!(
        manifest["catalogue"]["packages"].as_array().unwrap().len(),
        0
    );
    assert_eq!(
        manifest["catalogue"]["revision"].as_str(),
        Some("unqualified")
    );
}

fn tree(directory: &std::path::Path, prefix: &str, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = format!("{prefix}{}", entry.file_name().to_str().unwrap());
        if entry.file_type().unwrap().is_dir() {
            tree(&entry.path(), &format!("{name}/"), out);
        } else {
            let bytes = fs::read(entry.path()).unwrap();
            assert!(
                !bytes.starts_with(b"\0asm")
                    && !bytes.starts_with(b"\x7fELF")
                    && !bytes.starts_with(b"MZ")
            );
            assert!(
                matches!(
                    entry.path().extension().and_then(|v| v.to_str()),
                    Some("go" | "mod" | "json" | "toml")
                ),
                "source-only fixtures: {name}"
            );
            out.push((name, String::from_utf8(bytes).unwrap()));
        }
    }
}

#[test]
fn positive_sources_are_eligible_and_each_negative_has_exactly_its_documented_diagnostic() {
    use falcone_serverless_worker::adapters::compat_go::{File, check_tree, standard_library};
    let m = manifest();
    let mut all_sources = Vec::new();
    tree(&root(), "", &mut all_sources);
    let interfaces: Vec<_> = m["wit"]["interfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for (field, pin) in [("go", "1.26.11"), ("tinygo", "0.44.0")] {
        assert_eq!(m["stdlib"][field].as_str(), Some(pin));
    }
    let stdlib: Vec<_> = m["stdlib"]["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        stdlib,
        standard_library()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    let check = |files: &[(String, String)]| {
        let files: Vec<_> = files
            .iter()
            .map(|(name, text)| File { name, text })
            .collect();
        check_tree(&files, &interfaces)
    };
    for workload in m["workloads"].as_array().unwrap() {
        let mut files = Vec::new();
        tree(
            &root().join(workload["source"].as_str().unwrap()),
            "",
            &mut files,
        );
        for binding in workload["bindings"].as_array().unwrap() {
            let name = binding.as_str().unwrap();
            files.push((
                format!("bindings/{name}"),
                read(root().join("bindings").join(name)),
            ));
        }
        assert!(files.iter().any(|(name, _)| name == "go.mod"));
        assert!(
            check(&files).is_empty(),
            "{:?}: {:?}",
            workload["name"],
            check(&files)
        );
        for field in ["input", "expected"] {
            serde_json::from_str::<serde_json::Value>(&read(
                root().join(workload[field].as_str().unwrap()),
            ))
            .unwrap();
        }
    }
    let codes = [
        "WASM-NATIVE-LIBRARY",
        "WASM-GPU",
        "WASM-PROCESS-SPAWN",
        "WASM-INSTALL-HOOK",
        "WASM-PACKAGE-NOT-IN-CATALOGUE",
        "WASM-UNSUPPORTED-API",
    ];
    assert_eq!(m["diagnostics"].as_table().unwrap().len(), codes.len());
    for code in codes {
        assert!(m["diagnostics"].as_table().unwrap().contains_key(code));
    }
    assert_eq!(m["negative"].as_array().unwrap().len(), codes.len());
    for (fixture, code) in m["negative"].as_array().unwrap().iter().zip(codes) {
        let mut files = Vec::new();
        tree(
            &root().join(fixture["path"].as_str().unwrap()),
            "",
            &mut files,
        );
        let report = check(&files);
        assert_eq!(report.len(), 1, "{report:?}");
        assert_eq!(report[0].code, code);
        assert_eq!(Some(report[0].code), fixture["code"].as_str());
        assert_eq!(files[report[0].source].0, fixture["file"].as_str().unwrap());
        assert_eq!(report[0].file, fixture["file"].as_str().unwrap());
        assert_eq!(report[0].line as i64, fixture["line"].as_integer().unwrap());
        assert!(!format!("{report:?}").contains("tenant-private"));
    }
}

#[test]
fn storage_replay_matches_the_published_input_and_expected_output() {
    let directory = root().join("examples/storage-events");
    let cases: serde_json::Value =
        serde_json::from_str(&read(directory.join("cases.json"))).unwrap();
    let mut input: serde_json::Value =
        serde_json::from_str(&read(directory.join("input.json"))).unwrap();
    input["__ow_path"] = serde_json::json!("/v1/workspaces/ws-synthetic-0001/openapi");
    let expected: serde_json::Value =
        serde_json::from_str(&read(directory.join("expected.json"))).unwrap();
    assert_eq!(cases["cases"][0]["params"], input);
    assert_eq!(cases["cases"][0]["response"], expected);
}

fn assert_interfaces(m: toml::Value) {
    use falcone_serverless_worker::artifact::validate::ALLOWED_IMPORTS;
    use std::collections::BTreeSet;
    let interfaces: BTreeSet<_> = m["wit"]["interfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let mut resolve = wit_parser::Resolve::default();
    let (package, _) = resolve.push_path(worker().join("wit/host.wit")).unwrap();
    let world = &resolve.worlds[resolve.packages[package].worlds["function"]];
    let wit: BTreeSet<_> = world
        .imports
        .values()
        .map(|item| {
            let wit_parser::WorldItem::Interface { id, .. } = item else {
                panic!("interface")
            };
            format!(
                "falcone:host/{}@0.2.0",
                resolve.interfaces[*id].name.as_ref().unwrap()
            )
        })
        .collect();
    assert_eq!(interfaces, ALLOWED_IMPORTS.into_iter().collect());
    assert_eq!(
        interfaces.iter().copied().collect::<BTreeSet<_>>(),
        wit.iter().map(String::as_str).collect()
    );
    let matrix: serde_json::Value = serde_json::from_str(&read(
        worker().join("src/adapters/language-profile-matrix.json"),
    ))
    .unwrap();
    let go = &matrix["profiles"]["go"];
    assert_eq!(
        go["allowedInterfaces"],
        serde_json::to_value(&m["wit"]["interfaces"]).unwrap()
    );
    let mut names = Vec::new();
    for workload in m["workloads"].as_array().unwrap() {
        names.push(workload["name"].as_str().unwrap());
        let mut sources = Vec::new();
        tree(
            &root().join(workload["source"].as_str().unwrap()),
            "",
            &mut sources,
        );
        for binding in workload["bindings"].as_array().unwrap() {
            sources.push((
                binding.as_str().unwrap().to_owned(),
                read(root().join("bindings").join(binding.as_str().unwrap())),
            ));
        }
        let actual: BTreeSet<_> = sources
            .iter()
            .flat_map(|(_, text)| text.lines())
            .filter_map(|line| line.trim().strip_prefix("//go:wasmimport "))
            .map(|rest| rest.split_whitespace().next().unwrap())
            .collect();
        let declared: BTreeSet<_> = workload["imports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.as_str().unwrap())
            .collect();
        assert_eq!(actual, declared);
        assert!(actual.is_subset(&interfaces));
    }
    assert_eq!(go["testedExamples"], serde_json::json!(names));
    let codes: BTreeSet<_> = m["diagnostics"]
        .as_table()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        codes,
        go["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect()
    );
}

#[test]
fn serialized_wasmtime_and_native_inputs_are_rejected_without_deserialization() {
    let serialized = wasmtime::Engine::default()
        .precompile_component(b"(component)")
        .unwrap();
    for input in [
        serialized.as_slice(),
        b"\x7fELFsynthetic native bytes",
        b"MZsynthetic native bytes",
    ] {
        assert_eq!(
            falcone_serverless_worker::artifact::validate::validate(input, input.len())
                .unwrap_err()
                .code,
            "precompiled_rejected"
        );
    }
}

#[test]
fn trusted_build_is_opt_in_offline_and_only_builds_the_positive_examples() {
    let script = read(worker().join("qualification/go/build-fixtures.sh"));
    for marker in [
        "FS_TRUSTED_COMPILER_IMAGE",
        "FS_TOOLCHAIN_CACHE",
        "sha256",
        "GOPROXY=off",
        "GOSUMDB=off",
        "GOTOOLCHAIN=local",
        "--deny-all",
        "-target=wasip2",
        "-wit-world",
        "--test profile_go",
        "--test profile_go_runtime",
        "--test compat_go_static",
    ] {
        assert!(script.contains(marker), "missing build gate: {marker}");
    }
    assert!(script.find("--test profile_go\n").unwrap() < script.find("-target=wasip2").unwrap());
    for workload in manifest()["workloads"].as_array().unwrap() {
        assert!(script.contains(workload["name"].as_str().unwrap()));
    }
    assert!(!script.contains("negative/"));
    assert!(!script.contains("go get") && !script.contains("curl "));
    let runner = read(worker().join("tests/profile_go_runtime.rs"));
    assert!(runner.contains("validate::validate") && runner.contains("compile_declared"));
    assert!(runner.contains("UNVERIFIED") && runner.contains("#[ignore"));
}

fn documented_contract(m: &toml::Value, pins: &toml::Value) -> String {
    let mut doc = String::from(
        "# Go Wasm profile\n\nUNVERIFIED: source review does not qualify this profile. It remains disabled and not-qualified until the operator records successful restricted-cluster evidence with an artifact ID and sha256 on #53. No runtime adapter or admission path uses the static checker.\n\n",
    );
    for section in [
        "profile",
        "toolchain",
        "wit",
        "catalogue",
        "stdlib",
        "limits",
        "diagnostics",
    ] {
        doc.push_str(&format!(
            "## {section}\n\n| Field | Value |\n| --- | --- |\n"
        ));
        for (field, value) in m[section].as_table().unwrap() {
            doc.push_str(&format!(
                "| {field} | `{}` |\n",
                serde_json::to_string(value).unwrap()
            ));
        }
        doc.push('\n');
    }
    doc.push_str("## Operator archive pins\n\n");
    for section in ["tinygo", "go", "wasi_virt"] {
        doc.push_str(&format!("### {section}\n\n"));
        for (field, value) in pins[section].as_table().unwrap() {
            doc.push_str(&format!("- {field}: `{}`\n", value.as_str().unwrap()));
        }
        doc.push('\n');
    }
    doc.push_str("## Examples\n\n");
    for workload in m["workloads"].as_array().unwrap() {
        doc.push_str(&format!(
            "- `{}`: `{}`, expected `{}`; imports `{}`.\n",
            workload["name"].as_str().unwrap(),
            workload["input"].as_str().unwrap(),
            workload["expected"].as_str().unwrap(),
            serde_json::to_string(&workload["imports"]).unwrap()
        ));
    }
    doc.push_str(include_str!("fixtures/profile-go-notes.md"));
    doc
}

#[test]
fn documentation_matches_every_manifest_value_and_rejects_drift() {
    let m = manifest();
    let pins: toml::Value =
        toml::from_str(&read(worker().join("qualification/go/versions.toml"))).unwrap();
    let published = read(worker().join("../docs/qualification/profiles/go.md"));
    assert_document(&published, &m, &pins);
}

fn versions() -> toml::Value {
    toml::from_str(&read(worker().join("qualification/go/versions.toml"))).unwrap()
}

fn assert_document(published: &str, m: &toml::Value, pins: &toml::Value) {
    assert_eq!(
        published,
        documented_contract(m, pins),
        "Go documentation drift"
    );
}

#[test]
fn pins_match_the_operator_table_and_matrix() {
    assert_pins(versions(), manifest());
}

#[test]
fn interfaces_are_real_allowed_and_match_each_examples_imports() {
    assert_interfaces(manifest());
}

#[test]
fn missing_floating_and_substituted_pins_fail_the_contract_check() {
    for field in [
        "tinygo",
        "go",
        "wasmtime",
        "wasi",
        "target",
        "wasi_virt_rev",
    ] {
        let mut changed = manifest();
        changed["toolchain"].as_table_mut().unwrap().remove(field);
        assert!(
            std::panic::catch_unwind(|| assert_pins(versions(), changed)).is_err(),
            "missing {field}"
        );
        for invalid in ["", "latest", "^1.26.11", ">=0.44.0", "1.x"] {
            let mut changed = manifest();
            changed["toolchain"][field] = toml::Value::String(invalid.into());
            assert!(
                std::panic::catch_unwind(|| assert_pins(versions(), changed)).is_err(),
                "floating {field}"
            );
        }
    }
    // Consistent edits to both copies cannot substitute another operator toolchain.
    let mut pins = versions();
    let mut m = manifest();
    pins["tinygo"]["version"] = toml::Value::String("0.45.0".into());
    m["toolchain"]["tinygo"] = toml::Value::String("0.45.0".into());
    assert!(std::panic::catch_unwind(|| assert_pins(pins, m)).is_err());
    let mut pins = versions();
    pins["go"]["sha256"] = toml::Value::String("a".repeat(64));
    assert!(std::panic::catch_unwind(|| assert_pins(pins, manifest())).is_err());
}

#[test]
fn an_extra_interface_and_document_drift_fail_the_same_contract_checks() {
    let mut m = manifest();
    m["wit"]["interfaces"]
        .as_array_mut()
        .unwrap()
        .push(toml::Value::String("falcone:host/unlisted@0.2.0".into()));
    assert!(std::panic::catch_unwind(|| assert_interfaces(m)).is_err());
    let published = read(worker().join("../docs/qualification/profiles/go.md"));
    for token in [
        "0.44.0",
        "1.26.11",
        "falcone:host/storage@0.2.0",
        "1048576",
        "WASM-GPU",
    ] {
        let changed = published.replacen(token, "DRIFT", 1);
        assert!(
            std::panic::catch_unwind(|| assert_document(&changed, &manifest(), &versions()))
                .is_err()
        );
    }
}
