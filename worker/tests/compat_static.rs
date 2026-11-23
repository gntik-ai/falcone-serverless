use falcone_serverless_worker::adapters::compat::{Source, check};
use falcone_serverless_worker::{
    adapters::{Registry, javascript},
    api::{self, ExecuteRequest},
};
use serde_json::{Value, json};
#[path = "support/legacy_context.rs"]
mod legacy_context;
#[path = "support/legacy_oracle.rs"]
mod oracle;

#[test]
fn oracle_compares_json_values_and_only_declared_variations() {
    use oracle::{Prefix, Variable, compare};
    assert!(
        compare(
            br#"{ "b": [1.0, 2e0], "a": 1000 }"#,
            br#"{"a":1e3,"b":[1,2]}"#,
            &[],
            &[]
        )
        .is_ok()
    );
    for actual in [
        br#"{"b":[2,1],"a":1000}"#.as_slice(),
        br#"{"b":[1,2]}"#,
        br#"{"b":[1,2],"a":1000,"extra":null}"#,
    ] {
        assert!(compare(actual, br#"{"a":1000,"b":[1,2]}"#, &[], &[]).is_err());
    }
    assert!(compare(b"9007199254740993", b"9007199254740992", &[], &[]).is_err());
    let fields = [
        Variable {
            pointer: "/activation",
            schema: json!({"type":"string"}),
        },
        Variable {
            pointer: "/time",
            schema: json!({"type":"string","format":"date-time"}),
        },
    ];
    let expected = br#"{"activation":"synthetic","time":"2026-11-19T00:00:00Z"}"#;
    assert!(
        compare(
            br#"{"activation":"another","time":"2026-11-20T01:02:03Z"}"#,
            expected,
            &fields,
            &[]
        )
        .is_ok()
    );
    for actual in [
        br#"{"activation":1,"time":"2026-11-20T01:02:03Z"}"#.as_slice(),
        br#"{"activation":"another","time":"not-a-time"}"#,
        br#"{"time":"2026-11-20T01:02:03Z"}"#,
    ] {
        assert!(compare(actual, expected, &fields, &[]).is_err());
    }
    let expected =
        br#"{"logs":["2026-11-19T00:00:00Z stdout: first","2026-11-19T00:00:00Z stdout: second"]}"#;
    let actual =
        br#"{"logs":["2026-11-20T00:00:00Z stdout: first","2026-11-20T00:00:00Z stdout: second"]}"#;
    assert!(
        compare(
            actual,
            expected,
            &[],
            &[Prefix::Timestamp, Prefix::Stream("stdout: ")]
        )
        .is_ok()
    );
    assert!(compare(actual, expected, &[], &[]).is_err());
    for actual in [br#"{"logs":["2026-11-20T00:00:00Z stdout: second","2026-11-20T00:00:00Z stdout: first"]}"#.as_slice(), br#"{"logs":["2026-11-20T00:00:00Z stdout: first"]}"#, br#"{"logs":["2026-11-20T00:00:00Z stdout:  first","2026-11-20T00:00:00Z stdout: second"]}"#] {
        assert!(compare(actual, expected, &[], &[Prefix::Timestamp, Prefix::Stream("stdout: ")]).is_err());
    }
}

#[test]
fn oracle_preserves_decimal_precision_and_enforces_format_only_declarations() {
    assert!(oracle::compare(b"9007199254740993.0", b"9007199254740993", &[], &[]).is_ok());
    assert!(
        oracle::compare(
            b"0.123456789012345678901",
            b"0.123456789012345678902",
            &[],
            &[]
        )
        .is_err()
    );
    for (actual, expected, equal) in [
        (
            r#"{"1":"9.0","a/b":[-0.0,1.2300e2]}"#,
            r#"{"a/b":[0,123],"1":"9.0"}"#,
            true,
        ),
        (
            r#"{"escaped":"\"1.2\\3","n":1e+3}"#,
            r#"{"n":1000.0,"escaped":"\"1.2\\3"}"#,
            true,
        ),
        (r#"{"n":"1.0"}"#, r#"{"n":1}"#, false),
        (r#"{"n":-1.0}"#, r#"{"n":1}"#, false),
        (
            r#"{"n":0.123456789012345678901}"#,
            r#"{"n":0.123456789012345678902}"#,
            false,
        ),
    ] {
        assert_eq!(
            oracle::compare(actual.as_bytes(), expected.as_bytes(), &[], &[]).is_ok(),
            equal
        );
    }
    let variable = [oracle::Variable {
        pointer: "/time",
        schema: json!({"format":"date-time"}),
    }];
    assert!(
        oracle::compare(
            br#"{"time":42}"#,
            br#"{"time":"2026-11-19T00:00:00Z"}"#,
            &variable,
            &[]
        )
        .is_err()
    );
    let unknown = [oracle::Variable {
        pointer: "/time",
        schema: json!({"format":"not-an-inventory-format"}),
    }];
    assert!(
        oracle::compare(
            br#"{"time":"anything"}"#,
            br#"{"time":"anything"}"#,
            &unknown,
            &[]
        )
        .is_err()
    );
}

fn request() -> ExecuteRequest {
    let mut input: Value = serde_json::from_str(include_str!(
        "fixtures/contract/request/valid/wasm-no-base.json"
    ))
    .unwrap();
    input["params"]["tenant"] = json!("forged-tenant");
    input["params"]["workspace"] = json!("forged-workspace");
    input["params"]["invocation_key"] = json!("forged-invocation");
    input["params"]["__ow_headers"] = json!({"x-auth-tenant-id":"forged-tenant","x-workspace-id":"forged-workspace","x-invocation-key":"forged-invocation"});
    api::parse_request(&serde_json::to_vec(&input).unwrap()).unwrap()
}

#[test]
fn disabled_registry_refuses_supported_sources_before_instantiation() {
    let mut wire = serde_json::to_value(request()).unwrap();
    wire["projection"]["language_profile"] =
        json!({"name":"javascript-wasi-v1","version":"1.41.0"});
    let request = api::parse_request(&serde_json::to_vec(&wire).unwrap()).unwrap();
    let context = request.plan().context;
    let starts = std::cell::Cell::new(0);
    let error = Registry
        .invoke(
            &request,
            Some(&context),
            include_str!("fixtures/legacy/success.js"),
            "{}",
            |_| {
                starts.set(starts.get() + 1);
                javascript::Outcome {
                    result: Ok(b"null".to_vec()),
                    logs: vec![],
                }
            },
        )
        .unwrap_err();
    assert_eq!(starts.get(), 0);
    assert_eq!(error.refusal.code(), api::Code::ExecutionFailed);
    assert_eq!(error.message, "javascript profile disabled");
    let (status, response) = error.response(&request);
    assert_eq!(status, 500);
    api::parse_result(&response, &request).unwrap();
    let response: Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(response["status"], "failure");
    assert_eq!(response["result"]["detail"]["reason"], "artifact_rejected");
}

#[test]
fn registry_refuses_mismatched_profile_pins_before_instantiation() {
    for (name, version) in [
        ("other-profile", "1.41.0"),
        ("javascript-wasi-v1", "unqualified"),
        ("other-profile", "unqualified"),
    ] {
        let mut wire = serde_json::to_value(request()).unwrap();
        wire["projection"]["language_profile"] = json!({"name":name,"version":version});
        let request = api::parse_request(&serde_json::to_vec(&wire).unwrap()).unwrap();
        let context = request.plan().context;
        let error = Registry
            .invoke(
                &request,
                Some(&context),
                include_str!("fixtures/legacy/success.js"),
                "{}",
                |_| panic!("an unpinned profile must not start a guest"),
            )
            .unwrap_err();
        assert_eq!(error.message, "javascript qualification missing");
        let (status, response) = error.response(&request);
        assert_eq!(status, 500);
        api::parse_result(&response, &request).unwrap();
        let response: Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["status"], "failure");
        assert_eq!(response["result"]["detail"]["reason"], "artifact_rejected");
    }
}

#[test]
fn spoofed_params_and_headers_cannot_supply_the_handler_identity() {
    let request = request();
    let context = request.plan().context;
    let (_, response) = javascript::qualify(&request, Some(&context), include_str!("fixtures/legacy/identity.js"), "{}", |input| {
        assert_eq!(input.context.tenant, "tenant-synthetic-0001");
        assert_eq!(input.context.workspace, "ws-synthetic-0001");
        assert_eq!(input.context.invocation_key, "inv-0000007");
        assert_eq!(input.context.attempt_id, "att-synthetic-0001");
        assert_eq!(input.context.idempotency_key, "idem-0000007");
        let wire: Value = serde_json::from_slice(&input.wire().unwrap()).unwrap();
        assert_eq!(wire["context"], json!({"tenant":"tenant-synthetic-0001","workspace":"ws-synthetic-0001","invocation_key":"inv-0000007","attempt_id":"att-synthetic-0001","idempotency_key":"idem-0000007"}));
        assert_eq!(wire["params"]["tenant"], "forged-tenant");
        assert!(wire.get("credential").is_none());
        javascript::Outcome { result: Ok(serde_json::to_vec(&json!({"tenant":input.context.tenant,"workspace":input.context.workspace,"invocation_key":input.context.invocation_key})).unwrap()), logs: vec![] }
    }).unwrap();
    let value: Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(value["status"], "success");
    assert_eq!(
        value["result"]["body"],
        json!({"tenant":"tenant-synthetic-0001","workspace":"ws-synthetic-0001","invocation_key":"inv-0000007"})
    );
}

#[test]
fn context_absence_and_mismatch_refuse_before_any_instance_and_echo_no_values() {
    legacy_context::assert_rejected_contexts(&request(), |request, context| {
        Registry
            .invoke(
                request,
                context,
                include_str!("fixtures/legacy/success.js"),
                "{}",
                |_| panic!("guest must not start"),
            )
            .unwrap_err()
    });
}

#[test]
fn blocked_source_never_instantiates_even_during_qualification() {
    let request = request();
    let context = request.plan().context;
    let error = javascript::qualify(&request, Some(&context), "require('node:fs')", "{}", |_| {
        panic!("guest must not start")
    })
    .unwrap_err();
    assert_eq!(error.message, "javascript source blocked");
    let (status, response) = error.response(&request);
    assert_eq!(status, 500);
    api::parse_result(&response, &request).unwrap();
    let response: Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(response["status"], "failure");
    assert_eq!(response["result"]["detail"]["reason"], "artifact_rejected");
}

#[test]
fn failed_executor_outcomes_match_inventory_without_exception_details() {
    use falcone_serverless_worker::host::{ExecuteError, Execution, outcome::InvocationOutcome};
    let request = request();
    let context = request.plan().context;
    let inventory: Value = serde_json::from_str(include_str!(
        "fixtures/contract/result/valid/failure-execution-failed.json"
    ))
    .unwrap();
    for source in [
        include_str!("fixtures/legacy/throw.js"),
        include_str!("fixtures/legacy/reject.js"),
    ] {
        let (status, response) =
            javascript::qualify(&request, Some(&context), source, "{}", |_| {
                javascript::Outcome::from_execution(
                    Execution {
                        outcome: InvocationOutcome::Failed,
                        stdout_bytes: 0,
                        stdout_truncated: false,
                        result: Err(ExecuteError {
                            code: "handler_failed",
                            message: Some("synthetic private exception".into()),
                        }),
                    },
                    vec!["handler started".into(), "handler finished".into()],
                )
            })
            .unwrap();
        assert_eq!(status, 500);
        oracle::compare(
            &response,
            &serde_json::to_vec(&inventory["result"]).unwrap(),
            &[],
            &[],
        )
        .unwrap();
        assert!(
            !String::from_utf8(response)
                .unwrap()
                .contains("private exception")
        );
    }
}

#[test]
fn adapter_maps_raw_handler_values_to_legacy_results_and_preserves_log_order() {
    let request = request();
    let context = request.plan().context;
    // Raw executor outputs are independent of the expected legacy envelopes. Actual JS
    // wrapper execution remains gated by the trusted-image components in legacy_runtime.
    for (raw, expected) in [
        ("null", r#"{"statusCode":200}"#),
        ("true", r#"{"statusCode":200,"body":true}"#),
        ("[2,1]", r#"{"statusCode":200,"body":[2,1]}"#),
        (r#"{"ok":true}"#, r#"{"statusCode":200,"body":{"ok":true}}"#),
        (
            r#"{"body":null,"headers":null,"statusCode":304}"#,
            r#"{"statusCode":304}"#,
        ),
    ] {
        let (status, response) = javascript::qualify(
            &request,
            Some(&context),
            include_str!("fixtures/legacy/success.js"),
            "{}",
            |_| javascript::Outcome {
                result: Ok(raw.as_bytes().to_vec()),
                logs: vec!["first".into(), "second".into()],
            },
        )
        .unwrap();
        assert_eq!(status, 200);
        api::parse_result(&response, &request).unwrap();
        let response: Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["status"], "success");
        assert_eq!(response["logs"], json!(["first", "second"]));
        oracle::compare(
            &serde_json::to_vec(&response["result"]).unwrap(),
            expected.as_bytes(),
            &[],
            &[],
        )
        .unwrap();
    }
}

#[test]
fn adapter_preserves_inventoried_legacy_shaped_results_and_ordered_logs() {
    let request = request();
    let context = request.plan().context;
    // Independently specified executor outputs, never the inventory's expected value.
    // Null optional fields and reordered fields exercise the adapter's normalization.
    for (name, raw) in [
        (
            "200-json",
            r#"{"body":"{\"openapi\":\"3.0.3\",\"info\":{\"title\":\"Synthetic workspace API\",\"version\":\"1.0.0\"},\"paths\":{}}","headers":{"Cache-Control":"max-age=60, must-revalidate","X-Spec-Version":"1.0.0","ETag":"\"sha256:002ce259e8f4f989d9fb469c3023a00f2544a739fe6971a1ea1bb7b7009d9a32\"","Content-Type":"application/json"},"statusCode":200}"#,
        ),
        (
            "200-yaml",
            r#"{"body":"openapi: 3.0.3\ninfo:\n  title: Synthetic workspace API\n  version: 1.0.0\npaths: {}\n","headers":{"Cache-Control":"max-age=60, must-revalidate","X-Spec-Version":"1.0.0","ETag":"\"sha256:002ce259e8f4f989d9fb469c3023a00f2544a739fe6971a1ea1bb7b7009d9a32\"","Content-Type":"application/x-yaml"},"statusCode":200}"#,
        ),
        (
            "304",
            r#"{"body":null,"headers":{"ETag":"\"sha256:002ce259e8f4f989d9fb469c3023a00f2544a739fe6971a1ea1bb7b7009d9a32\""},"statusCode":304}"#,
        ),
        (
            "400",
            r#"{"headers":null,"body":{"message":"workspaceId missing from path","code":"INVALID_PATH"},"statusCode":400}"#,
        ),
        (
            "401",
            r#"{"headers":null,"body":{"message":"Authentication required","code":"UNAUTHORIZED"},"statusCode":401}"#,
        ),
        (
            "403",
            r#"{"headers":null,"body":{"message":"Workspace tenant mismatch","code":"FORBIDDEN"},"statusCode":403}"#,
        ),
        (
            "404",
            r#"{"headers":null,"body":{"message":"No OpenAPI spec available for workspace","code":"SPEC_NOT_FOUND"},"statusCode":404}"#,
        ),
        (
            "429",
            r#"{"headers":{"Retry-After":"60"},"body":{"message":"Too many requests","code":"RATE_LIMITED"},"statusCode":429}"#,
        ),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../docs/contracts/fixtures/legacy-envelope/valid/{name}.json"
        ));
        let inventory: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let expected = serde_json::to_vec(&inventory["expected"]).unwrap();
        let (status, response) = javascript::qualify(
            &request,
            Some(&context),
            include_str!("fixtures/legacy/success.js"),
            "{}",
            |_| javascript::Outcome {
                result: Ok(raw.as_bytes().to_vec()),
                logs: vec!["first".into(), "second".into()],
            },
        )
        .unwrap();
        assert_eq!(status, 200);
        let actual: Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(actual["status"], "success");
        assert_eq!(actual["logs"], json!(["first", "second"]));
        oracle::compare(
            &serde_json::to_vec(&actual["result"]).unwrap(),
            &expected,
            &[],
            &[],
        )
        .unwrap();
    }
}

#[test]
fn toolchain_pins_record_the_operator_decision_and_keep_javascript_disabled() {
    let versions: toml::Value =
        toml::from_str(include_str!("../qualification/javascript/versions.toml")).unwrap();
    for (name, package, version, url, digest) in [
        (
            "jco",
            "@bytecodealliance/jco",
            "1.41.0",
            "https://registry.npmjs.org/@bytecodealliance/jco/-/jco-1.41.0.tgz",
            "abe7ab13d0fb1b6ecc6f7d3fffd6ff27862d158936a11d6f48bcb0ada00af4c6",
        ),
        (
            "componentize_js",
            "@bytecodealliance/componentize-js",
            "0.23.0",
            "https://registry.npmjs.org/@bytecodealliance/componentize-js/-/componentize-js-0.23.0.tgz",
            "b2e4b6fdee055805370950f127c21d7c549dcc7a1e907f20c5ee769ebba7ad10",
        ),
        (
            "node",
            "Node.js",
            "v26.13.0",
            "https://nodejs.org/dist/v26.13.0/node-v26.13.0-linux-x64.tar.xz",
            "d28a96af6c38596f6d9d97c0ee531c13d238d757742387d1928de822d959e997",
        ),
    ] {
        assert_eq!(versions[name]["name"].as_str(), Some(package));
        assert_eq!(versions[name]["version"].as_str(), Some(version));
        assert_eq!(versions[name]["source"].as_str(), Some(url));
        assert_eq!(versions[name]["sha256"].as_str(), Some(digest));
    }
    assert_eq!(versions["wasi_virt"]["version"].as_str(), Some("0.2.0"));
    assert_eq!(
        versions["wasi_virt"]["source"].as_str(),
        Some("https://github.com/bytecodealliance/WASI-Virt")
    );
    assert_eq!(
        versions["wasi_virt"]["rev"].as_str(),
        Some("bca61c27499d2c290b5431cfcff013737b3ba01d")
    );
    let matrix: Value =
        serde_json::from_str(include_str!("../src/adapters/language-profile-matrix.json")).unwrap();
    assert_eq!(matrix["profiles"]["javascript"]["status"], "disabled");
    assert_eq!(
        matrix["profiles"]["javascript"]["toolchainVersion"],
        "1.41.0"
    );
}

#[test]
fn literal_main_is_supported_and_unsupported_constructs_are_located() {
    assert!(
        check(
            &[Source {
                text: "function main(params) { return {body: params.value}; }"
            }],
            "{}"
        )
        .supported()
    );
    let verdict = check(
        &[Source {
            text: "\nrequire('child_process');",
        }],
        "{}",
    );
    assert!(!verdict.supported());
    assert_eq!(verdict.diagnostics()[0].code, "WASM-PROCESS-SPAWN");
    assert_eq!(verdict.diagnostics()[0].construct, "child_process");
    assert_eq!(verdict.diagnostics()[0].line, 2);
    assert_eq!(verdict.diagnostics()[0].column, 9);
}

#[test]
fn line_comments_cannot_hide_executable_code_after_javascript_line_separators() {
    for separator in ["\r", "\u{2028}", "\u{2029}"] {
        let text = format!(
            "function main(params) {{ return null; }} // synthetic{separator}process.dlopen(params);"
        );
        let verdict = check(&[Source { text: &text }], "{}");
        assert!(!verdict.supported());
        assert_eq!(
            (
                verdict.diagnostics()[0].line,
                verdict.diagnostics()[0].column
            ),
            (2, 1)
        );
    }
}

#[test]
fn restricted_productions_do_not_ignore_javascript_automatic_semicolon_insertion() {
    for text in [
        "async\nfunction main(params) { return null; }",
        "async /* synthetic\ncomment */ function main(params) { return null; }",
        "function main(params) { throw\nnew Error('synthetic'); }",
        "function main(params) { return\n{body: params.value}; }",
    ] {
        assert!(!check(&[Source { text }], "{}").supported(), "{text}");
    }
}

#[test]
fn synthesized_legacy_sources_are_eligible_but_ambiguous_code_is_blocked() {
    for text in [
        include_str!("fixtures/legacy/success.js"),
        include_str!("fixtures/legacy/throw.js"),
        include_str!("fixtures/legacy/reject.js"),
        include_str!("fixtures/legacy/logs.js"),
        include_str!("fixtures/legacy/identity.js"),
        include_str!("fixtures/legacy/denied.js"),
        "async function main(params) { return await crypto.subtle.digest('SHA-256', new Uint8Array([1,2])); }",
        "function main(params) { return crypto.getRandomValues(new Uint8Array(4)); }",
    ] {
        let verdict = check(&[Source { text }], "{}");
        assert!(verdict.supported(), "{verdict:?}");
    }
    for text in [
        "function main(params) { return globalThis; }",
        "function main(params) { return params.constructor('hidden')(); }",
        "function main(params) { return params['constructor']; }",
        "function main(params) { return crypto.randomUUID(); }",
        "function main(params) { return crypto.subtle.encrypt(params); }",
        "function main(params) { console.log(params.value); return null; }",
        "function main(params) { console.log({value: 1}); return null; }",
        "function main(params) { console.log('%s', 'value'); return null; }",
        "function main(params) { return `hidden`; }",
        "function main(params) { return /hidden/; }",
        "function main(params) { return 1; } /* unclosed",
        "function main(params) { return 1; } require(params.module)",
        "function main(params) { return 1; } eval('hidden')",
        "function main(params) { return \\u0065val; }",
        "not javascript",
        "",
    ] {
        assert!(!check(&[Source { text }], "{}").supported(), "{text}");
    }
    assert!(!check(&[], "{}").supported());
    assert!(
        !check(
            &[Source {
                text: "function main(params) { return null; }"
            }],
            "not JSON"
        )
        .supported()
    );
}

#[test]
fn diagnostics_cover_the_operator_taxonomy_without_selecting_another_mode() {
    for (source, manifest, code, construct, column) in [
        (
            "require('addon.node')",
            "{}",
            "WASM-NATIVE-LIBRARY",
            ".node",
            9,
        ),
        (
            "process.dlopen(x)",
            "{}",
            "WASM-NATIVE-LIBRARY",
            "process.dlopen",
            1,
        ),
        (
            "require('worker_threads')",
            "{}",
            "WASM-PROCESS-SPAWN",
            "worker_threads",
            9,
        ),
        (
            "import x from 'cluster'",
            "{}",
            "WASM-PROCESS-SPAWN",
            "cluster",
            15,
        ),
        ("navigator.gpu", "{}", "WASM-GPU", "navigator.gpu", 1),
        (
            "",
            r#"{"scripts":{"postinstall":"synthetic"}}"#,
            "WASM-INSTALL-HOOK",
            "lifecycle script",
            2,
        ),
        (
            "require('unknown-package')",
            "{}",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            "package import",
            9,
        ),
        (
            "",
            r#"{"dependencies":{"some-package":"1.0.0"}}"#,
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            "package dependency",
            2,
        ),
        (
            "require('node:fs')",
            "{}",
            "WASM-UNSUPPORTED-API",
            "node module",
            9,
        ),
        (
            "require('node:crypto').randomBytes(4)",
            "{}",
            "WASM-UNSUPPORTED-API",
            "node:crypto API",
            9,
        ),
        ("eval('hidden')", "{}", "WASM-UNSUPPORTED-API", "eval", 1),
        (
            "new Function('hidden')",
            "{}",
            "WASM-UNSUPPORTED-API",
            "new Function",
            1,
        ),
        (
            "require(params.module)",
            "{}",
            "WASM-UNSUPPORTED-API",
            "dynamic require",
            1,
        ),
        (
            "import(params.module)",
            "{}",
            "WASM-UNSUPPORTED-API",
            "dynamic import",
            1,
        ),
    ] {
        let verdict = check(&[Source { text: source }], manifest);
        assert!(!verdict.supported(), "{source}");
        let diagnostic = &verdict.diagnostics()[0];
        assert_eq!(
            (
                diagnostic.code,
                diagnostic.construct,
                diagnostic.source,
                diagnostic.line,
                diagnostic.column
            ),
            (code, construct, usize::from(manifest != "{}"), 1, column),
            "{source}"
        );
        assert!(!format!("{verdict:?}").to_lowercase().contains("oci"));
        assert!(!format!("{verdict:?}").contains("hidden"));
    }
}

#[test]
fn non_literal_module_specifiers_are_dynamic_loading_not_package_imports() {
    for (expression, construct) in [
        ("require('lodash' + params.x)", "dynamic require"),
        ("import('lodash' + params.x)", "dynamic import"),
        ("require('child_' + 'process')", "dynamic require"),
        ("import('child_' + 'process')", "dynamic import"),
        ("require('a', params.b)", "dynamic require"),
        ("import('a', params.b)", "dynamic import"),
        ("require('node:fs' + params.x)", "dynamic require"),
        ("import('child_process', params.b)", "dynamic import"),
    ] {
        let source = format!("\n  {expression}");
        let verdict = check(&[Source { text: &source }], "{}");
        assert!(!verdict.supported(), "{expression}");
        let diagnostics: Vec<_> = verdict
            .diagnostics()
            .iter()
            .map(|item| {
                (
                    item.code,
                    item.construct,
                    item.source,
                    item.line,
                    item.column,
                )
            })
            .collect();
        assert_eq!(
            diagnostics,
            [("WASM-UNSUPPORTED-API", construct, 0, 2, 3)],
            "{expression}"
        );
        assert!(!format!("{verdict:?}").to_lowercase().contains("oci"));
    }
}

#[test]
fn feature_diagnostics_survive_ordinary_operators() {
    for (expression, code, construct, column) in [
        ("require('addon.node')", "WASM-NATIVE-LIBRARY", ".node", 9),
        (
            "process.dlopen(x)",
            "WASM-NATIVE-LIBRARY",
            "process.dlopen",
            1,
        ),
        (
            "require('child_process')",
            "WASM-PROCESS-SPAWN",
            "child_process",
            9,
        ),
        (
            "require('worker_threads')",
            "WASM-PROCESS-SPAWN",
            "worker_threads",
            9,
        ),
        ("require('cluster')", "WASM-PROCESS-SPAWN", "cluster", 9),
        ("navigator.gpu", "WASM-GPU", "navigator.gpu", 1),
        (
            "require('unknown-package')",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            "package import",
            9,
        ),
        (
            "require('node:fs')",
            "WASM-UNSUPPORTED-API",
            "node module",
            9,
        ),
        (
            "require('node:crypto').randomBytes(4)",
            "WASM-UNSUPPORTED-API",
            "node:crypto API",
            9,
        ),
        ("eval(params.code)", "WASM-UNSUPPORTED-API", "eval", 1),
        (
            "new Function(params.code)",
            "WASM-UNSUPPORTED-API",
            "new Function",
            1,
        ),
        (
            "require(params.module)",
            "WASM-UNSUPPORTED-API",
            "dynamic require",
            1,
        ),
        (
            "import(params.module)",
            "WASM-UNSUPPORTED-API",
            "dynamic import",
            1,
        ),
    ] {
        for ordinary in [
            "const value = 'ls ' + params.x;",
            "const f = (x) => x;",
            "if (!params.addon) {}",
            "const value = params.a > params.b;",
            "const value = params.a || 1;",
            "const value = 1+2;",
            "const value = 1-2;",
            "const value = params.a * 2 / 3 % 4;",
        ] {
            // Exercise operators both before and after the feature, with literal locations.
            for (source, line) in [
                (format!("{expression};\n{ordinary}"), 1),
                (format!("{ordinary}\n{expression};"), 2),
            ] {
                let verdict = check(&[Source { text: &source }], "{}");
                assert!(!verdict.supported());
                assert!(
                    verdict.diagnostics().iter().any(|diagnostic| {
                        (
                            diagnostic.code,
                            diagnostic.construct,
                            diagnostic.source,
                            diagnostic.line,
                            diagnostic.column,
                        ) == (code, construct, 0, line, column)
                    }),
                    "{source}: {verdict:?}"
                );
                assert!(!format!("{verdict:?}").to_lowercase().contains("oci"));
            }
        }
    }
    // Tokenizing operators for diagnostics must not qualify any additional expressions.
    for expression in [
        "params.a + 1",
        "!params.a",
        "params.a > 1",
        "params.a || 1",
        "1+2",
        "1-2",
    ] {
        let source = format!("function main(params) {{ return {expression}; }}");
        assert!(!check(&[Source { text: &source }], "{}").supported());
    }
}

#[test]
fn assigned_unsupported_apis_keep_their_construct_diagnostics() {
    for (expression, code, construct) in [
        (
            "require('child_process')",
            "WASM-PROCESS-SPAWN",
            "child_process",
        ),
        ("require('addon.node')", "WASM-NATIVE-LIBRARY", ".node"),
        ("navigator.gpu", "WASM-GPU", "navigator.gpu"),
        (
            "require('unknown-package')",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            "package import",
        ),
    ] {
        let source = format!("const value =\n{expression};");
        let verdict = check(&[Source { text: &source }], "{}");
        assert!(!verdict.supported());
        let item = &verdict.diagnostics()[0];
        assert_eq!((item.code, item.construct, item.line), (code, construct, 2));
        assert_eq!(
            item.column,
            if expression.starts_with("require") {
                9
            } else {
                1
            }
        );
    }
    // Recognizing an assignment for diagnostics must not make it eligible to execute.
    assert!(
        !check(
            &[Source {
                text: "function main(params) { return params.value = 1; }"
            }],
            "{}"
        )
        .supported()
    );
}

#[test]
fn feature_names_in_result_and_log_literals_are_data() {
    for literal in [
        "child_process",
        "node:worker_threads",
        "cluster",
        "node:crypto",
        "node:fs",
        "addon.node",
    ] {
        let source = format!(
            "function main(params) {{ console.log('{literal}'); return {{body: '{literal}'}}; }}"
        );
        let verdict = check(&[Source { text: &source }], "{}");
        assert!(verdict.supported(), "{literal}: {verdict:?}");
    }
}

#[test]
fn bare_node_builtin_imports_are_unsupported_apis_rather_than_packages() {
    for module in [
        "fs",
        "fs/promises",
        "net",
        "http",
        "http2",
        "dns",
        "os",
        "path",
        "stream/promises",
        "tls",
        "vm",
        "module",
        "util",
    ] {
        for (source, column) in [
            (format!("\nrequire('{module}')"), 9),
            (format!("\nimport item from '{module}'"), 18),
            (format!("\nimport('{module}')"), 8),
        ] {
            let verdict = check(&[Source { text: &source }], "{}");
            assert!(!verdict.supported());
            let diagnostic = &verdict.diagnostics()[0];
            assert_eq!(
                (
                    diagnostic.code,
                    diagnostic.construct,
                    diagnostic.source,
                    diagnostic.line,
                    diagnostic.column
                ),
                ("WASM-UNSUPPORTED-API", "node module", 0, 2, column),
                "{module}"
            );
        }
    }
    // A literal result containing a builtin name is data, not an import.
    assert!(
        check(
            &[Source {
                text: "function main(params) { return 'fs'; }"
            }],
            "{}"
        )
        .supported()
    );
}

#[test]
fn manifest_diagnostics_locate_the_construct_without_echoing_script_values() {
    let manifest = "{\n  \"description\":\"scripts\",\n  \"scr\\u0069pts\":{\"postinstall\":\"private-script-value\"}\n}";
    let verdict = check(
        &[Source {
            text: "function main(params) { return null; }",
        }],
        manifest,
    );
    let item = &verdict.diagnostics()[0];
    assert_eq!(
        (
            item.code,
            item.construct,
            item.source,
            item.line,
            item.column
        ),
        ("WASM-INSTALL-HOOK", "lifecycle script", 1, 3, 3)
    );
    assert!(!format!("{verdict:?}").contains("private-script-value"));
}
