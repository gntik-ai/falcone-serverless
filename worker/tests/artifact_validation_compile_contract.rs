//! Documentation conformance only; no compiler, supervisor or cache loader runs here.

use std::fs;
use std::path::Path;

const HEADER: &[&str] = &["Contract: ctr-artifact-validation-compile", "Version: 1"];

struct Section {
    heading: &'static str,
    tokens: &'static [&'static str],
    statements: &'static [(&'static str, &'static str)],
}

const SECTIONS: &[Section] = &[
    Section {
        heading: "Input fields",
        tokens: &[
            "raw_wasm_ref",
            "profile",
            "engine_version",
            "wasi_version",
            "interface_version",
            "target_architecture",
            "engine_config",
            "limits",
        ],
        statements: &[(
            "raw_wasm_ref",
            "References must be immutable, digest-pinned and authorized for the requesting tenant before fetching raw Wasm modules or components.",
        )],
    },
    Section {
        heading: "Output fields",
        tokens: &[
            "verdict",
            "imports",
            "features",
            "artifact_digest",
            "diagnostics",
            "accepted",
            "rejected",
        ],
        statements: &[],
    },
    Section {
        heading: "Compile limits",
        tokens: &[
            "AVC-R4",
            "AVC-R5",
            "AVC-R6",
            "cpu_seconds",
            "memory_bytes",
            "wall_clock_deadline",
            "input_bytes",
            "guest_memory_maximum",
            "buffer_limits",
            "ctr-wit-capability-host",
            "worker/wit/README.md",
        ],
        statements: &[
            (
                "AVC-R4",
                "A supervisor outside the compiler enforces every compile limit over the entire compile process tree.",
            ),
            (
                "cpu_seconds",
                "CPU consumption must not exceed the `cpu_seconds` budget.",
            ),
            (
                "memory_bytes",
                "Compiler memory consumption must not exceed the `memory_bytes` budget.",
            ),
            (
                "wall_clock_deadline",
                "Elapsed compilation time must not pass the `wall_clock_deadline`.",
            ),
            (
                "input_bytes",
                "Raw Wasm input size must not exceed the `input_bytes` budget before validation or compilation.",
            ),
            (
                "guest_memory_maximum",
                "Validation rejects any declared memory maximum above the profile guest limit (`guest_memory_maximum`).",
            ),
            (
                "buffer_limits",
                "Guest/host `buffer_limits` belong to `ctr-wit-capability-host` (falcone-serverless:WAS-02.t1), currently `worker/wit/README.md`; this contract does not redefine them.",
            ),
        ],
    },
    Section {
        heading: "Cancellation boundaries",
        tokens: &[
            "AVC-R7",
            "AVC-R8",
            "AVC-R9",
            "AVC-R12",
            "deadline_termination",
            "caller_gone_cancellation",
            "no_partial_cache_write",
            "guest_computation_only",
        ],
        statements: &[
            (
                "deadline_termination",
                "At the deadline, the supervisor terminates and reaps the entire compile process tree before returning.",
            ),
            (
                "caller_gone_cancellation",
                "When the caller goes away, cancel compilation and have the supervisor terminate and reap the entire compile process tree before returning.",
            ),
            (
                "no_partial_cache_write",
                "Neither deadline termination nor caller-gone cancellation writes a partial cache entry; discard staged output and publish no entry.",
            ),
            (
                "guest_computation_only",
                "Fuel/epoch interruption covers guest computation only, never compiler execution; compile termination is supervisor-enforced.",
            ),
        ],
    },
    Section {
        heading: "Isolation and cache rules",
        tokens: &[
            "AVC-R1",
            "AVC-R2",
            "AVC-R3",
            "AVC-R10",
            "AVC-R11",
            "validate_before_compile",
            "never_deserialize",
            "separate_trusted_process",
            "no_in_process_compile",
            "verified_entries_only",
        ],
        statements: &[
            (
                "validate_before_compile",
                "Validate raw Wasm before compilation; validation failure stops compilation.",
            ),
            (
                "never_deserialize",
                "Reject all non-raw-Wasm tenant input, including native precompiled and serialized engine artifacts; never deserialize tenant-supplied artifacts.",
            ),
            (
                "never_deserialize",
                "Tenant precompiled input is rejected without deserialization.",
            ),
            (
                "never_deserialize",
                "Tenant serialized input is rejected without deserialization.",
            ),
            (
                "separate_trusted_process",
                "Compilation runs in a separate trusted process outside the worker tenant execution path.",
            ),
            (
                "no_in_process_compile",
                "The worker never compiles tenant Wasm in-process.",
            ),
            (
                "verified_entries_only",
                "The worker loads only verified trusted-compiler cache entries; without one it requests a trusted compile or refuses execution.",
            ),
        ],
    },
    Section {
        heading: "Cache key",
        tokens: &[
            "artifact_digest",
            "engine_version",
            "target_architecture",
            "engine_config",
            "epoch",
            "fuel",
            "interface_version",
        ],
        statements: &[(
            "engine_config",
            "The `engine_config` component includes the exact `epoch` interruption and `fuel` settings.",
        )],
    },
    Section {
        heading: "Diagnostic codes",
        tokens: &[
            "invalid_module",
            "precompiled_rejected",
            "compile_cpu_limit_exceeded",
            "compile_memory_limit_exceeded",
            "compile_deadline_exceeded",
            "compile_size_limit_exceeded",
            "compile_cancelled",
            "unsupported_feature",
            "undeclared_import",
            "trusted_cache_unavailable",
            "cache_key_mismatch",
        ],
        statements: &[],
    },
    Section {
        heading: "Design basis",
        tokens: &[
            "FR-WAS-04",
            "WAS-02",
            "ctr-source-build-api",
            "ctr-revision-runtime-manifest",
        ],
        statements: &[],
    },
];

