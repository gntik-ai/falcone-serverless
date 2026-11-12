//! RUN-01.t3 restricted-pod qualification of the worker execution API over real TCP.
//!
//! Each case starts the service on loopback inside the restricted fs-test pod (non-root, read-only
//! root filesystem, seccomp RuntimeDefault) with its production limits, drives it through a real
//! socket, and ends by proving that no slot leaked: a normal execute and a cancel both succeed
//! right afterwards and every connection, execute and control slot is released. A case prints one
//! evidence record, `{"case", "group", "status", "elapsed_ms"}`; the cases of one group are
//! recorded together, the `saturation` group beside the `http` group of size ceilings and
//! transfer deadlines.

use falcone_serverless_worker::api::{self, MARKER};
use falcone_serverless_worker::artifact::cache_key::ENGINE_VERSION;
use falcone_serverless_worker::server::{
    Artifacts, CONTROL_RESERVE, Config, HttpLimits, Running, Server,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

type Outcome = Result<(), String>;

/// `(case, group, run)`.
type Case = (&'static str, &'static str, fn() -> Outcome);

/// The cases of this task. Every security-relevant size ceiling and transfer deadline of the
/// service has one.
pub const CASES: &[Case] = &[
    ("identity", "api", identity),
    ("non-wasm", "api", non_wasm),
    ("legacy", "api", legacy),
    ("credential", "api", credential),
    ("imports", "api", imports),
    ("saturation", "saturation", saturation),
    ("saturation-stalled", "saturation", saturation_stalled),
    ("saturation-headers", "saturation", saturation_headers),
    ("http-headers", "http", http_headers),
    ("http-request", "http", http_request),
    ("http-action-body", "http", http_action_body),
    ("http-silent", "http", http_silent),
    ("http-drip", "http", http_drip),
    ("http-stall", "http", http_stall),
    ("http-control-body", "http", http_control_body),
    ("http-body-drip", "http", http_body_drip),
];

const ECHO: &[u8] = include_bytes!("execute_fixtures/handler.wasm");
const IMPORTS: &[u8] = include_bytes!("execute_fixtures/api-imports.wasm");
const SPIN: &[u8] = include_bytes!("execute_fixtures/api-spin.wasm");
const WASI: &[u8] = include_bytes!("execute_fixtures/wasi-import.wasm");
const PROGRESS: &str = "falcone:host/progress@0.2.0";
const LOGGING: &str = "falcone:host/logging@0.2.0";
const CONTEXT: &str = "falcone:host/invocation-context@0.2.0";
const CANARY: &str = "canary-credential-0001";

fn check(ok: bool, reason: &str) -> Outcome {
    if ok { Ok(()) } else { Err(reason.to_owned()) }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// The artifact store boundary of the pod: the embedded components, counting every fetch.
struct Store {
    fetches: AtomicUsize,
}

impl Artifacts for Store {
    fn component(&self, wanted: &str) -> Option<Vec<u8>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        [ECHO, IMPORTS, SPIN, WASI]
            .into_iter()
            .find(|bytes| digest(bytes) == wanted)
            .map(<[u8]>::to_vec)
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Result<Value, String> {
        serde_json::from_slice(&self.body).map_err(|_| "reply_not_json".to_owned())
    }
}

fn read_reply(stream: &mut TcpStream) -> Option<Reply> {
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let split = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
    Some(Reply {
        status: head.split(' ').nth(1)?.parse().ok()?,
        body: bytes[split + 4..].to_vec(),
    })
}

fn connect(addr: SocketAddr) -> Result<TcpStream, String> {
    let stream = TcpStream::connect(addr).map_err(|_| "connect")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|_| "timeout")?;
    Ok(stream)
}

fn post(addr: SocketAddr, path: &str, body: &[u8]) -> Result<Reply, String> {
    let mut stream = connect(addr)?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: worker\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|_| "write")?;
    read_reply(&mut stream).ok_or_else(|| "no_reply".to_owned())
}

