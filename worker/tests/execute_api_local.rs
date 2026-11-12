//! HTTP admission tests; these never compile or instantiate a guest.
#[path = "support/execution_http.rs"]
mod execution_http;
use execution_http::{Service, request};
use serde_json::json;

#[test]
fn http_refuses_non_wasm_before_artifact_access_and_redacts_errors() {
    let service = Service::start(1);
    for mode in ["oci", "unknown"] {
        let mut input = request();
        input["projection"]["runtime_mode"] = json!(mode);
        if mode == "oci" {
            input["projection"]["base"] =
                json!({"kind":"image","image":"registry.invalid/function@sha256:deadbeef"});
        }
        input["credential"]["iss"] = json!("credential-canary-never-log");
        let (status, response) = service.post("/v1/execute", &input);
        assert_eq!(status, 409);
        assert_eq!(response["code"], "runtime_mode_not_supported");
        assert_eq!(response["detail"]["reason"], "runtime_mode_not_wasm");
        assert_eq!(response["retryable"], false);
    }
    let (status, response) =
        service.post("/v1/execute", &json!({"credential-canary-never-log":true}));
    assert_eq!(status, 400);
    assert!(!response.to_string().contains("credential-canary"));
    assert!(!service.logs().contains("credential-canary"));
    assert!(service.logs().contains("worker_http"));
    assert!(!service.root.join("native").exists());
}

#[test]
fn component_imports_must_be_both_declared_and_globally_allowed() {
    use sha2::{Digest, Sha256};
    let service = Service::start(1);
    for (import, declared) in [
        ("falcone:host/invocation-context@0.2.0", vec![]),
        (
            "falcone:host/invocation-context@0.2.0",
            vec!["falcone:host/logging@0.2.0"],
        ),
        (
            "tenant:ambient/escape@0.2.0",
            vec!["tenant:ambient/escape@0.2.0"],
        ),
    ] {
        let source = include_str!("../src/qualification/execute_fixtures/handler.wat")
            .replace(";; IMPORTS", &format!("(import \"{import}\" (instance))"));
        let bytes = wat::parse_str(source).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        std::fs::create_dir_all(service.root.join("components")).unwrap();
        std::fs::write(
            service
                .root
                .join("components")
                .join(format!("{digest}.wasm")),
            bytes,
        )
        .unwrap();
        let mut input = request();
        input["projection"]["component_digest"] = json!(format!("sha256:{digest}"));
        input["projection"]["imports"] = json!(declared);
        let (status, response) = service.post("/v1/execute", &input);
        assert_eq!(status, 422, "{import}");
        assert_eq!(response["code"], "undeclared_import");
        // No trusted compiler/cache is even opened for this rejected component.
        assert!(!service.root.join("native").exists());
    }
}

#[test]
fn spool_integrity_is_checked_before_component_imports() {
    let service = Service::start(1);
    let mut input = request();
    let hex = "a".repeat(64);
    input["projection"]["component_digest"] = json!(format!("sha256:{hex}"));
    std::fs::create_dir_all(service.root.join("components")).unwrap();
    std::fs::write(
        service.root.join("components").join(format!("{hex}.wasm")),
        include_bytes!("../src/qualification/api_fixtures/forbidden.wasm"),
    )
    .unwrap();
    let (status, response) = service.post("/v1/execute", &input);
    assert_eq!(status, 422);
    assert_eq!(response["code"], "invalid_module");
    assert!(!service.root.join("native").exists());
}

