//! Source-only qualification: no component compilation, loading or instantiation.
use sha2::{Digest, Sha256};
use std::fs;
#[path = "support/javascript_profile.rs"]
pub mod profile;
use profile::{imports_valid, manifest, pins_valid, profile_dir};
use serde_json::Value;

#[test]
fn manifest_records_exact_operator_pins_and_an_empty_catalogue() {
    let manifest = manifest();
    assert!(pins_valid(&manifest));
    for (section, field, value) in [
        ("jco", "version", "1.41.0"),
        ("componentize_js", "version", "0.23.0"),
        ("wasi_virt", "version", "0.2.0"),
        (
            "wasi_virt",
            "rev",
            "bca61c27499d2c290b5431cfcff013737b3ba01d",
        ),
        ("node", "version", "v26.13.0"),
        ("wasmtime", "version", "49.0.5"),
        ("wit", "version", "0.2.0"),
        ("wasi", "version", "none"),
    ] {
        assert_eq!(manifest["pins"][section][field], value);
        for bad in ["*", "latest", "^1.0.0", ">=1.0.0", ""] {
            let mut copy = manifest.clone();
            copy["pins"][section][field] = bad.into();
            assert!(!pins_valid(&copy), "{section}.{field}: {bad}");
        }
        let mut copy = manifest.clone();
        copy["pins"][section].as_object_mut().unwrap().remove(field);
        assert!(!pins_valid(&copy));
    }
    assert_eq!(manifest["pins"]["engine"], "StarlingMonkey");
    assert_eq!(
        manifest["pins"]["catalogue"]["packages"],
        serde_json::json!({})
    );
    let mut copy = manifest.clone();
    copy["pins"]["catalogue"]["packages"] = serde_json::json!({"pg":"*"});
    assert!(!pins_valid(&copy));
    let workspace: toml::Value =
        toml::from_str(&fs::read_to_string(profile_dir().join("../../../Cargo.toml")).unwrap())
            .unwrap();
    assert_eq!(
        workspace["workspace"]["dependencies"]["wasmtime"]["version"]
            .as_str()
            .unwrap(),
        format!(
            "={}",
            manifest["pins"]["wasmtime"]["version"].as_str().unwrap()
        )
    );
}

