//! ctr-worker-execution-api v1 served over pod-local HTTP, without running a guest.
//!
//! Each test starts the real `falcone-serverless-worker serve` process on an ephemeral port and
//! speaks HTTP to it over `std::net`. The artifact source holds no component that a request
//! could run, so every case here is decided before a guest exists: request parsing, refusals,
//! the body bound, artifact verification and log redaction. Guest-executing cases are in
//! `cluster_execute_api.rs`.

#![cfg(target_os = "linux")]

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Values no worker log or error body may contain.
const SENTINELS: [&str; 6] = [
    "sentinel-iss-7f3a",
    "sentinel-aud-7f3a",
    "sentinel-tenant-7f3a",
    "sentinel-workspace-7f3a",
    "sentinel-pool-7f3a",
    "sentinel-param-7f3a",
];

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/contract")
        .join(format!("{name}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A valid Wasm request whose credential claims and parameters carry the sentinels.
fn request() -> Value {
    let mut request = fixture("request/valid/wasm-no-base");
    let credential = &mut request["credential"];
    for (claim, sentinel) in ["iss", "aud", "tenant", "workspace", "pool_profile"]
        .into_iter()
        .zip(SENTINELS)
    {
        credential[claim] = sentinel.into();
    }
    request["params"]["probe"] = SENTINELS[5].into();
    request
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "execute-api-local-{}-{}",
            std::process::id(),
            sha256(format!("{:?}", std::time::Instant::now()).as_bytes())
        ));
        std::fs::create_dir_all(path.join("artifacts")).unwrap();
        Self(path)
    }

    fn artifacts(&self) -> PathBuf {
        self.0.join("artifacts")
    }

    fn stage(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.artifacts().join(format!("{name}.wasm")), bytes).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Worker {
    child: Child,
    port: u16,
    logs: Arc<Mutex<String>>,
    scratch: Scratch,
}

impl Worker {
    fn start() -> Self {
        let scratch = Scratch::new();
        let config = scratch.0.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "pool_id = \"pool-local\"\nlisten_port = 0\nfalcone_api_endpoint = \"http://api.invalid\"\nfalcone_dispatcher_endpoint = \"http://dispatcher.invalid\"\ncache_path = \"{}\"\n",
                scratch.0.join("cache").display()
            ),
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
            .arg("serve")
            .arg(&config)
            .arg("--artifacts")
            .arg(scratch.artifacts())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let first: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(first["event"], "listening");
        let port = first["port"].as_str().unwrap().parse().unwrap();
        let logs = Arc::new(Mutex::new(first.to_string()));
        let sink = logs.clone();
        std::thread::spawn(move || {
            for line in lines.map_while(Result::ok) {
                let mut sink = sink.lock().unwrap();
                sink.push('\n');
                sink.push_str(&line);
            }
        });
        Self {
            child,
            port,
            logs,
            scratch,
        }
    }

    /// One HTTP exchange; the status and the body.
    fn send(&self, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(60)))
            .unwrap();
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(60)))
            .unwrap();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: worker\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        // The worker may answer and close before a large body is written.
        let _ = stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(body));
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&raw[..split]).unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, raw[split + 4..].to_vec())
    }

    fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let (status, body) = self.send("POST", path, &serde_json::to_vec(body).unwrap());
        (status, serde_json::from_slice(&body).unwrap())
    }

    fn logs(&self) -> String {
        // The log reader runs on its own thread; let it drain what the worker has written.
        std::thread::sleep(std::time::Duration::from_millis(200));
        self.logs.lock().unwrap().clone()
    }

    /// Whether any guest was started: the worker logs `guest_start` before it creates one.
    fn started_a_guest(&self) -> bool {
        self.logs().contains("\"guest_start\"")
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = &self.scratch;
    }
}

fn refusal(code: &str, status: u16, reason: &str) -> Value {
    let message = match code {
        "runtime_mode_not_supported" => "This worker runs only revisions in runtime mode wasm.",
        "invalid_request" => "The request is malformed.",
        "request_too_large" => "The request body exceeds the size budget.",
        _ => unreachable!(),
    };
    json!({"status": status, "code": code, "message": message, "detail": {"reason": reason}, "retryable": false})
}

#[test]
fn a_mode_other_than_wasm_is_refused_before_anything_is_created() {
    let worker = Worker::start();
    // `oci` is a valid mode: the refusal answers the activation, with its identity.
    let mut oci = request();
    oci["projection"]["runtime_mode"] = "oci".into();
    let (status, reply) = worker.post("/v1/execute", &oci);
    assert_eq!(status, 409);
    assert_eq!(
        reply["result"],
        refusal("runtime_mode_not_supported", 409, "runtime_mode_not_wasm")
    );
    assert_eq!(reply["status"], "failure");
    assert_eq!(reply["class"], "non_retryable");
    assert_eq!(reply["activation_id"], oci["activation_id"]);
    assert_eq!(reply["identity"]["runtime_mode"], "oci");
    // An unknown mode cannot be read in full: the bare refusal is answered.
    let mut unknown = request();
    unknown["projection"]["runtime_mode"] = "container".into();
    let (status, reply) = worker.post("/v1/execute", &unknown);
    assert_eq!(status, 409);
    assert_eq!(
        reply,
        refusal("runtime_mode_not_supported", 409, "runtime_mode_not_wasm")
    );
    assert!(!worker.started_a_guest());
}