#[test]
fn incomplete_execute_headers_cannot_exhaust_cancel_capacity() {
    use execution_http::Wire;
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    struct Paused {
        wire: Wire,
        offset: u64,
        ready: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }
    impl Read for Paused {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.wire.input.position() == self.offset
                && let Some(ready) = self.ready.take()
            {
                ready.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(300)).unwrap();
            }
            self.wire.read(bytes)
        }
    }
    impl Write for Paused {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.wire.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let service = Service::start(1);
    std::thread::scope(|scope| {
        let mut releases = Vec::new();
        let mut threads = Vec::new();
        let mut rejected = 0;
        // One admitted execute at the body boundary, then four incomplete heads.
        for index in 0..5 {
            let Some(connection) = service.inner.connection() else {
                rejected += 1;
                continue;
            };
            let wire = Wire::new("/v1/execute", &json!({}));
            let offset = if index == 0 {
                wire.input.get_ref().len() as u64 - 2
            } else {
                b"POST /v1/execute HTTP/1.1\r\n".len() as u64
            };
            let (ready, waiting) = mpsc::channel();
            let (release, released) = mpsc::channel();
            threads.push(scope.spawn(move || {
                let mut paused = Paused {
                    wire,
                    offset,
                    ready: Some(ready),
                    release: released,
                };
                connection.respond(&mut paused, &mut Vec::new());
            }));
            waiting.recv_timeout(Duration::from_secs(5)).unwrap();
            releases.push(release);
        }
        let mut cancel = Wire::new(
            "/v1/cancel",
            &json!({
                "version":"1", "type":"cancel", "activation_id":"unknown", "attempt_id":"unknown", "cancel":{"state":"requested"}
            }),
        );
        let response = service.inner.control_connection().map(|connection| {
            connection.respond(&mut cancel, &mut Vec::new());
            cancel.response()
        });
        for release in releases {
            release.send(()).unwrap();
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            response.map(|response| response.0),
            Some(200),
            "cancel starved by execute headers"
        );
        assert_eq!(rejected, 4, "execute headers consumed the control reserve");
    });
}

#[test]
fn control_listener_refuses_execute_before_reading_its_headers() {
    use execution_http::Wire;
    use std::io::{Read, Write};

    struct RequestLineOnly(Wire);
    impl Read for RequestLineOnly {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            assert!(
                self.0.input.position() < self.0.input.get_ref().len() as u64,
                "execute waited for headers on the control listener"
            );
            self.0.read(bytes)
        }
    }
    impl Write for RequestLineOnly {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let service = Service::start(1);
    let mut wire = Wire::new("/v1/execute", &request());
    wire.input = std::io::Cursor::new(b"POST /v1/execute HTTP/1.1\r\n".to_vec());
    let mut io = RequestLineOnly(wire);
    service
        .inner
        .control_connection()
        .unwrap()
        .respond(&mut io, &mut Vec::new());
    assert_eq!(io.0.response(), (400, json!({"code":"invalid_request"})));
    assert!(!service.root.join("native").exists());
}

#[test]
fn control_port_is_fixed_without_changing_runtime_config_v1() {
    use falcone_serverless_worker::server::control_port;
    assert_eq!(control_port(8080), Ok(8082));
    assert_eq!(control_port(65533), Ok(65535));
    assert_eq!(control_port(65534), Err("invalid_control_port"));
    assert_eq!(control_port(65535), Err("invalid_control_port"));
}

#[test]
fn execute_capacity_never_consumes_the_control_reserve_even_above_64() {
    use execution_http::Wire;
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    // Pausing at the transport's body boundary exercises real admission without a guest.
    struct Paused {
        wire: Wire,
        body_offset: u64,
        ready: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }
    impl Read for Paused {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.wire.input.position() == self.body_offset
                && let Some(ready) = self.ready.take()
            {
                ready.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(300)).unwrap();
            }
            self.wire.read(bytes)
        }
    }
    impl Write for Paused {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.wire.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for concurrency in [1, 65] {
        let service = Service::start(concurrency);
        std::thread::scope(|scope| {
            let mut releases = Vec::new();
            let mut threads = Vec::new();
            for _ in 0..concurrency {
                let connection = service.inner.connection().unwrap();
                let wire = Wire::new("/v1/execute", &json!({}));
                let offset = wire.input.get_ref().len() as u64 - 2;
                let (ready, waiting) = mpsc::channel();
                let (release, released) = mpsc::channel();
                threads.push(scope.spawn(move || {
                    let mut paused = Paused {
                        wire,
                        body_offset: offset,
                        ready: Some(ready),
                        release: released,
                    };
                    connection.respond(&mut paused, &mut Vec::new());
                    assert_eq!(paused.wire.response().0, 400);
                }));
                waiting.recv_timeout(Duration::from_secs(5)).unwrap();
                releases.push(release);
            }
            let start = Instant::now();
            let (status, response) = service.post("/v1/execute", &json!({}));
            // Always release readers before asserting, including on a regression.
            let cancel = service.post("/v1/cancel", &json!({
                "version":"1", "type":"cancel", "activation_id":"unknown", "attempt_id":"unknown", "cancel":{"state":"requested"}
            }));
            let elapsed = start.elapsed();
            let mut reserve = Vec::new();
            for _ in 0..4 {
                reserve.push(service.inner.control_connection());
            }
            let all_reserved = reserve.iter().all(Option::is_some);
            let full = service.inner.control_connection().is_none();
            drop(reserve);
            for release in releases {
                release.send(()).unwrap();
            }
            for thread in threads {
                thread.join().unwrap();
            }
            assert_eq!(status, 503);
            assert_eq!(response["code"], "busy");
            assert_eq!(cancel.0, 200);
            assert_eq!(cancel.1["cancel"]["state"], "uncertain");
            assert!(elapsed < Duration::from_secs(1));
            assert!(all_reserved);
            assert!(full);
        });
    }
}

