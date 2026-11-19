use falcone_serverless_worker::adapters::compat::check;
use falcone_serverless_worker::{adapters::javascript, api};
#[path = "support/legacy_oracle.rs"]
mod oracle;
#[path = "support/legacy_runtime.rs"]
mod runtime;

fn request() -> api::ExecuteRequest {
    api::parse_request(include_bytes!(
        "fixtures/contract/request/valid/wasm-no-base.json"
    ))
    .unwrap()
}

#[test]
fn plain_main_is_supported() {
    assert!(check("function main(params) { return params; }", b"{}").is_supported());
}

#[test]
fn web_crypto_is_supported_without_a_polyfill() {
    for source in [
        "async function main(params) { const data = new Uint8Array(8); crypto.getRandomValues(data); return await crypto.subtle.digest('SHA-256', data); }",
        "import crypto from 'node:crypto'; async function main(params) { return await crypto.subtle.digest('SHA-256', new Uint8Array(8)); }",
    ] {
        let verdict = check(source, b"{}");
        assert!(verdict.is_supported(), "{:?}", verdict.diagnostics());
    }
}

#[test]
fn unsupported_features_have_stable_redacted_diagnostics() {
    for (source, code, construct) in [
        (
            "require('binding.node');",
            "WASM-NATIVE-LIBRARY",
            "native addon",
        ),
        (
            "process.dlopen(module, 'binding');",
            "WASM-NATIVE-LIBRARY",
            "process.dlopen",
        ),
        (
            "require('child_process');",
            "WASM-PROCESS-SPAWN",
            "process or thread module",
        ),
        (
            "require('node:worker_threads');",
            "WASM-PROCESS-SPAWN",
            "process or thread module",
        ),
        (
            "import cluster from 'node:cluster';",
            "WASM-PROCESS-SPAWN",
            "process or thread module",
        ),
        ("navigator.gpu.requestAdapter();", "WASM-GPU", "GPU access"),
        (
            "require('unqualified-package');",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            "third-party module",
        ),
        (
            "import fs from 'node:fs';",
            "WASM-UNSUPPORTED-API",
            "unsupported node module",
        ),
        (
            "import net from 'node:net';",
            "WASM-UNSUPPORTED-API",
            "unsupported node module",
        ),
        (
            "crypto.createHash('sha256');",
            "WASM-UNSUPPORTED-API",
            "crypto API",
        ),
        (
            "eval('tenant-private-value');",
            "WASM-UNSUPPORTED-API",
            "eval",
        ),
        (
            "new Function('tenant-private-value');",
            "WASM-UNSUPPORTED-API",
            "new Function",
        ),
        (
            "require(params.secret);",
            "WASM-UNSUPPORTED-API",
            "dynamic require",
        ),
        (
            "import(params.secret);",
            "WASM-UNSUPPORTED-API",
            "dynamic import",
        ),
    ] {
        let verdict = check(&format!("// fixture\n{source}"), b"{}");
        assert!(!verdict.is_supported());
        assert!(
            verdict.diagnostics().iter().any(|d| d.code == code
                && d.construct == construct
                && d.file == "function.js"
                && d.line == 2),
            "{source}: {:?}",
            verdict.diagnostics()
        );
        let text = format!("{verdict:?}");
        assert!(!text.to_lowercase().contains("oci"));
        assert!(!text.contains("tenant-private-value"));
    }
    for (manifest, code, construct) in [
        (
            "{\n\"scripts\":{\"postinstall\":\"private-value\"}}",
            "WASM-INSTALL-HOOK",
            "lifecycle script",
        ),
        (
            "{\n\"dependencies\":{\"private-package\":\"1.2.3\"}}",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            "third-party dependency",
        ),
        (
            "{\n\"optionalDependencies\":{\"private-package\":\"1.2.3\"}}",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            "third-party dependency",
        ),
    ] {
        let verdict = check(
            "function main(params) { return params; }",
            manifest.as_bytes(),
        );
        assert!(!verdict.is_supported());
        let d = &verdict.diagnostics()[0];
        assert_eq!(
            (d.code, d.construct, d.file, d.line),
            (code, construct, "package.json", 2)
        );
        assert!(!format!("{d:?} {d}").contains("private-"));
    }
}