#[test]
fn a_request_that_names_an_image_is_malformed() {
    let worker = Worker::start();
    for place in ["", "projection", "base-kind"] {
        let mut named = request();
        match place {
            "" => named["image"] = "registry.invalid/function@sha256:00".into(),
            "projection" => named["projection"]["image"] = "registry.invalid/f".into(),
            _ => {
                named["projection"]["base"] = json!({"kind": "image", "ref": "registry.invalid/f"})
            }
        }
        let (status, reply) = worker.post("/v1/execute", &named);
        assert_eq!(status, 400, "{place}");
        assert_eq!(
            reply,
            json!({"error": "malformed message: does not match the schema"})
        );
    }
    assert!(!worker.started_a_guest());
}

#[test]
fn legacy_inputs_are_refused_with_their_contract_status_and_reason() {
    let worker = Worker::start();
    let mut missing = request();
    missing["params"]
        .as_object_mut()
        .unwrap()
        .remove("__ow_path");
    let mut oversized = request();
    oversized["params"]["__ow_body"] = "x".repeat(1_048_577).into();
    let mut not_json = request();
    not_json["params"]["__ow_body"] = "{not json".into();
    not_json["params"]["__ow_headers"]["content-type"] = "application/json".into();
    for (name, document, status, code, reason) in [
        (
            "missing path",
            missing,
            400,
            "invalid_request",
            "missing_path",
        ),
        (
            "oversized",
            oversized,
            413,
            "request_too_large",
            "body_too_large",
        ),
        (
            "non-JSON",
            not_json,
            400,
            "invalid_request",
            "non_json_body",
        ),
    ] {
        let (got, reply) = worker.post("/v1/execute", &document);
        assert_eq!(got, status, "{name}");
        assert_eq!(reply["result"], refusal(code, status, reason), "{name}");
        assert_eq!(reply["class"], "non_retryable", "{name}");
    }
    assert!(!worker.started_a_guest());
}

#[test]
fn malformed_bodies_get_a_fixed_label_and_no_envelope() {
    let worker = Worker::start();
    for (body, label) in [
        (&b"{ not json"[..], "malformed message: not JSON"),
        (
            br#"{"version":"1"}"#,
            "malformed message: does not match the schema",
        ),
    ] {
        let (status, body) = worker.send("POST", "/v1/execute", body);
        assert_eq!(status, 400);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"error": label})
        );
    }
    // A credential that does not match the request is a cross-field rule.
    let mut mismatch = request();
    mismatch["credential"]["attempt"] = "att-other-0001".into();
    let (status, reply) = worker.post("/v1/execute", &mismatch);
    assert_eq!(status, 400);
    assert_eq!(
        reply,
        json!({"error": "malformed message: rule credential.binding violated"})
    );
}

#[test]
fn a_body_above_the_fixed_bound_is_refused_before_it_is_parsed() {
    let worker = Worker::start();
    // Valid JSON, but above the bound: it must be refused without being read.
    let body = vec![b' '; 4 * 1024 * 1024 + 1];
    let (status, body) = worker.send("POST", "/v1/execute", &body);
    assert_eq!(status, 413);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        refusal("request_too_large", 413, "body_too_large")
    );
    // The worker still serves.
    let (status, _) = worker.post("/v1/cancel", &json!({}));
    assert_eq!(status, 400);
}

#[test]
fn only_the_v1_operations_are_routed() {
    let worker = Worker::start();
    let (status, _) = worker.send("GET", "/v1/execute", b"");
    assert_eq!(status, 405);
    let (status, _) = worker.send("POST", "/v1/unknown", b"{}");
    assert_eq!(status, 404);
    let (status, _) = worker.send("POST", "/v2/execute", b"{}");
    assert_eq!(status, 404);
}

#[test]
fn an_artifact_that_is_missing_or_has_another_digest_is_refused_before_compile() {
    let worker = Worker::start();
    let document = request();
    let digest = document["projection"]["artifact_digest"].as_str().unwrap();
    let hex = digest.strip_prefix("sha256:").unwrap();
    let (status, reply) = worker.post("/v1/execute", &document);
    assert_eq!(
        (status, reply),
        (404, json!({"error": "artifact_unavailable"}))
    );
    // Staged under the pinned digest, but the bytes hash to another.
    worker
        .scratch
        .stage(hex, b"\0asm\x0d\0\x01\0 not the pinned artifact");
    let (status, reply) = worker.post("/v1/execute", &document);
    assert_eq!(
        (status, reply),
        (422, json!({"error": "artifact_digest_mismatch"}))
    );
    assert!(!worker.started_a_guest());
}