#[test]
fn successful_envelopes_match_the_request_with_and_without_a_pinned_base() {
    use falcone_serverless_worker::api;
    for base in [
        None,
        Some(
            json!({"kind":"catalog","id":"test","version":"1.0.0","digest":format!("sha256:{}", "a".repeat(64))}),
        ),
    ] {
        let mut input = request();
        if let Some(base) = base {
            input["projection"]["base"] = base;
        }
        let parsed = api::parse_request(&serde_json::to_vec(&input).unwrap()).unwrap();
        let result =
            api::build_result(&parsed, br#"{"statusCode":200,"body":{"ok":true}}"#, vec![])
                .unwrap();
        let bytes = serde_json::to_vec(&result).unwrap();
        api::parse_result(&bytes, &parsed).unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["status"], "success");
        assert_eq!(result["identity"]["wasmtime_version"], "49.0.5");
        assert_eq!(result["identity"]["interface_version"], "0.2.0");
        assert_eq!(
            result["identity"]["function_revision"],
            "rev-synthetic-0003"
        );
        assert_eq!(result["identity"]["runtime_mode"], "wasm");
        assert_eq!(
            result["identity"].get("base_digest").is_some(),
            input["projection"].get("base").is_some()
        );
    }
}

#[test]
fn http_preserves_all_three_legacy_refusals_before_invocation() {
    let service = Service::start(1);
    for (params, status, code, reason) in [
        (json!({}), 400, "invalid_request", "missing_path"),
        (
            json!({"__ow_path":"/","__ow_headers":{"Content-Type":"application/json"},"__ow_body":"invalid"}),
            400,
            "invalid_request",
            "non_json_body",
        ),
        (
            json!({"__ow_path":"/","__ow_body":"a".repeat(1_048_577)}),
            413,
            "request_too_large",
            "body_too_large",
        ),
    ] {
        let mut input = request();
        input["params"] = params;
        let (actual, response) = service.post("/v1/execute", &input);
        assert_eq!(actual, status);
        assert_eq!(response["status"], status);
        assert_eq!(response["code"], code);
        assert_eq!(response["detail"]["reason"], reason);
        assert_eq!(response["retryable"], false);
        assert!(!service.root.join("native").exists());
    }
}

#[test]
fn oversized_transport_is_a_framing_error_not_a_legacy_body_refusal() {
    use execution_http::Wire;
    let service = Service::start(1);
    let mut wire = Wire::new("/v1/execute", &request());
    wire.input = std::io::Cursor::new(
        b"POST /v1/execute HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 8388609\r\n\r\n"
            .to_vec(),
    );
    service
        .inner
        .connection()
        .unwrap()
        .respond(&mut wire, &mut Vec::new());
    assert_eq!(wire.response(), (400, json!({"code":"invalid_request"})));
    assert!(!service.root.join("native").exists());
}

