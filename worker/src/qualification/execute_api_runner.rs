//! RUN-01 qualification of the served execution API, run inside a restricted fs-test pod.
//!
//! Each case starts the real `falcone-serverless-worker serve` process beside this executable
//! and drives it over its pod-local port with HTTP from here, the client, in the same pod. The
//! guests are embedded and staged by digest in a private read-only-by-convention directory.
//! A case passes by returning `Ok`; the pod's exit status is the verdict.

use falcone_serverless_worker::api::{parse_cancel, parse_request, parse_result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ENGINE: &str = "49.0.5";
const OK: &[u8] = include_bytes!("execute_api_fixtures/ok.wasm");
const PROBE: &[u8] = include_bytes!("execute_api_fixtures/probe.wasm");
const SPIN: &[u8] = include_bytes!("execute_api_fixtures/spin.wasm");
const WASI: &[u8] = include_bytes!("execute_fixtures/wasi-import.wasm");

/// Credential claim values and a parameter, each unique in the process and in the worker.
const CLAIMS: [(&str, &str); 5] = [
    ("iss", "claim-iss-run01-1c7d"),
    ("aud", "claim-aud-run01-5e92"),
    ("tenant", "claim-tenant-run01-a03b"),
    ("workspace", "claim-workspace-run01-77f4"),
    ("pool_profile", "claim-pool-run01-d815"),
];
const PARAM: &str = "param-token-run01-4d2e";

fn check(ok: bool, reason: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(reason.to_owned()) }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// An execute request for the artifact staged under `hex`, carrying the claim values.
fn request(hex: &str, attempt: &str, base: Option<Value>) -> Value {
    let mut credential = json!({
        "revision": "rev-run01-0001", "activation": "act-run01-0001", "attempt": attempt,
        "iat": 1, "exp": 2, "allowed_capabilities": []
    });
    for (claim, value) in CLAIMS {
        credential[claim] = value.into();
    }
    let digest = format!("sha256:{hex}");
    let mut projection = json!({
        "revision": "rev-run01-0001", "runtime_mode": "wasm",
        "artifact_digest": digest, "component_digest": digest,
        "language_profile": {"name": "profile-run01", "version": "1"},
        "engine_version": ENGINE, "wasi_version": "0.2.6", "interface_version": "0.2.0",
        "imports": [],
        "budgets": {"fuel": 1, "memory_bytes": 1, "timeout_ms": 1, "max_host_calls": 1},
        "grants": []
    });
    if let Some(base) = base {
        projection["base"] = base;
    }
    json!({
        "version": "1", "type": "execute",
        "activation_id": "act-run01-0001", "attempt_id": attempt, "attempt_number": 1,
        "deadline": "2030-01-01T00:00:00Z",
        "invocation": {"invocation_key": "inv-run01-0001"},
        "credential": credential, "projection": projection,
        "params": {"__ow_method": "post", "__ow_path": "/v1/run01", "probe": PARAM}
    })
}

struct Worker {
    child: Child,
    port: u16,
    root: PathBuf,
    logs: Arc<Mutex<Vec<String>>>,
}

impl Worker {
    fn start() -> Result<Self, String> {
        let root = std::env::temp_dir().join(format!("run01-{}", std::process::id()));
        std::fs::create_dir_all(root.join("artifacts")).map_err(|_| "scratch_unavailable")?;
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "pool_id = \"pool-run01\"\nlisten_port = 0\nfalcone_api_endpoint = \"http://api.invalid\"\nfalcone_dispatcher_endpoint = \"http://dispatcher.invalid\"\ncache_path = \"{}\"\n",
                root.join("cache").display()
            ),
        )
        .map_err(|_| "scratch_unavailable")?;
        let binary = std::env::current_exe()
            .map_err(|_| "worker_unavailable")?
            .with_file_name("falcone-serverless-worker");
        let mut child = Command::new(binary)
            .arg("serve")
            .arg(&config)
            .arg("--artifacts")
            .arg(root.join("artifacts"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| "worker_not_started")?;
        let mut lines = BufReader::new(child.stderr.take().ok_or("no_worker_logs")?).lines();
        let first = lines
            .next()
            .and_then(Result::ok)
            .ok_or("worker_not_listening")?;
        let port = serde_json::from_str::<Value>(&first)
            .ok()
            .and_then(|line| line["port"].as_str()?.parse().ok())
            .ok_or("worker_not_listening")?;
        let logs = Arc::new(Mutex::new(vec![first]));
        let sink = logs.clone();
        std::thread::spawn(move || {
            for line in lines.map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });
        Ok(Self {
            child,
            port,
            root,
            logs,
        })
    }

    fn stage(&self, bytes: &[u8]) -> Result<String, String> {
        let hex = sha256(bytes);
        std::fs::write(
            self.root.join("artifacts").join(format!("{hex}.wasm")),
            bytes,
        )
        .map_err(|_| "stage_failed")?;
        Ok(hex)
    }

    fn send(&self, path: &str, body: &[u8]) -> Result<(u16, Vec<u8>), String> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).map_err(|_| "connect")?;
        for result in [
            stream.set_read_timeout(Some(Duration::from_secs(120))),
            stream.set_write_timeout(Some(Duration::from_secs(60))),
        ] {
            result.map_err(|_| "socket")?;
        }
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: worker\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        // The worker may answer and close before a large body has been written.
        let _ = stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(body));
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or("no_response")?;
        let status = std::str::from_utf8(&raw[..split])
            .ok()
            .and_then(|head| head.split(' ').nth(1)?.parse().ok())
            .ok_or("bad_response")?;
        Ok((status, raw[split + 4..].to_vec()))
    }

    fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), String> {
        let (status, body) = self.send(path, &serde_json::to_vec(body).map_err(|_| "encode")?)?;
        Ok((
            status,
            serde_json::from_slice(&body).map_err(|_| "not_json")?,
        ))
    }

    fn logs(&self) -> String {
        std::thread::sleep(Duration::from_millis(200));
        self.logs.lock().unwrap().join("\n")
    }

    fn guests_started(&self) -> usize {
        self.logs().matches("\"guest_start\"").count()
    }

    fn wait_for_guest(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if self
                .logs
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("\"guest_start\""))
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("guest_never_started".into())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// AC1: executing a Wasm revision returns the legacy envelope whose identity matches the
/// projection and the pinned engine, for a revision with no base, a catalog base and a BYO base.
fn identity() -> Result<(), String> {
    let worker = Worker::start()?;
    let hex = worker.stage(OK)?;
    let digest = |character: char| format!("sha256:{}", character.to_string().repeat(64));
    let bases = [
        None,
        Some(
            json!({"kind": "catalog", "id": "base-run01", "version": "1.0.0", "digest": digest('b')}),
        ),
        Some(
            json!({"kind": "byo", "image_digest": digest('c'), "signer_identity": "signer-run01", "attestation_ref": "attestation-run01"}),
        ),
    ];
    for (index, base) in bases.into_iter().enumerate() {
        let document = request(&hex, &format!("att-run01-{index}"), base);
        let (status, body) = worker.send("/v1/execute", &serde_json::to_vec(&document).unwrap())?;
        check(status == 200, "execute_not_200")?;
        let pinned = parse_request(&serde_json::to_vec(&document).unwrap())
            .map_err(|_| "request_not_admitted")?;
        let result = parse_result(&body, &pinned).map_err(|_| "result_does_not_match_request")?;
        let value = serde_json::to_value(&result).map_err(|_| "encode")?;
        check(value["status"] == "success", "not_success")?;
        check(value["result"]["statusCode"] == 200, "wrong_handler_result")?;
        check(
            value["result"]["body"] == json!({"ok": true}),
            "wrong_handler_body",
        )?;
        check(
            value["identity"]["wasmtime_version"] == ENGINE,
            "wrong_engine_identity",
        )?;
        check(
            value["identity"]["artifact_digest"] == format!("sha256:{hex}"),
            "wrong_artifact_identity",
        )?;
    }
    check(worker.guests_started() == 3, "guest_count")
}