fn document() -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/contracts/artifact-validation-compile.md"),
    )
    .expect("read canonical artifact validation and compile contract")
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

fn validate(text: &str) -> Result<(), String> {
    let lowercase = text.to_ascii_lowercase();
    for forbidden in [
        "tenant precompiled input is accepted",
        "tenant serialized input is accepted",
    ] {
        if lowercase.contains(forbidden) {
            return Err(format!("forbidden statement: {forbidden}"));
        }
    }
    for line in HEADER {
        if !text.lines().any(|actual| actual == *line) {
            return Err(format!("missing {line}"));
        }
    }
    for required in SECTIONS {
        let body = section(text, required.heading)?;
        for token in required.tokens {
            if !body.contains(&format!("`{token}`")) {
                return Err(format!("missing {token} in {}", required.heading));
            }
        }
        for (element, statement) in required.statements {
            if !body.contains(statement) {
                return Err(format!("missing {element}: {statement}"));
            }
        }
    }
    Ok(())
}

fn replace_present(text: &str, anchor: &str, replacement: &str) -> String {
    assert!(text.contains(anchor), "mutation anchor missing: {anchor}");
    text.replace(anchor, replacement)
}

fn rejects(text: &str, element: &str) {
    let error = validate(text).expect_err("mutated contract must return Err");
    assert!(error.contains(element), "expected {element:?} in {error:?}");
}

#[test]
fn canonical_contract_is_complete() {
    validate(&document()).expect("complete artifact validation and compile contract");
}

#[test]
fn every_required_header_token_and_section_is_enforced() {
    let baseline = document();
    validate(&baseline).expect("valid mutation baseline");
    for header in HEADER {
        rejects(&replace_present(&baseline, header, ""), header);
    }
    for required in SECTIONS {
        let heading = format!("## {}", required.heading);
        rejects(&replace_present(&baseline, &heading, ""), required.heading);
        for token in required.tokens {
            rejects(
                &replace_present(&baseline, &format!("`{token}`"), ""),
                token,
            );
        }
    }
}

#[test]
fn removing_safety_statements_is_rejected() {
    let baseline = document();
    validate(&baseline).expect("valid mutation baseline");
    for required in SECTIONS {
        for (element, statement) in required.statements {
            rejects(&replace_present(&baseline, statement, ""), element);
        }
    }
}

#[test]
fn tokens_in_the_wrong_section_do_not_satisfy_the_contract() {
    let baseline = document();
    validate(&baseline).expect("valid mutation baseline");
    for required in SECTIONS {
        let body = section(&baseline, required.heading).expect("required section");
        for token in required.tokens {
            let marker = format!("`{token}`");
            let changed_body = replace_present(body, &marker, "");
            let misplaced = format!("{marker}\n{}", baseline.replacen(body, &changed_body, 1));
            rejects(&misplaced, token);
        }
        for (element, statement) in required.statements {
            let changed_body = replace_present(body, statement, "");
            let misplaced = format!("{statement}\n{}", baseline.replacen(body, &changed_body, 1));
            rejects(&misplaced, element);
        }
    }
}

#[test]
fn accepting_precompiled_or_serialized_tenant_input_is_forbidden() {
    let baseline = document();
    validate(&baseline).expect("valid mutation baseline");
    for kind in ["precompiled", "serialized"] {
        let rejection = format!("Tenant {kind} input is rejected");
        let accepted = replace_present(
            &baseline,
            &rejection,
            &format!("Tenant {kind} input is accepted"),
        );
        rejects(&accepted, "forbidden");
        rejects(
            &format!("{baseline}\nTenant {kind} input is accepted."),
            "forbidden",
        );
    }
}
