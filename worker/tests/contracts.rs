//! Documentation gates only; these do not implement the compiler sandbox.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

const SOURCE_HEADER: &[&str] = &["Contract: ctr-source-build-api", "Version: 1"];
const REQUEST_FIELDS: &[&str] = &[
    "source_ref",
    "language_profile",
    "catalogue_version",
    "base_ref",
    "output_prefix",
    "compile_limits",
    "build_credential",
];
const RESPONSE_FIELDS: &[&str] = &[
    "wasm_digest",
    "base_digest",
    "sbom_ref",
    "provenance_ref",
    "attestation_ref",
    "signature_ref",
    "signing_key_id",
    "imports",
    "features",
    "diagnostics",
];
const OUTCOMES: &[&str] = &["limit_exceeded", "base_mismatch"];
const RULES: &[&str] = &[
    "SB-R1", "SB-R2", "SB-R3", "SB-R4", "SB-R5", "SB-R6", "SB-R7", "SB-R8", "SB-R9", "SB-R10",
];
const CONFIG_HEADER: &[&str] = &["Contract: ctr-compiler-runtime-config", "Version: 2"];
const SERVICE_KEYS: &[&str] = &[
    "listen_port",
    "metrics_port",
    "probe_port",
    "compile_limits",
    "toolchain_dir",
    "catalogue_dir",
    "cache_output",
    "storage_endpoint",
    "tls_cert_path",
    "jwks_url",
];
const RUNNER_KEYS: &[&str] = &[
    "workspace_root",
    "landlock_min_abi",
    "build_cpu_seconds",
    "build_max_processes",
    "build_output_bytes",
    "build_wall_clock",
    "control_socket",
];

fn document(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/contracts")
            .join(name),
    )
    .expect("read canonical contract")
}

fn fixture(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/contracts")
            .join(name),
    )
    .expect("read negative fixture")
}

fn rejects(result: Result<(), String>, element: &str) {
    let error = result.expect_err("invalid contract must return Err");
    assert!(error.contains(element), "expected {element:?} in {error:?}");
}

fn require_lines(text: &str, lines: &[&str]) -> Result<(), String> {
    for line in lines {
        if !text.lines().any(|actual| actual == *line) {
            return Err(format!("missing {line}"));
        }
    }
    Ok(())
}

fn require_tokens(text: &str, tokens: &[&str]) -> Result<(), String> {
    for token in tokens {
        if !text.contains(&format!("`{token}`")) {
            return Err(format!("missing {token}"));
        }
    }
    Ok(())
}

fn section<'a>(text: &'a str, heading: &str) -> Result<&'a str, String> {
    let marker = format!("## {heading}");
    let start = text
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            *offset += line.len();
            Some((*offset, line.trim_end_matches(['\r', '\n'])))
        })
        .find(|(_, line)| *line == marker)
        .map(|(offset, _)| offset)
        .ok_or_else(|| format!("missing section {heading}"))?;
    let body = &text[start..];
    Ok(body.split("\n## ").next().unwrap_or(body))
}

fn validate_source_build(text: &str) -> Result<(), String> {
    require_lines(text, SOURCE_HEADER)?;
    require_tokens(section(text, "Request fields")?, REQUEST_FIELDS)?;
    require_tokens(section(text, "Response fields")?, RESPONSE_FIELDS)?;
    require_tokens(section(text, "Outcomes")?, OUTCOMES)?;
    require_tokens(section(text, "Build isolation rules")?, RULES)
}

fn validate_compiler_config(text: &str) -> Result<(), String> {
    if text.contains("falcone_service_token") {
        return Err("forbidden falcone_service_token: no standing Falcone credential".into());
    }
    require_lines(text, CONFIG_HEADER)?;
    require_tokens(section(text, "Service keys")?, SERVICE_KEYS)?;
    section(text, "Authentication")?;
    require_tokens(
        section(text, "Artifact signing key")?,
        &["signing_key_path"],
    )?;
    require_tokens(section(text, "Build-runner container")?, RUNNER_KEYS)?;
    let mut heading = "preamble";
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            heading = name;
        }
        if line.contains("signing_key_path") && heading != "Artifact signing key" {
            return Err(format!("forbidden signing_key_path in {heading} section"));
        }
    }
    Ok(())
}

#[test]
fn source_build_contract_is_complete() {
    validate_source_build(&document("source-build.md")).expect("complete source build contract");
}