#[test]
fn qualification_binaries_match_the_wat_oracles_without_running_guests() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/qualification/api_fixtures");
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "wat") {
            assert_eq!(
                wat::parse_file(&path).unwrap(),
                std::fs::read(path.with_extension("wasm")).unwrap(),
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn short_identity_claims_do_not_corrupt_ordinary_guest_logs() {
    use falcone_serverless_worker::api;
    let mut input = request();
    input["credential"]["tenant"] = json!("a");
    let parsed = api::parse_request(&serde_json::to_vec(&input).unwrap()).unwrap();
    let result = api::build_result(
        &parsed,
        br#"{"statusCode":200}"#,
        vec!["ordinary guest diagnostic".into()],
    )
    .unwrap();
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(result["logs"][0], "ordinary guest diagnostic");
}

#[test]
fn result_logs_redact_credential_claims_and_preserve_ordinary_diagnostics() {
    use falcone_serverless_worker::api;
    let mut input = request();
    input["credential"]["iss"] = json!("secret-canary-credential-130");
    let parsed = api::parse_request(&serde_json::to_vec(&input).unwrap()).unwrap();
    let result = api::build_result(
        &parsed,
        br#"{"statusCode":200}"#,
        vec![
            "ordinary guest log".into(),
            "issuer=secret-canary-credential-130".into(),
        ],
    )
    .unwrap();
    let wire = serde_json::to_value(result).unwrap();
    assert_eq!(wire["logs"][0], "ordinary guest log");
    assert_eq!(wire["logs"][1], "issuer=<redacted>");
}

#[test]
fn declared_allowed_component_imports_pass_preflight_without_guest_execution() {
    use falcone_serverless_worker::{
        api::{self, Pinned},
        artifact::validate,
    };
    let mut input = request();
    input["projection"]["imports"] = json!([
        "falcone:host/invocation-context@0.2.0",
        "falcone:host/logging@0.2.0"
    ]);
    let request = api::parse_request(&serde_json::to_vec(&input).unwrap()).unwrap();
    let bytes = include_bytes!("../src/qualification/api_fixtures/logging.wasm");
    let component = validate::validate(bytes, bytes.len()).unwrap();
    request
        .projection()
        .check_imports(&component.imports)
        .unwrap();
}

#[test]
fn structured_log_redaction_covers_json_escaped_claims() {
    use falcone_serverless_worker::api;
    let mut input = request();
    input["credential"]["iss"] = json!("secret\"canary\\credential\n130");
    let parsed = api::parse_request(&serde_json::to_vec(&input).unwrap()).unwrap();
    let line = json!({"message":"secret\"canary\\credential\n130"}).to_string();
    let result = api::build_result(&parsed, br#"{"statusCode":200}"#, vec![line]).unwrap();
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(result["logs"][0], "{\"message\":\"<redacted>\"}");
}

#[test]
fn malformed_json_and_ambiguous_http_framing_are_redacted_before_admission() {
    use execution_http::Wire;
    let service = Service::start(1);
    for header in [
        "",
        "Transfer-Encoding: chunked\r\n",
        "Content-Length: 1\r\n",
    ] {
        let mut wire = Wire::new(
            "/v1/execute",
            &json!({"credential":"secret-canary-credential-130"}),
        );
        let mut input = String::from_utf8(wire.input.into_inner()).unwrap();
        if header.is_empty() {
            input = input.replace("{\"credential\"", "!\"credential\"");
        } else {
            input = input.replace(
                "Host: localhost\r\n",
                &format!("Host: localhost\r\n{header}"),
            );
        }
        wire.input = std::io::Cursor::new(input.into_bytes());
        let mut diagnostics = Vec::new();
        service
            .inner
            .connection()
            .unwrap()
            .respond(&mut wire, &mut diagnostics);
        assert_eq!(wire.response().0, 400);
        assert!(
            !String::from_utf8(wire.output)
                .unwrap()
                .contains("secret-canary")
        );
        assert!(
            !String::from_utf8(diagnostics)
                .unwrap()
                .contains("secret-canary")
        );
        assert!(!service.root.join("native").exists());
    }
}