#[test]
fn unknown_or_unparseable_constructs_fail_closed() {
    for source in [
        "function main(params) {",
        "function main(params) { return mysterium(params); }",
        "function main(params) { return params['constructor']('return process')(); }",
        "function main(params) { return params.constructor; }",
        "function main(params) { const function = 1; return function; }",
        "function main(params) { return `${eval(params.source)}`; }",
        "function main(params) { return /eval/.test(params); }",
        "function main(params) { return crypto['createHash']('sha256'); }",
        "function main(params) { return cr\\u0079pto.createHash('sha256'); }",
        "function main(params) { return globalThis.crypto.createHash('sha256'); }",
        "import crypto from 'node:crypto'; import crypto from 'node:crypto'; function main(params) { return params; }",
        "function main(params) { throw\nnew Error('failure'); }",
        "function main(params) { return await crypto.subtle.digest('SHA-256', new Uint8Array(1)); }",
        "function main(params) { return {__proto__:1,__proto__:2}; }",
    ] {
        assert!(!check(source, b"{}").is_supported(), "{source}");
    }
    assert!(!check("function main(params) { return params; }", b"[]").is_supported());
    for manifest in [
        b"{\"scripts\":[]}".as_slice(),
        b"{\"dependencies\":[]}".as_slice(),
    ] {
        assert!(!check("function main(params) { return params; }", manifest).is_supported());
    }
}

#[test]
fn manifest_diagnostics_locate_actual_top_level_keys() {
    let manifest = br#"{
      "description": "dependencies",
      "\u0064ependencies": {"synthetic-package":"1.2.3"}
    }"#;
    let verdict = check("function main(params) { return params; }", manifest);
    assert_eq!(verdict.diagnostics()[0].line, 3);
    assert_eq!(
        verdict.diagnostics()[0].code,
        "WASM-PACKAGE-NOT-IN-CATALOGUE"
    );
}

#[test]
fn context_refusals_precede_every_guest_start_and_do_not_echo_inputs() {
    let request = request();
    let good = request.plan();
    let verdict = check("function main(params) { return params; }", b"{}");
    let mut starts = 0;
    for kind in ["missing", "revision", "tenant", "workspace", "credential"] {
        let mut plan = request.plan();
        match kind {
            "revision" => plan.revision = "private-revision".into(),
            "tenant" => plan.context.tenant = "private-tenant".into(),
            "workspace" => plan.context.workspace = "private-workspace".into(),
            "credential" => plan.credential = "invalid-private-credential".into(),
            _ => {}
        }
        let error = javascript::invoke(
            &request,
            if kind == "missing" { None } else { Some(&plan) },
            &verdict,
            || {
                starts += 1;
                panic!("guest must not start");
            },
        )
        .err()
        .unwrap();
        let expected = if kind == "missing" {
            "execution context missing"
        } else {
            "execution context does not match the pinned revision"
        };
        assert_eq!(error.to_string(), expected);
        let (status, bytes) = error.response(&request);
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, 400);
        assert_eq!(body["result"]["code"], "invalid_request");
        assert_eq!(body["result"]["detail"]["reason"], "malformed_message");
        assert_eq!(body["class"], "non_retryable");
        assert_eq!(body["result"]["retryable"], false);
        let text = format!("{error:?} {error}");
        for value in [
            &good.context.tenant,
            &good.context.workspace,
            &good.revision,
            &plan.revision,
            &plan.context.tenant,
            &plan.context.workspace,
        ] {
            assert!(!text.contains(value));
        }
    }
    assert_eq!(starts, 0);
}

#[test]
fn disabled_and_blocked_functions_never_reach_the_instantiation_boundary() {
    let request = request();
    let plan = request.plan();
    let mut starts = 0;
    for (source, expected) in [
        (
            "function main(params) { return params; }",
            "language profile disabled",
        ),
        (
            "function main(params) { return eval(params.source); }",
            "function blocked by compatibility check",
        ),
    ] {
        let error = javascript::invoke(&request, Some(&plan), &check(source, b"{}"), || {
            starts += 1;
            panic!("guest must not start");
        })
        .err()
        .unwrap();
        assert_eq!(error.to_string(), expected);
    }
    assert_eq!(starts, 0);
    assert!(
        falcone_serverless_worker::adapters::admit_profile(
            "unrecognized-profile",
            &check("function main(params) { return params; }", b"{}")
        )
        .is_err()
    );
}