#[test]
fn compiler_runtime_config_contract_is_complete() {
    validate_compiler_config(&document("compiler-runtime-config.md"))
        .expect("complete compiler runtime config contract");
}

#[test]
fn compiler_config_standing_credential_is_rejected() {
    rejects(
        validate_compiler_config(&fixture("compiler-config-standing-credential.md")),
        "falcone_service_token",
    );
}

#[test]
fn compiler_config_runner_secret_is_rejected() {
    let result = validate_compiler_config(&fixture("compiler-config-runner-secret.md"));
    let error = result.expect_err("runner must never receive the signing key");
    assert!(error.contains("signing_key_path"), "{error}");
    assert!(error.contains("Build-runner container"), "{error}");
}

#[test]
fn source_build_missing_signature_is_rejected() {
    rejects(
        validate_source_build(&fixture("source-build-missing-signature.md")),
        "signature_ref",
    );
}

#[test]
fn source_build_missing_base_ref_is_rejected() {
    rejects(
        validate_source_build(&fixture("source-build-missing-base-ref.md")),
        "base_ref",
    );
}

#[test]
fn source_build_base_mismatch_is_rejected() {
    let error = validate_source_build(&fixture("source-build-base-mismatch.md"))
        .expect_err("base_mismatch outcome is required");
    assert!(
        error.contains("base_mismatch") || error.contains("SB-R10"),
        "{error}"
    );
}

#[test]
fn every_required_element_is_enforced() {
    let source = document("source-build.md");
    validate_source_build(&source).expect("valid mutation baseline");
    for token in SOURCE_HEADER
        .iter()
        .chain(REQUEST_FIELDS)
        .chain(RESPONSE_FIELDS)
        .chain(OUTCOMES)
        .chain(RULES)
    {
        rejects(validate_source_build(&source.replace(token, "")), token);
    }
    for heading in [
        "Request fields",
        "Response fields",
        "Outcomes",
        "Build isolation rules",
    ] {
        rejects(
            validate_source_build(&source.replace(&format!("## {heading}"), "")),
            heading,
        );
    }

    let config = document("compiler-runtime-config.md");
    validate_compiler_config(&config).expect("valid mutation baseline");
    for token in CONFIG_HEADER
        .iter()
        .chain(SERVICE_KEYS)
        .chain(RUNNER_KEYS)
        .chain(&["signing_key_path"])
    {
        rejects(validate_compiler_config(&config.replace(token, "")), token);
    }
    for heading in [
        "Service keys",
        "Authentication",
        "Artifact signing key",
        "Build-runner container",
    ] {
        rejects(
            validate_compiler_config(&config.replace(&format!("## {heading}"), "")),
            heading,
        );
    }
}

#[test]
fn signing_key_in_any_other_section_is_rejected() {
    let config = document("compiler-runtime-config.md");
    for heading in [
        "Service keys",
        "Authentication",
        "Build-runner container",
        "Design basis",
    ] {
        let misplaced = config.replace(
            &format!("## {heading}"),
            &format!("## {heading}\n`signing_key_path`"),
        );
        let error = validate_compiler_config(&misplaced).expect_err("misplaced signing key");
        assert!(error.contains("signing_key_path"), "{error}");
        assert!(error.contains(heading), "{error}");
    }
    rejects(
        validate_compiler_config(&format!("signing_key_path\n{config}")),
        "signing_key_path",
    );
}

const BASE_MISMATCH: &str = OUTCOMES[1];
const BASE_SUCCESS: &str = "success";
const BASE_KEYS: &[&str] = &[
    "requested.base_ref.id",
    "requested.base_ref.version",
    "requested.base_ref.digest",
    "requested.language_profile",
    "actual.base_digest",
    "actual.base_profile",
    "expected_outcome",
];

fn base_fixture(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/contracts/fixtures/source-build")
            .join(name),
    )
    .expect("read base composition fixture")
}

fn parse_base(text: &str) -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("malformed line {line:?}"))?;
        let (key, value) = (key.trim(), value.trim());
        if !BASE_KEYS.contains(&key) {
            return Err(format!("unknown key {key}"));
        }
        if value.is_empty() {
            return Err(format!("empty value for {key}"));
        }
        if values.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("duplicate key {key}"));
        }
    }
    for key in BASE_KEYS {
        if !values.contains_key(*key) {
            return Err(format!("missing key {key}"));
        }
    }
    Ok(values)
}