/// AC2: a mode other than wasm, a request naming an image and staged OCI image bytes are
/// refused, and no guest is created for any of them.
fn refusals() -> Result<(), String> {
    let worker = Worker::start()?;
    let hex = worker.stage(OK)?;
    for mode in ["oci", "container"] {
        let mut document = request(&hex, "att-run01-mode", None);
        document["projection"]["runtime_mode"] = mode.into();
        let (status, reply) = worker.post("/v1/execute", &document)?;
        let refusal = if mode == "oci" {
            &reply["result"]
        } else {
            &reply
        };
        check(status == 409, "mode_status")?;
        check(refusal["code"] == "runtime_mode_not_supported", "mode_code")?;
        check(
            refusal["detail"]["reason"] == "runtime_mode_not_wasm",
            "mode_reason",
        )?;
        check(refusal["retryable"] == false, "mode_retryable")?;
    }
    let mut named = request(&hex, "att-run01-image", None);
    named["image"] = "registry.invalid/function@sha256:00".into();
    check(
        worker.post("/v1/execute", &named)?.0 == 400,
        "image_not_malformed",
    )?;
    let image = b"\x1f\x8b\x08\0 an OCI image layer, not a Wasm component".to_vec();
    let staged = worker.stage(&image)?;
    let (status, reply) = worker.post("/v1/execute", &request(&staged, "att-run01-layer", None))?;
    check(
        status == 422 && reply["error"] == "precompiled_rejected",
        "image_bytes",
    )?;
    check(worker.guests_started() == 0, "guest_created")
}

