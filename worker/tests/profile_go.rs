use serde_json::Value;
use std::{fs, path::PathBuf};
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn manifest() -> toml::Value {
    toml::from_str(
        &fs::read_to_string(root().join("qualification/profiles/go/manifest.toml")).unwrap(),
    )
    .unwrap()
}
const PINS: [(&str, &str, &str, &str); 3] = [
    (
        "tinygo",
        "0.44.0",
        "ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9",
        "https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz",
    ),
    (
        "go",
        "1.26.11",
        "9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec",
        "https://go.dev/dl/go1.26.11.linux-amd64.tar.gz",
    ),
    (
        "wasm_tools",
        "1.258.3",
        "99b486a9ddc19af0260cd5f98f3b709a60817a3acd20efc9c582d719caebbf6d",
        "https://github.com/bytecodealliance/wasm-tools/releases/download/v1.258.3/wasm-tools-1.258.3-x86_64-linux.tar.gz",
    ),
];
fn pins_valid(manifest: &toml::Value, pins: &toml::Value, matrix: &Value) -> bool {
    PINS.iter().all(|(key, version, sha, source)| {
        [
            manifest
                .get("pins")
                .and_then(|p| p.get(key))
                .unwrap_or(&toml::Value::Boolean(false)),
            pins.get(key).unwrap_or(&toml::Value::Boolean(false)),
        ]
        .iter()
        .all(|p| {
            p.get("version").and_then(toml::Value::as_str) == Some(*version)
                && p.get("sha256").and_then(toml::Value::as_str) == Some(*sha)
                && p.get("source").and_then(toml::Value::as_str) == Some(*source)
        }) && matrix["toolchainPins"][key]["version"] == *version
            && matrix["toolchainPins"][key]["sha256"] == *sha
            && matrix["toolchainPins"][key]["source"].as_str() == pins[key]["source"].as_str()
    }) && manifest["stdlib"]["toolchain"].as_str() == Some("1.26.11")
        && manifest["modules"].as_array().is_some_and(|modules| {
            modules.iter().all(|m| {
                m.get("version")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|v| {
                        let v = v.strip_prefix('v').unwrap_or(v);
                        v.split('.').count() == 3
                            && v.split('.')
                                .all(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
                    })
            })
        })
}
#[test]
fn exact_operator_pins_and_disabled_state() {
    let m = manifest();
    let pins: toml::Value = toml::from_str(
        &fs::read_to_string(root().join("src/qualification/go_versions.toml")).unwrap(),
    )
    .unwrap();
    let matrix: Value = serde_json::from_str(
        &fs::read_to_string(root().join("../docs/contracts/language-profile-matrix.json")).unwrap(),
    )
    .unwrap();
    let go = &matrix["profiles"]["go"];
    assert!(pins_valid(&m, &pins, go));
    assert_eq!(m["pins"], pins);
    assert_eq!(go["toolchainPins"], serde_json::to_value(&pins).unwrap());
    assert_eq!(m["toolchain"]["target"].as_str(), Some("wasip2"));
    assert_eq!(m["toolchain"]["wasi"].as_str(), Some("0.2.0"));
    assert_eq!(m["wit"]["package"].as_str(), Some("falcone:host@0.2.0"));
    assert_eq!(m["wit"]["world"].as_str(), Some("function"));
    assert_eq!(go["toolchainVersion"], "0.44.0");
    assert_eq!(go["wasiVersion"], "0.2.0");
    assert_eq!(
        fs::read(root().join("qualification/go/versions.toml")).unwrap(),
        fs::read(root().join("src/qualification/go_versions.toml")).unwrap()
    );
    for bad in ["", "latest", "1.26.10"] {
        let mut altered = m.clone();
        altered["stdlib"]["toolchain"] = bad.into();
        assert!(!pins_valid(&altered, &pins, go));
    }
    for bad in ["", "latest", ">=1.2.3"] {
        let mut altered = m.clone();
        let mut module = toml::Table::new();
        module.insert("name".into(), "example.invalid/module".into());
        module.insert("version".into(), bad.into());
        altered["modules"] = toml::Value::Array(vec![toml::Value::Table(module)]);
        assert!(!pins_valid(&altered, &pins, go));
    }

    assert_eq!(m["profile"]["id"].as_str(), Some("go-wasi-v1"));
    assert_eq!(m["profile"]["status"].as_str(), Some("disabled"));
    assert_eq!(
        m["profile"]["qualification"].as_str(),
        Some("not-qualified")
    );
    assert_eq!(go["status"], "disabled");
    assert_eq!(go["evidence"], serde_json::json!({}));
    for (key, _, _, _) in PINS {
        for bad in ["", "latest", ">=1.0.0", "0.0.0"] {
            let mut altered = m.clone();
            altered["pins"][key]["version"] = bad.into();
            assert!(!pins_valid(&altered, &pins, go));
            let mut altered = pins.clone();
            altered[key]["version"] = bad.into();
            assert!(!pins_valid(&m, &altered, go));
            let mut altered = go.clone();
            altered["toolchainPins"][key]["version"] = bad.into();
            assert!(!pins_valid(&m, &pins, &altered));
        }
        let mut altered = m.clone();
        altered["pins"].as_table_mut().unwrap().remove(key);
        assert!(!pins_valid(&altered, &pins, go));
    }
}