#[test]
fn envelope_oracle_is_structural_and_requires_declared_variables() {
    use serde_json::json;
    use std::collections::BTreeMap;
    let none = BTreeMap::new();
    let expected = json!({"result":{"statusCode":200,"body":[1,2]},"logs":[]});
    let reordered: serde_json::Value =
        serde_json::from_str("{ \"logs\":[], \"result\":{\"body\":[1.0,2], \"statusCode\":200} }")
            .unwrap();
    assert!(oracle::envelope(&reordered, &expected, &none));
    for wrong in [
        json!({"result":{"statusCode":200,"body":[2,1]},"logs":[]}),
        json!({"result":{"statusCode":200,"body":[1,2]},"logs":[],"extra":0}),
        json!({"result":{"statusCode":200,"body":[1,2]}}),
    ] {
        assert!(!oracle::envelope(&wrong, &expected, &none));
    }
    let variables = BTreeMap::from([("/activation".into(), oracle::Variable::String)]);
    let expected = json!({"activation":"placeholder","result":1});
    assert!(oracle::envelope(
        &json!({"activation":"synthetic-id","result":1.0}),
        &expected,
        &variables
    ));
    assert!(!oracle::envelope(
        &json!({"activation":42,"result":1}),
        &expected,
        &variables
    ));
    assert!(!oracle::envelope(
        &json!({"result":1}),
        &expected,
        &variables
    ));
    assert!(!oracle::envelope(
        &json!(9007199254740993u64),
        &json!(9007199254740992u64),
        &none
    ));
    assert!(oracle::envelope(&json!(1000), &json!(1e3), &none));
    assert!(oracle::envelope(&json!(0), &json!(-0.0), &none));
    assert!(oracle::envelope(
        &json!(1.0),
        &json!(null),
        &BTreeMap::from([("".into(), oracle::Variable::Integer)])
    ));
    for (kind, good, bad) in [
        (oracle::Variable::Number, json!(1.5), json!("1.5")),
        (oracle::Variable::Integer, json!(42), json!(1.5)),
        (oracle::Variable::Boolean, json!(true), json!(1)),
        (oracle::Variable::Object, json!({}), json!([])),
        (oracle::Variable::Array, json!([]), json!({})),
        (oracle::Variable::Null, json!(null), json!(false)),
        (
            oracle::Variable::Timestamp,
            json!("2026-11-19T12:00:00.001Z"),
            json!("2026-02-30T12:00:00Z"),
        ),
    ] {
        let variable = BTreeMap::from([("".into(), kind)]);
        assert!(oracle::envelope(&good, &json!(null), &variable));
        assert!(!oracle::envelope(&bad, &json!(null), &variable));
    }
}

#[test]
fn log_oracle_strips_only_declared_prefixes_and_preserves_order() {
    let expected = vec!["started".into(), "finished".into()];
    let actual = vec![
        "2026-11-19T12:00:00Z stdout: started".into(),
        "2026-11-19T12:00:01Z stdout: finished".into(),
    ];
    let prefixes = [
        oracle::Prefix::Timestamp,
        oracle::Prefix::Literal("stdout: "),
    ];
    assert!(oracle::logs(&actual, &expected, &prefixes));
    assert!(!oracle::logs(&actual, &expected, &[]));
    assert!(!oracle::logs(&actual[..1], &expected, &prefixes));
    assert!(!oracle::logs(
        &[actual[1].clone(), actual[0].clone()],
        &expected,
        &prefixes
    ));
    assert!(!oracle::logs(
        &["stderr: started".into(), "finished".into()],
        &expected,
        &prefixes
    ));
    assert!(!oracle::logs(
        &[" started".into(), "finished".into()],
        &expected,
        &prefixes
    ));
}