/// AC7: a missing artifact, one with another digest and an oversized body are refused before
/// anything is compiled or created.
fn artifacts() -> Result<(), String> {
    let worker = Worker::start()?;
    let missing = sha256(b"never staged");
    let (status, reply) = worker.post("/v1/execute", &request(&missing, "att-run01-a", None))?;
    check(
        status == 404 && reply["error"] == "artifact_unavailable",
        "missing",
    )?;
    let pinned = sha256(b"the pinned bytes");
    std::fs::write(
        worker.root.join("artifacts").join(format!("{pinned}.wasm")),
        OK,
    )
    .map_err(|_| "stage_failed")?;
    let (status, reply) = worker.post("/v1/execute", &request(&pinned, "att-run01-b", None))?;
    check(
        status == 422 && reply["error"] == "artifact_digest_mismatch",
        "mismatch",
    )?;
    let (status, body) = worker.send("/v1/execute", &vec![b' '; 4 * 1024 * 1024 + 1])?;
    check(status == 413, "oversized_status")?;
    let refusal: Value = serde_json::from_slice(&body).map_err(|_| "not_json")?;
    check(refusal["code"] == "request_too_large", "oversized_code")?;
    check(worker.guests_started() == 0, "guest_created")
}

fn decode(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// AC4: the guest finds no credential claim value in its memory, and a component cannot import
/// the WASI environment, arguments or filesystem at all.
fn credential() -> Result<(), String> {
    let worker = Worker::start()?;
    let hex = worker.stage(PROBE)?;
    let (status, body) = worker.send(
        "/v1/execute",
        &serde_json::to_vec(&request(&hex, "att-run01-p", None)).unwrap(),
    )?;
    check(status == 200, "probe_not_200")?;
    let reply: Value = serde_json::from_slice(&body).map_err(|_| "not_json")?;
    let dump = decode(reply["result"]["body"].as_str().ok_or("no_memory_dump")?);
    check(dump.len() == 0x30000, "short_memory_dump")?;
    // The control: the scan can find a value that was passed to the guest.
    check(contains(&dump, PARAM), "scan_cannot_find_a_param")?;
    for (claim, value) in CLAIMS {
        check(!contains(&dump, value), &format!("{claim}_in_guest_memory"))?;
        check(!contains(&body, value), &format!("{claim}_in_response"))?;
    }
    let wasi = worker.stage(WASI)?;
    let (status, reply) = worker.post("/v1/execute", &request(&wasi, "att-run01-w", None))?;
    check(
        status == 422 && reply["error"] == "undeclared_import",
        "wasi_import_admitted",
    )?;
    let logs = worker.logs();
    for (claim, value) in CLAIMS {
        check(!logs.contains(value), &format!("{claim}_in_logs"))?;
    }
    check(logs.contains("<redacted>"), "no_redaction_marker")
}

/// AC6: cancelling a running attempt stops the guest and is acknowledged; a repeated cancel and
/// a cancel or update for an unknown attempt are refused.
fn cancel() -> Result<(), String> {
    let worker = Worker::start()?;
    let hex = worker.stage(SPIN)?;
    let document = request(&hex, "att-run01-spin", None);
    let cancel = json!({"version": "1", "type": "cancel", "activation_id": "act-run01-0001",
        "attempt_id": "att-run01-spin", "cancel": {"state": "requested"}});
    let worker = Arc::new(worker);
    let running = {
        let worker = worker.clone();
        std::thread::spawn(move || worker.post("/v1/execute", &document))
    };
    worker.wait_for_guest()?;
    let (status, reply) = worker.post("/v1/cancel", &cancel)?;
    check(status == 200, "cancel_not_200")?;
    let answer = parse_cancel(&serde_json::to_vec(&reply).unwrap()).map_err(|_| "bad_cancel")?;
    check(!answer.is_requested(), "answer_is_a_request")?;
    check(
        reply["cancel"]["state"] == "acknowledged",
        "not_acknowledged",
    )?;
    let (status, reply) = running.join().map_err(|_| "execute_panicked")??;
    check(
        status == 409 && reply["error"] == "cancelled",
        "guest_not_cancelled",
    )?;
    // The attempt is over: a second cancel and an unknown attempt change nothing.
    check(
        worker.post("/v1/cancel", &cancel)?.0 == 404,
        "second_cancel",
    )?;
    let mut unknown = cancel.clone();
    unknown["attempt_id"] = "att-run01-unknown".into();
    check(
        worker.post("/v1/cancel", &unknown)?.0 == 404,
        "unknown_cancel",
    )?;
    let update = json!({"version": "1", "type": "update", "activation_id": "act-run01-0001",
        "attempt_id": "att-run01-unknown", "sequence": 1, "update": {"kind": "heartbeat"}});
    check(
        worker.post("/v1/update", &update)?.0 == 404,
        "unknown_update",
    )?;
    check(worker.guests_started() == 1, "guest_count")
}

pub fn run(case: &str) -> ExitCode {
    let outcome = match case {
        "identity" => identity(),
        "refusals" => refusals(),
        "artifacts" => artifacts(),
        "credential" => credential(),
        "cancel" => cancel(),
        _ => Err("unknown_execute_api_case".into()),
    };
    match outcome {
        Ok(()) => {
            println!("{}", json!({"case": case, "status": "passed"}));
            println!("{}", json!({"check": "execute_api", "status": "passed"}));
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!(
                "{}",
                json!({"case": case, "status": "failed", "reason": reason})
            );
            ExitCode::FAILURE
        }
    }
}