#[test]
fn directives_and_every_reflect_alias_have_source_locations() {
    use falcone_serverless_worker::adapters::compat_go::{TreeFile, check_tree};
    for (source, code, line) in [
        (
            "package main\nimport (\n//go:generate forbidden\n\"encoding/json\"\n)\n",
            "WASM-INSTALL-HOOK",
            3,
        ),
        (
            "package main\nimport \"encoding/json\"\n//go:generate forbidden\nimport \"regexp\"\n",
            "WASM-INSTALL-HOOK",
            3,
        ),
        (
            "package main\n//go:generate forbidden\n",
            "WASM-INSTALL-HOOK",
            2,
        ),
        (
            "//go:linkname hidden runtime.hidden\npackage main\n",
            "WASM-UNSUPPORTED-API",
            1,
        ),
        (
            "package main\nimport (\n//go:linkname hidden runtime.hidden\n\"reflect\"\n)\n",
            "WASM-UNSUPPORTED-API",
            3,
        ),
        (
            "package main\nimport (r \"reflect\"; _ \"reflect\")\nvar _ = r.MakeFunc\n",
            "WASM-UNSUPPORTED-API",
            3,
        ),
        (
            "package main\nimport (_ \"reflect\"; r \"reflect\")\nvar _ = r.MakeFunc\n",
            "WASM-UNSUPPORTED-API",
            3,
        ),
        (
            "package main\nimport (r \"reflect\"; . \"reflect\"; _ \"reflect\")\nvar _ = r.MakeFunc\n",
            "WASM-UNSUPPORTED-API",
            3,
        ),
        (
            "package main\nimport (. \"reflect\"; r \"reflect\")\nvar _ = MakeFunc\n",
            "WASM-UNSUPPORTED-API",
            3,
        ),
    ] {
        let found = check_tree(
            &[TreeFile {
                path: "main.go",
                text: source,
            }],
            None,
        );
        assert!(
            found
                .diagnostics()
                .iter()
                .any(|d| d.code == code && d.path == "main.go" && d.line == line),
            "{found:?}"
        );
    }
    let harmless =
        "package main\n// ordinary comment about //go:generate\nvar s = `//go:linkname`\n";
    assert!(
        check_tree(
            &[TreeFile {
                path: "main.go",
                text: harmless
            }],
            None
        )
        .supported()
    );
}

#[test]
fn unicode_reflect_aliases_cannot_bypass_unsupported_api_diagnostics() {
    use falcone_serverless_worker::adapters::compat_go::{TreeFile, check_tree};
    for alias in ["ρ", "反射", "rρ", "_反射", "ρ２"] {
        for imports in [
            format!("{alias} \"reflect\""),
            format!("({alias} \"reflect\"; _ \"reflect\")"),
            format!("(_ \"reflect\"; {alias} \"reflect\")"),
            format!("(. \"reflect\"; {alias} \"reflect\"; _ \"reflect\")"),
        ] {
            let source = format!("package main\nimport {imports}\nvar _ = {alias}.MakeFunc\n");
            let verdict = check_tree(
                &[TreeFile {
                    path: "main.go",
                    text: &source,
                }],
                None,
            );
            assert!(
                verdict.diagnostics().iter().any(|d| {
                    d.path == "main.go"
                        && d.line == 3
                        && d.code == "WASM-UNSUPPORTED-API"
                        && d.construct == "unsupported reflection API"
                }),
                "alias {alias}, imports {imports}: {verdict:?}"
            );
        }
    }
}