#[test]
fn javascript_pins_match_the_operator_table_and_leave_the_profile_disabled() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("qualification/javascript/versions.toml");
    let pins: serde_json::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        pins,
        serde_json::json!({
            "jco": {"name":"@bytecodealliance/jco", "version":"1.41.0", "source":"https://registry.npmjs.org/@bytecodealliance/jco/-/jco-1.41.0.tgz", "archive":"jco-1.41.0.tgz", "sha256":"abe7ab13d0fb1b6ecc6f7d3fffd6ff27862d158936a11d6f48bcb0ada00af4c6", "integrity":"sha512-hsl9PEj5kOQLMsUhMqaho6yYooQ8SqULjPCuUuUSXu0D3huVQKFwmbUH0B9JTBpLPZPuJJNfnpKnZx6rV53VOA=="},
            "componentize_js": {"name":"@bytecodealliance/componentize-js", "version":"0.23.0", "source":"https://registry.npmjs.org/@bytecodealliance/componentize-js/-/componentize-js-0.23.0.tgz", "archive":"componentize-js-0.23.0.tgz", "sha256":"b2e4b6fdee055805370950f127c21d7c549dcc7a1e907f20c5ee769ebba7ad10", "integrity":"sha512-Qc2+lrwJHU7lq4CBMyVVYE8ASyUFeegMYdRnMVG0wNpNiATzGDF/mWpjDNTeVZeT8L9chhK3vuLaQE9FVANvfw=="},
            "wasi_virt": {"name":"wasi-virt", "version":"0.2.0", "source":"https://github.com/bytecodealliance/WASI-Virt", "commit":"bca61c27499d2c290b5431cfcff013737b3ba01d"},
            "node": {"name":"Node.js", "version":"v26.13.0", "source":"https://nodejs.org/dist/v26.13.0/node-v26.13.0-linux-x64.tar.xz", "archive":"node-v26.13.0-linux-x64.tar.xz", "sha256":"d28a96af6c38596f6d9d97c0ee531c13d238d757742387d1928de822d959e997"}
        })
    );
    let matrix: serde_json::Value = serde_json::from_str(include_str!(
        "../../docs/contracts/language-profile-matrix.json"
    ))
    .unwrap();
    assert_eq!(matrix["matrixVersion"], "1");
    assert_eq!(
        matrix["profiles"]["javascript"]["toolchainVersion"],
        "1.41.0"
    );
    assert_eq!(matrix["profiles"]["javascript"]["status"], "disabled");
}

#[test]
fn handler_failure_and_logs_match_the_inventory_through_real_host_execution() {
    let fixture = wat::parse_str(include_str!("fixtures/legacy/mapped-failure.wat")).unwrap();
    let (status, actual) =
        runtime::execute(&request(), &fixture, std::sync::Arc::new(runtime::Denied));
    let inventory: serde_json::Value = serde_json::from_slice(include_bytes!(
        "fixtures/contract/result/valid/failure-execution-failed.json"
    ))
    .unwrap();
    assert_eq!(status, 500);
    assert!(
        oracle::envelope(
            &actual,
            &inventory["result"],
            &std::collections::BTreeMap::new()
        ),
        "{actual}"
    );
}

#[test]
fn synthesized_legacy_sources_have_no_static_blockers() {
    for source in [
        include_str!("fixtures/legacy/success/function.js"),
        include_str!("fixtures/legacy/throw/function.js"),
        include_str!("fixtures/legacy/reject/function.js"),
        include_str!("fixtures/legacy/identity/function.js"),
        include_str!("fixtures/legacy/forged-tenant/function.js"),
        include_str!("fixtures/legacy/forged-workspace/function.js"),
        include_str!("fixtures/legacy/guessed-handle/function.js"),
        include_str!("fixtures/legacy/inventory-main.js"),
    ] {
        let verdict = check(source, include_bytes!("fixtures/legacy/package.json"));
        assert!(verdict.is_supported(), "{:?}", verdict.diagnostics());
    }
}

#[test]
fn fixture_build_refuses_a_toolchain_hash_mismatch_before_running_tools() {
    let directory = std::env::temp_dir().join(format!("legacy-hash-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("jco-1.41.0.tgz"),
        b"corrupt synthetic archive",
    )
    .unwrap();
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy/build.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg("--verify-only")
        .env("FS_TOOLCHAIN_CACHE", &directory)
        .env_remove("FS_JS_TOOLCHAIN_ROOT")
        .output()
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "toolchain sha256 mismatch: jco\n"
    );
}

fn built_manifest() -> (std::path::PathBuf, Vec<serde_json::Value>) {
    assert!(
        std::env::var_os("FS_TOOLCHAIN_CACHE").is_some(),
        "UNVERIFIED: FS_TOOLCHAIN_CACHE unset"
    );
    let root = std::path::PathBuf::from(
        std::env::var_os("FS_LEGACY_COMPONENTS").expect("UNVERIFIED: FS_LEGACY_COMPONENTS unset"),
    );
    let manifest: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let names: std::collections::BTreeSet<_> = manifest
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "success",
            "throw",
            "reject",
            "identity",
            "forged-tenant",
            "forged-workspace",
            "guessed-handle",
            "inventory-401",
            "inventory-400",
            "inventory-429",
            "inventory-404",
            "inventory-403",
            "inventory-304",
            "inventory-200-json",
            "inventory-200-yaml"
        ]
        .into()
    );
    assert_eq!(manifest.len(), names.len());
    (root, manifest)
}