#[test]
fn staged_bytes_that_are_an_oci_image_are_not_a_component() {
    let worker = Worker::start();
    // A gzip stream, the shape of an OCI layer, staged under its own correct digest.
    let image = b"\x1f\x8b\x08\0 an oci image layer, not a Wasm component".to_vec();
    let mut document = request();
    document["projection"]["artifact_digest"] = format!("sha256:{}", sha256(&image)).into();
    worker.scratch.stage(&sha256(&image), &image);
    let (status, reply) = worker.post("/v1/execute", &document);
    assert_eq!(
        (status, reply),
        (422, json!({"error": "precompiled_rejected"}))
    );
    assert!(!worker.started_a_guest());
}

#[test]
fn a_pinned_engine_other_than_the_embedded_one_is_not_run() {
    let worker = Worker::start();
    let mut document = request();
    document["projection"]["engine_version"] = "1.0.0".into();
    let (status, reply) = worker.post("/v1/execute", &document);
    assert_eq!(
        (status, reply),
        (409, json!({"error": "engine_version_mismatch"}))
    );
    assert!(!worker.started_a_guest());
}

#[test]
fn a_cancel_or_update_for_an_unknown_attempt_is_refused_without_effect() {
    let worker = Worker::start();
    let cancel = fixture("cancel/valid/requested");
    let (status, reply) = worker.post("/v1/cancel", &cancel);
    assert_eq!((status, reply), (404, json!({"error": "unknown_attempt"})));
    let update = fixture("update/valid/heartbeat");
    let (status, reply) = worker.post("/v1/update", &update);
    assert_eq!((status, reply), (404, json!({"error": "unknown_attempt"})));
    // Only `requested` is a request: the answers are the worker's.
    let (status, _) = worker.post("/v1/cancel", &fixture("cancel/valid/acknowledged"));
    assert_eq!(status, 400);
    let (status, _) = worker.post("/v1/cancel", &json!({"version": "1"}));
    assert_eq!(status, 400);
    let (status, _) = worker.post("/v1/update", &json!({"version": "1"}));
    assert_eq!(status, 400);
    assert!(!worker.started_a_guest());
}

#[test]
fn logs_and_error_bodies_carry_no_request_value() {
    let worker = Worker::start();
    let mut bodies = Vec::new();
    // Every outcome that can reach a log or an error body, each carrying the sentinels.
    let mut oci = request();
    oci["projection"]["runtime_mode"] = "oci".into();
    let mut unknown = request();
    unknown["projection"]["runtime_mode"] = "container".into();
    let mut oversized = request();
    oversized["params"]["__ow_body"] = "x".repeat(1_048_577).into();
    let mut mismatch = request();
    mismatch["credential"]["attempt"] = SENTINELS[1].into();
    let mut image = request();
    image["image"] = SENTINELS[2].into();
    let mut wrong_type = request();
    wrong_type["credential"]["iat"] = SENTINELS[0].into();
    let mut engine = request();
    engine["projection"]["engine_version"] = SENTINELS[4].into();
    for document in [
        request(),
        oci,
        unknown,
        oversized,
        mismatch,
        image,
        wrong_type,
        engine,
    ] {
        bodies.push(
            worker
                .send(
                    "POST",
                    "/v1/execute",
                    &serde_json::to_vec(&document).unwrap(),
                )
                .1,
        );
    }
    // Syntax errors that quote a credential value in their text.
    let broken = format!("{{\"credential\": {{\"iss\": \"{}\" ", SENTINELS[0]);
    bodies.push(worker.send("POST", "/v1/execute", broken.as_bytes()).1);
    let mut cancel = fixture("cancel/valid/requested");
    cancel["activation_id"] = SENTINELS[3].into();
    bodies.push(
        worker
            .send("POST", "/v1/cancel", &serde_json::to_vec(&cancel).unwrap())
            .1,
    );
    let logs = worker.logs();
    for sentinel in SENTINELS {
        assert!(!logs.contains(sentinel), "a log carries a request value");
        for body in &bodies {
            assert!(
                !String::from_utf8_lossy(body).contains(sentinel),
                "an error body carries a request value"
            );
        }
    }
    // The requests were logged, as a marker.
    assert!(logs.contains("ExecuteRequest(<redacted>)"), "{logs}");
    assert!(logs.contains("<redacted>"));
    for line in logs.lines() {
        assert!(
            serde_json::from_str::<Value>(line).is_ok(),
            "not structured: {line}"
        );
    }
}

/// The guests of the cluster qualification are embedded as `.wasm` beside their `.wat` source.
#[test]
fn the_embedded_guests_are_compiled_from_their_sources() {
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/qualification/execute_api_fixtures");
    for name in ["ok", "probe", "spin"] {
        let source = std::fs::read_to_string(directory.join(format!("{name}.wat"))).unwrap();
        let compiled = wat::parse_str(&source).unwrap();
        let path = directory.join(format!("{name}.wasm"));
        if std::env::var_os("REGENERATE_EXECUTE_API_GUESTS").is_some() {
            std::fs::write(&path, &compiled).unwrap();
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            compiled,
            "{name}.wasm is stale"
        );
    }
}