/// A valid execute request for `component`; every case builds on it.
fn request(n: u32, component: &[u8], imports: &[&str], base: Option<Value>) -> Value {
    let (activation, attempt) = (format!("act-pod-{n}"), format!("att-pod-{n}"));
    let artifact = digest(component);
    let mut projection = json!({
        "revision": "rev-pod-0001",
        "runtime_mode": "wasm",
        "artifact_digest": artifact,
        "component_digest": artifact,
        "language_profile": {"name": "profile-pod", "version": "1"},
        "engine_version": ENGINE_VERSION,
        "wasi_version": "0.2.6",
        "interface_version": "0.2.0",
        "imports": imports,
        "budgets": {"fuel": 1, "memory_bytes": 1, "timeout_ms": 10000, "max_host_calls": 1},
        "grants": [{
            "family": "http",
            "interface": "falcone:host/http@0.2.0",
            "scope": {"hosts": ["api.example.test"]}
        }]
    });
    if let Some(base) = base {
        projection["base"] = base;
    }
    json!({
        "version": "1", "type": "execute",
        "activation_id": activation, "attempt_id": attempt, "attempt_number": 1,
        "deadline": "2099-01-01T00:00:00Z",
        "invocation": {"invocation_key": format!("inv-pod-{n}")},
        "credential": {
            "iss": "canary-issuer-0001", "aud": "canary-audience-0001",
            "tenant": "canary-tenant-0001", "workspace": "canary-workspace-0001",
            "pool_profile": "canary-pool-0001",
            "revision": "rev-pod-0001", "activation": activation, "attempt": attempt,
            "iat": 1, "exp": 2,
            "allowed_capabilities": [{
                "family": "http",
                "interface": "falcone:host/http@0.1.0",
                "scope": {"hosts": [CANARY]}
            }]
        },
        "projection": projection,
        "params": {"__ow_path": "/v1/pod", "name": "world"}
    })
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

fn cancel_body(request: &Value) -> Vec<u8> {
    bytes(&json!({
        "version": "1", "type": "cancel",
        "activation_id": request["activation_id"], "attempt_id": request["attempt_id"],
        "cancel": {"state": "requested"}
    }))
}

fn update_body(request: &Value) -> Vec<u8> {
    bytes(&json!({
        "version": "1", "type": "update",
        "activation_id": request["activation_id"], "attempt_id": request["attempt_id"],
        "sequence": 1, "update": {"kind": "heartbeat"}
    }))
}

/// Waits until the guest of `request` has run: the spinning guest reports progress 1 of 1 as its
/// first act and the worker serves it through the update operation. An execute slot says only
/// that a request was admitted, not that its guest runs.
fn wait_until_running(addr: SocketAddr, request: &Value) -> Outcome {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let reply = post(addr, "/v1/update", &update_body(request))?;
        if reply.status == 200 && reply.json()?["update"]["kind"] == "progress" {
            return Ok(());
        }
        check(Instant::now() < deadline, "guest_did_not_start")?;
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The service under test, with its own scratch directory.
struct Harness {
    running: Running,
    store: Arc<Store>,
    directory: std::path::PathBuf,
    recoveries: usize,
}

impl Harness {
    fn start(concurrency: u32, http: HttpLimits) -> Result<Self, String> {
        let directory = std::env::temp_dir().join(format!("run01-api-{}", std::process::id()));
        std::fs::create_dir(&directory).map_err(|_| "scratch_create")?;
        let exe = std::env::current_exe().map_err(|_| "executable_path")?;
        let compiler = exe
            .parent()
            .ok_or("executable_directory")?
            .join("artifact-compile-child");
        let store = Arc::new(Store {
            fetches: AtomicUsize::new(0),
        });
        let mut config = Config::new(compiler, directory.join("cache"), store.clone());
        config.concurrency = concurrency;
        config.http = http;
        let server = Server::bind(SocketAddr::from(([127, 0, 0, 1], 0)), config)
            .map_err(|_| "server_bind")?;
        Ok(Self {
            running: server.spawn(),
            store,
            directory,
            recoveries: 0,
        })
    }

    fn addr(&self) -> SocketAddr {
        self.running.addr()
    }

    fn execute(&self, request: &Value) -> Result<Reply, String> {
        post(self.addr(), "/v1/execute", &bytes(request))
    }

    /// No guest ran for anything but the recovery checks.
    fn no_guest_ran(&self) -> Outcome {
        check(self.running.instances() == self.recoveries, "a_guest_ran")
    }

    /// Exactly `guests` guests ran, besides the recovery checks.
    fn no_guest_ran_beyond(&self, guests: usize) -> Outcome {
        check(
            self.running.instances() == self.recoveries + guests,
            "an_unexpected_guest_ran",
        )
    }

    /// After a case: a normal execute and a cancel both succeed, and no slot is still held.
    fn recovered(&mut self) -> Outcome {
        self.recoveries += 1;
        let request = request(1000, ECHO, &[], None);
        check(self.execute(&request)?.status == 200, "execute_after_case")?;
        let cancel = post(self.addr(), "/v1/cancel", &cancel_body(&request))?;
        check(cancel.status == 200, "cancel_after_case")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.running.slots() != (0, 0, 0)
            || self.running.overflow() != 0
            || self.running.heads() != 0
        {
            check(Instant::now() < deadline, "slot_leaked")?;
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn identity() -> Outcome {
    let mut harness = Harness::start(2, HttpLimits::default())?;
    let catalog = json!({"kind": "catalog", "id": "base-pod", "version": "1.0.0",
        "digest": format!("sha256:{}", "2".repeat(64))});
    for (n, base) in [(1, None), (2, Some(catalog))] {
        let request = request(n, ECHO, &[], base.clone());
        let reply = harness.execute(&request)?;
        check(reply.status == 200, "execute_status")?;
        let parsed = api::parse_request(&bytes(&request)).map_err(|_| "request_rejected")?;
        api::parse_result(&reply.body, &parsed).map_err(|_| "result_not_accepted")?;
        let result = reply.json()?;
        let identity = &result["identity"];
        let projection = &request["projection"];
        check(result["status"] == "success", "status")?;
        check(
            ["activation_id", "attempt_id"]
                .iter()
                .all(|field| result[*field] == request[*field])
                && result["attempt_number"] == 1,
            "attempt_fields",
        )?;
        check(
            result["result"]["body"] == request["params"],
            "handler_result",
        )?;
        check(
            identity["function_revision"] == projection["revision"]
                && identity["runtime_mode"] == "wasm"
                && identity["artifact_digest"] == projection["artifact_digest"]
                && identity["wasmtime_version"] == ENGINE_VERSION
                && identity["language_profile"] == projection["language_profile"]
                && identity["interface_version"] == "0.2.0",
            "identity_fields",
        )?;
        let base_digest = base.as_ref().map(|base| base["digest"].clone());
        check(
            identity.get("base_digest").cloned() == base_digest,
            "base_digest",
        )?;
    }
    harness.recovered()
}

fn non_wasm() -> Outcome {
    let mut harness = Harness::start(1, HttpLimits::default())?;
    let byo = json!({"kind": "byo", "image_digest": format!("sha256:{}", "3".repeat(64)),
        "signer_identity": "signer-pod", "attestation_ref": "attestation-pod"});
    let mut cases = Vec::new();
    // An image-based revision, also failing every later check (no path, undeclared import).
    let mut oci = request(10, ECHO, &[], Some(byo));
    oci["projection"]["runtime_mode"] = json!("oci");
    cases.push(oci.clone());
    oci["params"] = json!({"name": "no path"});
    oci["projection"]["imports"] = json!(["wasi:cli/environment@0.2.0"]);
    cases.push(oci);
    for mode in ["wasm2", "OCI", "native", ""] {
        let mut unknown = request(11, ECHO, &[], None);
        unknown["projection"]["runtime_mode"] = json!(mode);
        cases.push(unknown);
    }
    for case in cases {
        let reply = harness.execute(&case)?;
        check(reply.status == 409, "refusal_status")?;
        let refusal = reply.json()?;
        let envelope = refusal.get("result").unwrap_or(&refusal);
        check(
            envelope["code"] == "runtime_mode_not_supported"
                && envelope["detail"]["reason"] == "runtime_mode_not_wasm"
                && envelope["retryable"] == false,
            "refusal_envelope",
        )?;
        let text = String::from_utf8_lossy(&reply.body).into_owned();
        check(
            ["canary-", "signer-pod", "wasm2", "native"]
                .iter()
                .all(|value| !text.contains(value)),
            "refusal_carries_a_value",
        )?;
    }
    harness.no_guest_ran()?;
    check(harness.running.compiles() == 0, "compile_started")?;
    check(
        harness.store.fetches.load(Ordering::SeqCst) == 0,
        "artifact_fetched",
    )?;
    harness.recovered()
}

fn legacy() -> Outcome {
    let mut harness = Harness::start(1, HttpLimits::default())?;
    let mut request = request(20, ECHO, &[], None);
    request["params"] =
        json!({"__ow_headers": {"content-type": "application/json"}, "__ow_body": "not json"});
    let expected = [
        (400, "invalid_request", "missing_path"),
        (400, "invalid_request", "non_json_body"),
        (413, "request_too_large", "body_too_large"),
    ];
    for (step, (status, code, reason)) in expected.into_iter().enumerate() {
        match step {
            1 => request["params"]["__ow_path"] = json!("/x"),
            2 => {
                request["params"]["__ow_body"] =
                    json!(format!("{{\"a\":\"{}\"", "x".repeat(1_048_576)));
            }
            _ => {}
        }
        let reply = harness.execute(&request)?;
        let result = reply.json()?;
        check(
            reply.status == status
                && result["result"]["code"] == code
                && result["result"]["detail"]["reason"] == reason,
            "legacy_refusal",
        )?;
    }
    harness.no_guest_ran()?;
    check(
        harness.store.fetches.load(Ordering::SeqCst) == 0,
        "artifact_fetched",
    )?;
    harness.recovered()
}

/// The credential is absent from everything a guest can see: it has no environment or file to
/// read (an ambient WASI import is refused even when declared), its memory holds only the params,
/// and it is redacted from the logs the worker returns. The worker's own environment does not
/// hold it either.
fn credential() -> Outcome {
    let mut harness = Harness::start(1, HttpLimits::default())?;
    for ambient in [
        "wasi:cli/environment@0.2.0",
        "wasi:filesystem/preopens@0.2.0",
    ] {
        let reply = harness.execute(&request(30, WASI, &[ambient], None))?;
        let result = reply.json()?;
        check(
            reply.status == 422 && result["result"]["code"] == "undeclared_import",
            "ambient_import_not_refused",
        )?;
    }
    harness.no_guest_ran()?;
    let clean = harness.execute(&request(31, IMPORTS, &[LOGGING, CONTEXT], None))?;
    let logs = clean.json()?["logs"].clone();
    let lines = logs.as_array().ok_or("logs")?;
    check(lines.len() == 2, "log_lines")?;
    check(
        lines.iter().all(|line| {
            line.as_str()
                .is_some_and(|line| !line.contains(MARKER) && line.contains("__ow_path"))
        }),
        "credential_in_guest_memory",
    )?;
    let mut leaking = request(32, IMPORTS, &[LOGGING, CONTEXT], None);
    leaking["params"] = json!({"__ow_path": "/x", "leak": CANARY, "again": "canary-tenant-0001"});
    let logs = harness.execute(&leaking)?.json()?["logs"].to_string();
    check(
        !logs.contains("canary-") && logs.contains(MARKER),
        "credential_not_redacted",
    )?;
    check(
        std::env::vars().all(|(name, value)| !name.contains("canary") && !value.contains("canary")),
        "credential_in_worker_environment",
    )?;
    harness.recovered()
}

fn imports() -> Outcome {
    let mut harness = Harness::start(1, HttpLimits::default())?;
    let refused = |reply: Reply| -> Outcome {
        let result = reply.json()?;
        check(
            reply.status == 422
                && result["result"]["code"] == "undeclared_import"
                && result["result"]["retryable"] == false
                && result["class"] == "non_retryable",
            "undeclared_import",
        )
    };
    // An ungated interface with `imports: []`, a list missing one import, and a declared import
    // that is not allow-listed.
    refused(harness.execute(&request(40, IMPORTS, &[], None))?)?;
    refused(harness.execute(&request(41, IMPORTS, &[LOGGING], None))?)?;
    refused(harness.execute(&request(42, WASI, &["wasi:cli/environment@0.2.0"], None))?)?;
    check(harness.running.compiles() == 0, "compile_started")?;
    harness.no_guest_ran()?;
    let reply = harness.execute(&request(43, IMPORTS, &[LOGGING, CONTEXT], None))?;
    check(
        reply.status == 200 && reply.json()?["status"] == "success",
        "declared_import_run",
    )?;
    harness.recovered()
}

fn saturation() -> Outcome {
    let concurrency = 2u32;
    let mut harness = Harness::start(concurrency, HttpLimits::default())?;
    check(
        harness.running.connection_limit() == (concurrency + CONTROL_RESERVE) as usize,
        "connection_limit",
    )?;
    let spinners = spin(&harness, concurrency)?;
    // An extra execute gets 503 busy at once and holds no connection.
    let started = Instant::now();
    let extra = harness.execute(&request(79, ECHO, &[], None))?;
    check(
        started.elapsed() < Duration::from_secs(2),
        "busy_not_immediate",
    )?;
    check(
        extra.status == 503
            && extra.json()?["code"] == "busy"
            && extra.json()?["retryable"] == true,
        "busy_reply",
    )?;
    check(
        harness.running.slots() == (concurrency as usize, concurrency as usize, 0)
            && harness.running.overflow() == 0,
        "busy_held_a_slot",
    )?;
    // The contract's admission comes before capacity: a runtime mode other than wasm is still the
    // non-retryable 409 refusal while every execute slot is held.
    for mode in ["oci", "wasm2"] {
        let mut other = request(78, ECHO, &[], None);
        other["projection"]["runtime_mode"] = json!(mode);
        let refused = harness.execute(&other)?;
        let body = refused.json()?;
        let envelope = body.get("result").unwrap_or(&body);
        check(
            refused.status == 409
                && envelope["code"] == "runtime_mode_not_supported"
                && envelope["detail"]["reason"] == "runtime_mode_not_wasm"
                && envelope["retryable"] == false,
            "non_wasm_under_saturation",
        )?;
    }
    // A cancel is accepted with every execute slot held, and takes effect within its deadline.
    cancel_all(&harness, spinners, "cancel_not_acknowledged")?;
    harness.recovered()
}

/// A running execute request and the thread waiting for its reply.
type Spinner = (Value, std::thread::JoinHandle<Result<Reply, String>>);

/// `concurrency` execute requests whose guests block, each confirmed running.
fn spin(harness: &Harness, concurrency: u32) -> Result<Vec<Spinner>, String> {
    let spinners: Vec<_> = (0..concurrency)
        .map(|n| {
            let request = request(70 + n, SPIN, &[PROGRESS], None);
            let (addr, body) = (harness.addr(), bytes(&request));
            let handle = std::thread::spawn(move || post(addr, "/v1/execute", &body));
            (request, handle)
        })
        .collect();
    for (request, _) in &spinners {
        wait_until_running(harness.addr(), request)?;
    }
    check(
        harness.running.slots().1 == concurrency as usize,
        "execute_slots_not_held",
    )?;
    Ok(spinners)
}

/// Cancels each blocked guest and checks it takes effect within the deadline.
fn cancel_all(harness: &Harness, spinners: Vec<Spinner>, starved: &str) -> Outcome {
    for (request, handle) in spinners {
        let started = Instant::now();
        let cancel = post(harness.addr(), "/v1/cancel", &cancel_body(&request))?;
        check(
            cancel.status == 200 && cancel.json()?["cancel"]["state"] == "acknowledged",
            starved,
        )?;
        check(
            started.elapsed() < Duration::from_secs(5),
            "cancel_over_deadline",
        )?;
        let ended = handle.join().map_err(|_| "spinner_panicked")??;
        check(
            ended.json()?["result"]["detail"]["reason"] == "cancelled",
            "execution_not_cancelled",
        )?;
    }
    Ok(())
}

fn wait_for(deadline: Duration, reason: &str, mut done: impl FnMut() -> bool) -> Outcome {
    let until = Instant::now() + deadline;
    while !done() {
        check(Instant::now() < until, reason)?;
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// Whether the peer closed `stream` (EOF or a terminal connection error). A read that merely times
/// out or would block means the socket is still open; any reply bytes are drained first.
fn closed(stream: &TcpStream) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
    let mut probe = [0u8; 256];
    loop {
        match (&*stream).read(&mut probe) {
            Ok(0) => return true,
            Ok(_) => continue,
            Err(error) => {
                return !matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                );
            }
        }
    }
}

/// Excess execute requests that send valid headers and then stall their bodies cannot take a
/// connection a cancel needs: with every execute slot held by a blocked guest and the overflow
/// pool full of stalled bodies, a complete non-wasm request still gets its non-retryable 409 and a
/// wasm one `503 busy`, more stalled bodies replace the oldest ones, a cancel is still accepted
/// within its deadline, and everything recovers.
fn saturation_stalled() -> Outcome {
    let concurrency = 2u32;
    let mut harness = Harness::start(concurrency, HttpLimits::default())?;
    let spinners = spin(&harness, concurrency)?;
    let reserve = CONTROL_RESERVE as usize;
    let stall = |harness: &Harness| -> Result<TcpStream, String> {
        let mut stream = connect(harness.addr())?;
        stream
            .write_all(b"POST /v1/execute HTTP/1.1\r\nContent-Length: 1000000\r\n\r\n{")
            .map_err(|_| "write")?;
        Ok(stream)
    };
    // One at a time: each waits in the overflow pool until the pool is full.
    let mut streams = Vec::new();
    for n in 0..reserve {
        streams.push(stall(&harness)?);
        wait_for(Duration::from_secs(4), "overflow_pool_not_filled", || {
            harness.running.overflow() == n + 1
        })?;
    }
    // With the pool full, a complete request is still read: the contract refuses a runtime mode
    // other than wasm first, and a wasm request is answered busy.
    let mut other = request(61, ECHO, &[], None);
    other["projection"]["runtime_mode"] = json!("oci");
    let refused = harness.execute(&other)?;
    let body = refused.json()?;
    let envelope = body.get("result").unwrap_or(&body);
    check(
        refused.status == 409
            && envelope["detail"]["reason"] == "runtime_mode_not_wasm"
            && envelope["retryable"] == false,
        "non_wasm_with_full_overflow",
    )?;
    let busy = harness.execute(&request(62, ECHO, &[], None))?;
    check(
        busy.status == 503 && busy.json()?["code"] == "busy",
        "busy_with_full_overflow",
    )?;
    // More stalled bodies than the pool holds replace the oldest instead of queueing behind them.
    for _ in 0..reserve + 2 {
        streams.push(stall(&harness)?);
    }
    wait_for(
        Duration::from_secs(4),
        "old_stalled_body_not_replaced",
        || streams[..reserve + 2].iter().all(closed) && harness.running.overflow() == reserve,
    )?;
    // The stalled executes hold no connection: only the running guests are counted.
    check(
        harness.running.slots() == (concurrency as usize, concurrency as usize, 0),
        "stalled_execute_held_a_connection",
    )?;
    // Cancels are accepted while the stalled executes are still open, within the deadline.
    cancel_all(&harness, spinners, "cancel_starved_by_stalled_executes")?;
    drop(streams);
    wait_for(Duration::from_secs(10), "overflow_slot_leaked", || {
        harness.running.overflow() == 0
    })?;
    harness.no_guest_ran_beyond(concurrency as usize)?;
    harness.recovered()
}

/// Peers that connect and never finish their request line and headers cannot take the capacity a
/// cancel needs: with every execute slot held by a blocked guest and more incomplete heads than
/// the connection limit, a cancel is still accepted within its deadline, whichever route the
/// stalled heads announce, and everything recovers.
fn saturation_headers() -> Outcome {
    let concurrency = 2u32;
    let mut harness = Harness::start(concurrency, HttpLimits::default())?;
    let spinners = spin(&harness, concurrency)?;
    let limit = harness.running.connection_limit();
    let mut streams = Vec::new();
    for route in ["execute", "cancel"] {
        for _ in 0..limit + 3 {
            let mut stream = connect(harness.addr())?;
            stream
                .write_all(format!("POST /v1/{route} HTTP/1.1\r\n").as_bytes())
                .map_err(|_| "write")?;
            streams.push(stream);
        }
        wait_for(Duration::from_secs(4), "header_pool_not_filled", || {
            harness.running.heads() >= limit
        })?;
        check(
            harness.running.slots() == (concurrency as usize, concurrency as usize, 0),
            "stalled_head_held_a_slot",
        )?;
    }
    cancel_all(&harness, spinners, "cancel_starved_by_stalled_heads")?;
    drop(streams);
    wait_for(Duration::from_secs(10), "head_slot_leaked", || {
        harness.running.heads() == 0
    })?;
    harness.no_guest_ran_beyond(concurrency as usize)?;
    harness.recovered()
}

fn http_headers() -> Outcome {
    let limits = HttpLimits::default();
    let mut harness = Harness::start(1, limits)?;
    let mut stream = connect(harness.addr())?;
    let head = format!(
        "POST /v1/execute HTTP/1.1\r\nContent-Length: 2\r\nX-Pad: {}\r\n\r\n{{}}",
        "a".repeat(limits.max_headers)
    );
    let _ = stream.write_all(head.as_bytes());
    // 431, or the connection is closed; never a guest.
    check(
        read_reply(&mut stream).is_none_or(|reply| reply.status == 431),
        "oversized_headers_status",
    )?;
    harness.no_guest_ran()?;
    harness.recovered()
}

fn http_request() -> Outcome {
    let limits = HttpLimits::default();
    let mut harness = Harness::start(1, limits)?;
    let mut stream = connect(harness.addr())?;
    let head = format!(
        "POST /v1/execute HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        limits.max_request + 1
    );
    stream.write_all(head.as_bytes()).map_err(|_| "write")?;
    let reply = read_reply(&mut stream).ok_or("no_reply")?;
    check(
        reply.status == 413 && reply.json()?["code"] == "request_too_large",
        "oversized_request_status",
    )?;
    harness.no_guest_ran()?;
    harness.recovered()
}

fn http_action_body() -> Outcome {
    let mut harness = Harness::start(1, HttpLimits::default())?;
    let mut request = request(90, ECHO, &[], None);
    request["params"]["__ow_body"] = json!("x".repeat(1_048_577));
    let reply = harness.execute(&request)?;
    check(
        reply.status == 413 && reply.json()?["result"]["detail"]["reason"] == "body_too_large",
        "oversized_action_body_status",
    )?;
    harness.no_guest_ran()?;
    check(
        harness.store.fetches.load(Ordering::SeqCst) == 0,
        "artifact_fetched",
    )?;
    harness.recovered()
}

fn closed_within(stream: &mut TcpStream, started: Instant, bound: Duration) -> Outcome {
    check(read_reply(stream).is_none(), "reply_to_a_stalled_request")?;
    check(started.elapsed() < bound, "closed_after_its_bound")
}

fn http_silent() -> Outcome {
    let limits = HttpLimits::default();
    let mut harness = Harness::start(1, limits)?;
    let mut stream = connect(harness.addr())?;
    closed_within(
        &mut stream,
        Instant::now(),
        limits.header_deadline + Duration::from_secs(2),
    )?;
    harness.recovered()
}

fn http_drip() -> Outcome {
    let limits = HttpLimits::default();
    let mut harness = Harness::start(1, limits)?;
    let mut stream = connect(harness.addr())?;
    let mut dripper = stream.try_clone().map_err(|_| "clone")?;
    let started = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    // A header line every third of the inactivity timeout: no read ever stalls, yet the section
    // as a whole passes the absolute deadline. The reader below sees the close when it happens.
    let drip = std::thread::spawn({
        let stop = stop.clone();
        move || {
            let _ = dripper.write_all(b"POST /v1/execute HTTP/1.1\r\n");
            while !stop.load(Ordering::SeqCst) && started.elapsed() < limits.header_deadline * 3 {
                if dripper.write_all(b"X-A: b\r\n").is_err() {
                    break;
                }
                std::thread::sleep(limits.io_timeout / 3);
            }
        }
    });
    let closed = closed_within(
        &mut stream,
        started,
        limits.header_deadline + Duration::from_secs(2),
    );
    stop.store(true, Ordering::SeqCst);
    let _ = drip.join();
    closed?;
    harness.recovered()
}

fn http_stall() -> Outcome {
    let limits = HttpLimits::default();
    let mut harness = Harness::start(1, limits)?;
    let mut stream = connect(harness.addr())?;
    let started = Instant::now();
    stream
        .write_all(b"POST /v1/execute HTTP/1.1\r\nContent-Length: 100\r\n\r\n{")
        .map_err(|_| "write")?;
    closed_within(
        &mut stream,
        started,
        limits.io_timeout + Duration::from_secs(2),
    )?;
    harness.no_guest_ran()?;
    harness.recovered()
}

fn http_control_body() -> Outcome {
    let limits = HttpLimits::default();
    let mut harness = Harness::start(1, limits)?;
    // A cancel and an update whose body is over the 16 KiB control ceiling get 413 before the body
    // is read, and no guest runs.
    for path in ["/v1/cancel", "/v1/update"] {
        let mut stream = connect(harness.addr())?;
        let head = format!(
            "POST {path} HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            limits.max_control_body + 1
        );
        stream.write_all(head.as_bytes()).map_err(|_| "write")?;
        let reply = read_reply(&mut stream).ok_or("no_reply")?;
        check(
            reply.status == 413 && reply.json()?["code"] == "request_too_large",
            "oversized_control_body_status",
        )?;
    }
    harness.no_guest_ran()?;
    harness.recovered()
}

fn http_body_drip() -> Outcome {
    let limits = HttpLimits::default();
    let mut harness = Harness::start(1, limits)?;
    let mut stream = connect(harness.addr())?;
    let mut dripper = stream.try_clone().map_err(|_| "clone")?;
    let started = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    // One body byte every third of the inactivity timeout: no read ever stalls, yet the body as
    // a whole passes the absolute BODY_DEADLINE (30 s). The announced length is never reached.
    let drip = std::thread::spawn({
        let stop = stop.clone();
        move || {
            let head = b"POST /v1/execute HTTP/1.1\r\nContent-Length: 1000000\r\n\r\n";
            if dripper.write_all(head).is_err() {
                return;
            }
            while !stop.load(Ordering::SeqCst) && started.elapsed() < limits.body_deadline * 2 {
                if dripper.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(limits.io_timeout / 3);
            }
        }
    });
    let closed = closed_within(
        &mut stream,
        started,
        limits.body_deadline + Duration::from_secs(2),
    );
    stop.store(true, Ordering::SeqCst);
    let _ = drip.join();
    closed?;
    // Closed by the absolute deadline, not by the shorter inactivity timeout.
    check(
        started.elapsed() >= limits.body_deadline - Duration::from_secs(1),
        "closed_by_inactivity_not_by_the_body_deadline",
    )?;
    harness.no_guest_ran()?;
    harness.recovered()
}

/// Runs one case and prints its evidence record, then the success record the harness waits for.
pub fn run(name: &str) -> ExitCode {
    let Some((_, group, case)) = CASES.iter().find(|(candidate, _, _)| *candidate == name) else {
        eprintln!("unknown_execute_api_case");
        return ExitCode::FAILURE;
    };
    let started = Instant::now();
    match case() {
        Ok(()) => {
            println!(
                "{}",
                json!({"case": name, "group": group, "status": "passed",
                    "elapsed_ms": started.elapsed().as_millis() as u64})
            );
            println!("{}", json!({"check": "execute_api", "status": "passed"}));
            ExitCode::SUCCESS
        }
        Err(reason) => {
            println!(
                "{}",
                json!({"case": name, "group": group, "status": "failed", "reason": reason})
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn closed_tells_a_live_socket_from_a_closed_one() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        // Negative control: an open, silent socket times out its read and is not closed.
        assert!(!closed(&client));
        drop(peer);
        assert!(closed(&client));
    }
}