#[test]
#[ignore = "UNVERIFIED: operator-built components and FS_TOOLCHAIN_CACHE required"]
fn built_components_pass_import_gate() {
    use sha2::{Digest, Sha256};
    let (root, manifest) = built_manifest();
    for record in manifest {
        let directory = root.join(record["name"].as_str().unwrap());
        let bytes = std::fs::read(directory.join("component.wasm")).unwrap();
        assert_eq!(
            record["sha256"],
            format!("sha256:{:x}", Sha256::digest(&bytes))
        );
        falcone_serverless_worker::artifact::validate::validate(&bytes, 64 * 1024 * 1024).unwrap();
        let request =
            api::parse_request(&std::fs::read(directory.join("request.json")).unwrap()).unwrap();
        assert_eq!(
            request.plan().artifact_digest,
            record["sha256"].as_str().unwrap()
        );
        let expected = std::fs::read(directory.join("expected.json")).unwrap();
        api::parse_result(&expected, &request).unwrap();
    }
}

#[test]
#[ignore = "UNVERIFIED: operator-built components and FS_TOOLCHAIN_CACHE required"]
fn built_legacy_fixtures() {
    use falcone_serverless_worker::host;
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Mutex};
    struct Recording(Mutex<Vec<(String, String)>>);
    impl host::Bindings for Recording {
        fn call(
            &self,
            context: &host::Context,
            operation: host::Operation,
        ) -> Result<host::Reply, host::HostError> {
            match operation {
                host::Operation::Publish { destination, .. } if destination == "orders" => {
                    self.0
                        .lock()
                        .unwrap()
                        .push((context.tenant.clone(), context.workspace.clone()));
                    Ok(host::Reply::Empty)
                }
                _ => Err(host::HostError::Denied),
            }
        }
    }
    let (root, manifest) = built_manifest();
    for record in manifest {
        let name = record["name"].as_str().unwrap();
        let directory = root.join(name);
        let bytes = std::fs::read(directory.join("component.wasm")).unwrap();
        assert_eq!(
            record["sha256"],
            format!("sha256:{:x}", Sha256::digest(&bytes))
        );
        let request =
            api::parse_request(&std::fs::read(directory.join("request.json")).unwrap()).unwrap();
        let verdict = check(
            &std::fs::read_to_string(directory.join("legacy.js")).unwrap(),
            b"{}",
        );
        assert!(
            verdict.is_supported(),
            "{name}: {:?}",
            verdict.diagnostics()
        );
        let binding = Arc::new(Recording(Mutex::new(Vec::new())));
        let (status, actual) =
            runtime::execute_checked(&request, &bytes, binding.clone(), &verdict);
        let expected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("expected.json")).unwrap())
                .unwrap();
        assert_eq!(serde_json::json!(status), record["http_status"], "{name}");
        api::parse_result(&serde_json::to_vec(&actual).unwrap(), &request).unwrap();
        assert!(
            oracle::envelope(&actual, &expected, &std::collections::BTreeMap::new()),
            "{name}: {actual}"
        );
        let actual_logs: Vec<String> = serde_json::from_value(actual["logs"].clone()).unwrap();
        let expected_logs: Vec<String> = serde_json::from_value(expected["logs"].clone()).unwrap();
        assert!(oracle::logs(&actual_logs, &expected_logs, &[]));
        if name == "identity" {
            let context = request.plan().context;
            assert_eq!(
                actual["result"]["body"]["invocation_key"],
                context.invocation_key
            );
            assert_eq!(actual["result"]["body"]["attempt_id"], context.attempt_id);
            assert_eq!(
                actual["result"]["body"]["idempotency_key"],
                context.idempotency_key
            );
            assert_eq!(
                *binding.0.lock().unwrap(),
                [(context.tenant, context.workspace)]
            );
        } else {
            assert!(binding.0.lock().unwrap().is_empty());
        }
    }
    // Context rejection is part of the same operator-run suite, before any instantiation.
    context_refusals_precede_every_guest_start_and_do_not_echo_inputs();
    disabled_and_blocked_functions_never_reach_the_instantiation_boundary();
    println!(
        "{}",
        serde_json::json!({"check":"legacy_fixtures","status":"passed"})
    );
}