#[test]
fn imports_are_exactly_the_pinned_host_world_without_wasi() {
    let manifest = manifest();
    assert!(imports_valid(&manifest, &[]));
    for name in [
        "wasi:cli/environment@0.2.0",
        "falcone:host/undeclared@0.2.0",
        "falcone:host/handler@0.2.0",
    ] {
        assert!(!imports_valid(&manifest, &[name.into()]));
        let mut copy = manifest.clone();
        copy["allowedInterfaces"]
            .as_array_mut()
            .unwrap()
            .push(name.into());
        assert!(!imports_valid(&copy, &[]));
    }
    let matrix: Value = serde_json::from_slice(
        &fs::read(profile_dir().join("../../../docs/contracts/language-profile-matrix.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        matrix["profiles"]["javascript"]["allowedInterfaces"],
        manifest["allowedInterfaces"]
    );
    assert_eq!(matrix["profiles"]["javascript"]["status"], "disabled");
}

#[test]
fn six_negative_fixtures_report_only_their_documented_diagnostic() {
    use falcone_serverless_worker::adapters::compat::{Source, check};
    let manifest = manifest();
    let mut seen = std::collections::BTreeSet::new();
    for path in manifest["negativeFixtures"]
        .as_array()
        .expect("six negative fixtures")
    {
        let fixture: Value =
            serde_json::from_slice(&fs::read(profile_dir().join(path.as_str().unwrap())).unwrap())
                .unwrap();
        let verdict = check(
            &[Source {
                text: fixture["source"].as_str().unwrap(),
            }],
            fixture["packageManifest"].as_str().unwrap(),
        );
        let actual: Vec<_> = verdict.diagnostics().iter().map(|d| serde_json::json!({"code":d.code,"construct":d.construct,"source":d.source,"line":d.line,"column":d.column})).collect();
        assert_eq!(
            serde_json::json!(actual),
            serde_json::json!([fixture["expected"]]),
            "{path}"
        );
        assert!(!verdict.supported());
        seen.insert(fixture["expected"]["code"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        seen,
        manifest["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect()
    );
    assert_eq!(seen.len(), 6);
}

#[test]
fn reference_sources_match_the_operator_receipts() {
    let manifest = manifest();
    assert_eq!(
        manifest["reference"]["sources"].as_array().unwrap().len(),
        4
    );
    for (path, digest) in [
        (
            "actions/openapi-spec-serve.mjs",
            "3827f5e2c7c14cc3cf5abb4eb8272d8cb67938dc4d195723efcf695bf8924ce1",
        ),
        (
            "src/config.mjs",
            "749d3fefde37945e57607ae462a6a55ee193181c898ae4bbbd641f1dae0a30e5",
        ),
        (
            "src/spec-version-repo.mjs",
            "5b13e9bd885954900ddf1d4e2d7b780d5e46d73e09074b31bdf5aa013f5de437",
        ),
        (
            "src/spec-cache.mjs",
            "74883a947b993384a4fe3fd697e7cf4f73bdb2f36ac688ec4303bea43d17e2dc",
        ),
    ] {
        let bytes = fs::read(profile_dir().join("reference").join(path)).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), digest, "{path}");
        assert!(
            manifest["reference"]["sources"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!({"path":path,"sha256":digest}))
        );
    }
}

#[test]
fn reference_diagnostics_are_recorded_and_qualification_is_blocked() {
    let manifest = manifest();
    let diagnostics = profile::reference_diagnostics(&manifest);
    assert_eq!(diagnostics, manifest["referenceDiagnostics"]);
    let checks = profile::static_checks(&manifest);
    assert_eq!(profile::verdict(&checks, &[]), "blocked");
    assert_eq!(manifest["verdict"], "blocked");
    assert!(!checks["compatibility"]);
    assert!(!checks["sourceClosure"]);
    assert!(!checks["componentImports"]);
    assert!(checks["sourceHashes"]);
    assert!(checks["negativeFixtures"]);
    assert!(checks["envelopeInventory"]);
    let mut copy = manifest.clone();
    copy["reference"]["sources"][0]["sha256"] = "0".repeat(64).into();
    assert!(!profile::static_checks(&copy)["sourceHashes"]);
    assert_eq!(
        profile::verdict(&profile::static_checks(&copy), &[true; 8]),
        "blocked"
    );
}

#[test]
fn every_static_check_and_all_eight_cluster_envelopes_are_required() {
    let all: std::collections::BTreeMap<_, _> = profile::STATIC_CHECKS
        .iter()
        .map(|name| ((*name).to_owned(), true))
        .collect();
    assert_eq!(profile::verdict(&all, &[true; 8]), "qualified");
    for name in profile::STATIC_CHECKS {
        let mut failed = all.clone();
        failed.insert((*name).into(), false);
        assert_eq!(profile::verdict(&failed, &[true; 8]), "blocked", "{name}");
        failed.remove(*name);
        assert_eq!(
            profile::verdict(&failed, &[true; 8]),
            "blocked",
            "missing {name}"
        );
    }
    for i in 0..8 {
        let mut envelopes = [true; 8];
        envelopes[i] = false;
        assert_eq!(profile::verdict(&all, &envelopes), "blocked");
    }
    for envelopes in [vec![], vec![true; 7], vec![true; 9]] {
        assert_eq!(profile::verdict(&all, &envelopes), "blocked");
    }
    assert_eq!(manifest()["runtime_mode"], "wasm");
}

#[test]
fn published_evidence_and_document_match_the_computed_blocked_verdict() {
    let manifest = manifest();
    let checks = profile::static_checks(&manifest);
    let actual = serde_json::json!({
        "id":manifest["id"], "runtime_mode":"wasm", "checks":checks,
        "diagnostics":profile::reference_diagnostics(&manifest),
        "cluster":"UNVERIFIED", "envelopes":[], "verdict":profile::verdict(&checks, &[]),
    });
    let receipt = &manifest["staticEvidence"];
    let bytes =
        fs::read(profile_dir().join(receipt["path"].as_str().expect("static evidence path")))
            .unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), actual);
    assert_eq!(receipt["sha256"], format!("{:x}", Sha256::digest(&bytes)));
    let doc = fs::read_to_string(
        profile_dir().join("../../../docs/qualification/profiles/javascript.md"),
    )
    .unwrap();
    let summary: Value = serde_json::from_str(
        doc.lines()
            .find_map(|line| {
                line.strip_prefix("<!-- qualification-summary: ")
                    .and_then(|line| line.strip_suffix(" -->"))
            })
            .expect("document summary"),
    )
    .unwrap();
    for key in [
        "id",
        "pins",
        "allowedInterfaces",
        "diagnostics",
        "verdict",
        "runtime_mode",
    ] {
        assert_eq!(summary[key], manifest[key], "document {key}");
    }
    for value in [
        &manifest["id"],
        &manifest["verdict"],
        &manifest["runtime_mode"],
        &receipt["artifactId"],
        &receipt["sha256"],
    ] {
        assert!(doc.contains(value.as_str().unwrap()), "missing {value}");
    }
    for section in [
        "jco",
        "componentize_js",
        "wasi_virt",
        "node",
        "wasmtime",
        "wit",
        "wasi",
    ] {
        for (key, value) in manifest["pins"][section].as_object().unwrap() {
            if key == "integrity" || key == "archive" {
                continue;
            }
            assert!(
                doc.contains(value.as_str().unwrap()),
                "missing {section}.{key}"
            );
        }
    }
    for list in ["allowedInterfaces", "diagnostics"] {
        for value in manifest[list].as_array().unwrap() {
            assert!(doc.contains(value.as_str().unwrap()), "missing {value}");
        }
    }
    for diagnostic in manifest["referenceDiagnostics"]["bundle"]
        .as_array()
        .unwrap()
        .iter()
        .chain(
            manifest["referenceDiagnostics"]["files"]
                .as_object()
                .unwrap()
                .values()
                .flat_map(|v| v.as_array().unwrap()),
        )
    {
        assert!(doc.contains(diagnostic["code"].as_str().unwrap()));
        assert!(doc.contains(diagnostic["construct"].as_str().unwrap()));
    }
    let mut mentioned = manifest.clone();
    mentioned["note"] = "OCI requires explicit operator authorization".into();
    profile::static_checks(&mentioned);
    assert_eq!(mentioned["runtime_mode"], "wasm");
    let mut request: Value = serde_json::from_str(include_str!(
        "fixtures/contract/request/valid/wasm-no-base.json"
    ))
    .unwrap();
    request["params"]["message"] = mentioned["note"].clone();
    let before = serde_json::to_vec(&request).unwrap();
    falcone_serverless_worker::api::parse_request(&before).unwrap();
    assert_eq!(request["projection"]["runtime_mode"], "wasm");
    assert!(!falcone_serverless_worker::api::refuses_runtime_mode(
        &before
    ));
}

#[test]
fn qualification_sources_contain_no_precompiled_or_native_artifacts() {
    fn walk(path: &std::path::Path) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            assert!(!entry.file_type().unwrap().is_symlink());
            let path = entry.path();
            if path.is_dir() {
                walk(&path);
                continue;
            }
            let extension = path.extension().and_then(|v| v.to_str());
            assert!(
                matches!(extension, Some("mjs" | "json" | "toml" | "sh" | "wasm")),
                "unexpected artifact {}",
                path.display()
            );
            let bytes = fs::read(&path).unwrap();
            assert!(!bytes.starts_with(b"\x7fELF"));
            if extension == Some("wasm") {
                // A raw validated component is allowed; native Wasmtime caches are not.
                falcone_serverless_worker::artifact::validate::validate(&bytes, 64 * 1024 * 1024)
                    .unwrap();
            } else {
                std::str::from_utf8(&bytes).unwrap();
            }
        }
    }
    walk(&profile_dir());
}

#[test]
#[ignore = "UNVERIFIED: operator-only build eligibility gate, expected blocked for this reference"]
fn reference_is_build_eligible() -> Result<(), String> {
    let checks = profile::static_checks(&manifest());
    if checks
        .iter()
        .filter(|(name, _)| name.as_str() != "componentImports")
        .all(|(_, passed)| *passed)
    {
        Ok(())
    } else {
        Err(
            "blocked: reference compatibility or static source checks failed; do not componentize"
                .into(),
        )
    }
}

#[test]
#[ignore = "UNVERIFIED: requires FS_JS_PROFILE_COMPONENT from build-fixtures.sh in the trusted image"]
fn built_reference_imports() {
    let path =
        std::env::var_os("FS_JS_PROFILE_COMPONENT").expect("trusted raw component path required");
    let bytes = fs::read(path).unwrap();
    let component =
        falcone_serverless_worker::artifact::validate::validate(&bytes, 64 * 1024 * 1024).unwrap();
    assert!(imports_valid(&manifest(), &component.imports));
}