#[test]
fn documented_workloads_interfaces_and_negative_diagnostics() {
    use falcone_serverless_worker::adapters::compat_go::{TreeFile, check_tree};
    let m = manifest();
    let profile = root().join("qualification/profiles/go");
    let mut resolve = wit_parser::Resolve::default();
    let (id, _) = resolve.push_path(root().join("wit/host.wit")).unwrap();
    let package = &resolve.packages[id];
    let allowed = m["wit"]["interfaces"].as_array().unwrap();
    for name in allowed {
        let name = name
            .as_str()
            .unwrap()
            .strip_prefix("falcone:host/")
            .unwrap()
            .strip_suffix("@0.2.0")
            .unwrap();
        assert!(package.interfaces.contains_key(name));
    }
    let matrix: Value = serde_json::from_str(
        &fs::read_to_string(root().join("../docs/contracts/language-profile-matrix.json")).unwrap(),
    )
    .unwrap();
    let allowed_json: Vec<_> = allowed.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(
        matrix["profiles"]["go"]["allowedInterfaces"],
        serde_json::json!(allowed_json)
    );
    let expected_wasi = [
        "wasi:random/random@0.2.0",
        "wasi:cli/stdout@0.2.0",
        "wasi:io/streams@0.2.0",
        "wasi:io/error@0.2.0",
    ];
    let wasi: Vec<_> = m["host_wasi"]["interfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(wasi, expected_wasi);
    for workload in m["workloads"].as_array().unwrap() {
        for import in workload["imports"].as_array().unwrap() {
            assert!(allowed.contains(import));
        }
        let path = workload["source"].as_str().unwrap();
        let text = fs::read_to_string(profile.join(path)).unwrap();
        assert!(
            check_tree(
                &[TreeFile { path, text: &text }],
                Some(&fs::read_to_string(profile.join("go.mod")).unwrap())
            )
            .supported()
        );
        let _: Value = serde_json::from_str(
            &fs::read_to_string(profile.join(workload["expected"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
    }
    for fixture in m["negative"].as_array().unwrap() {
        let path = fixture["file"].as_str().unwrap();
        let text = fs::read_to_string(profile.join(path)).unwrap();
        let result = check_tree(&[TreeFile { path, text: &text }], None);
        let found: Vec<_> = result
            .diagnostics()
            .iter()
            .map(|d| (d.code, d.path.as_str(), d.line))
            .collect();
        assert_eq!(
            found,
            vec![(
                fixture["code"].as_str().unwrap(),
                path,
                fixture["line"].as_integer().unwrap() as usize
            )]
        );
    }
    let doc = fs::read_to_string(root().join("../docs/qualification/profiles/go.md")).unwrap();
    let recorded = doc
        .split_once("<!-- manifest:start -->\n```toml\n")
        .unwrap()
        .1
        .split_once("\n```\n<!-- manifest:end -->")
        .unwrap()
        .0;
    assert_eq!(
        recorded.trim(),
        fs::read_to_string(profile.join("manifest.toml"))
            .unwrap()
            .trim(),
        "go.md drift"
    );
}

#[test]
fn trusted_build_fails_closed_before_running_unpinned_tools() {
    use std::process::Command;
    let script = root().join("qualification/go/build-profile.sh");
    let output = Command::new("bash")
        .arg(&script)
        .env_remove("FS_TRUSTED_COMPILER_IMAGE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("requires the trusted compiler image")
    );
    let output = Command::new("bash")
        .arg(&script)
        .env("FS_TRUSTED_COMPILER_IMAGE", "1")
        .env_remove("FS_TOOLCHAIN_CACHE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("offline toolchain cache required"));
}