/// Computes the outcome from the requested and actual values only, by exact equality.
fn base_outcome(text: &str) -> Result<(&'static str, String), String> {
    let values = parse_base(text)?;
    let mut differing = Vec::new();
    if values["requested.base_ref.digest"] != values["actual.base_digest"] {
        differing.push("base_digest");
    }
    if values["requested.language_profile"] != values["actual.base_profile"] {
        differing.push("base_profile");
    }
    if differing.is_empty() {
        Ok((BASE_SUCCESS, String::new()))
    } else {
        Ok((BASE_MISMATCH, differing.join(", ")))
    }
}

/// Checks the computed outcome against the fixture's declared `expected_outcome`.
fn check_base_fixture(text: &str) -> Result<(&'static str, String), String> {
    let values = parse_base(text)?;
    let expected = values["expected_outcome"].as_str();
    if expected != BASE_SUCCESS && expected != BASE_MISMATCH {
        return Err(format!("unknown expected_outcome {expected}"));
    }
    let (outcome, diagnostic) = base_outcome(text)?;
    if outcome != expected {
        return Err(format!(
            "computed outcome {outcome} but fixture expects {expected} ({})",
            if diagnostic.is_empty() {
                "bases match"
            } else {
                &diagnostic
            }
        ));
    }
    Ok((outcome, diagnostic))
}

#[test]
fn base_outcome_token_is_the_listed_outcome() {
    assert_eq!(BASE_MISMATCH, "base_mismatch");
    assert!(OUTCOMES.contains(&BASE_MISMATCH));
}

#[test]
fn matching_base_succeeds() {
    let (outcome, diagnostic) = check_base_fixture(&base_fixture("base-match.txt")).unwrap();
    assert_eq!(outcome, BASE_SUCCESS);
    assert!(diagnostic.is_empty(), "{diagnostic}");
}

#[test]
fn differing_base_digest_is_base_mismatch() {
    let (outcome, diagnostic) =
        check_base_fixture(&base_fixture("base-digest-mismatch.txt")).unwrap();
    assert_eq!(outcome, BASE_MISMATCH);
    assert!(diagnostic.contains("base_digest"), "{diagnostic}");
    assert!(!diagnostic.contains("base_profile"), "{diagnostic}");
}

#[test]
fn differing_base_profile_is_base_mismatch() {
    let (outcome, diagnostic) =
        check_base_fixture(&base_fixture("base-profile-mismatch.txt")).unwrap();
    assert_eq!(outcome, BASE_MISMATCH);
    assert!(diagnostic.contains("base_profile"), "{diagnostic}");
    assert!(!diagnostic.contains("base_digest"), "{diagnostic}");
}

#[test]
fn mismatched_base_claiming_success_is_rejected() {
    rejects(
        check_base_fixture(&base_fixture("base-mismatch-claims-success.txt")).map(|_| ()),
        "base_mismatch",
    );
}

#[test]
fn matching_base_claiming_mismatch_is_rejected() {
    let text = base_fixture("base-match.txt").replace("= success", "= base_mismatch");
    rejects(check_base_fixture(&text).map(|_| ()), "success");
}

#[test]
fn base_fixture_fails_closed_on_missing_empty_duplicate_or_unknown() {
    let baseline = base_fixture("base-match.txt");
    check_base_fixture(&baseline).expect("valid mutation baseline");
    for key in BASE_KEYS {
        let kept: Vec<&str> = baseline
            .lines()
            .filter(|line| !line.starts_with(key))
            .collect();
        rejects(
            check_base_fixture(&kept.join("\n")).map(|_| ()),
            &format!("missing key {key}"),
        );
        let emptied: Vec<String> = baseline
            .lines()
            .map(|line| {
                if line.starts_with(key) {
                    format!("{key} =")
                } else {
                    line.to_string()
                }
            })
            .collect();
        rejects(
            check_base_fixture(&emptied.join("\n")).map(|_| ()),
            &format!("empty value for {key}"),
        );
        let duplicated = format!("{baseline}\n{key} = x");
        rejects(
            check_base_fixture(&duplicated).map(|_| ()),
            &format!("duplicate key {key}"),
        );
    }
    rejects(
        check_base_fixture(&baseline.replace("= success", "= maybe")).map(|_| ()),
        "unknown expected_outcome",
    );
}
