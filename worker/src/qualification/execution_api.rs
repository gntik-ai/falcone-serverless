//! RUN-01.t3 qualification of the worker execution API over real TCP.
//!
//! Each case starts its own `falcone-serverless-worker serve` process, talks to both listeners
//! over TCP and stops it. The same cases run under `cargo test` (the `execute_api_local`
//! integration test) and inside the restricted fs-test pod (`--execution-api=<case>` of the
//! qualification runner, started by the `cluster_execute_api` integration test).
//!
//! Cases synchronize on the worker's structured events (`listening`, `admitted`,
//! `execution_started`, `execution_finished`) and on responses, never on sleeps shorter than the
//! documented deadlines: a stalled connection is waited for until its documented deadline.
//! Nothing here prints a credential value; the fixtures carry only synthetic marker strings.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const ECHO: &[u8] = include_bytes!("execute_fixtures/handler.wasm");
const BLOCK: &[u8] = include_bytes!("execute_fixtures/api-block.wasm");
const CONTEXT: &[u8] = include_bytes!("execute_fixtures/api-context.wasm");
const LOGGER: &[u8] = include_bytes!("execute_fixtures/api-log.wasm");
const LOG_MANY: &[u8] = include_bytes!("execute_fixtures/api-log-many.wasm");
const WASI: &[u8] = include_bytes!("execute_fixtures/wasi-import.wasm");

const CONTEXT_IMPORT: &str = "falcone:host/invocation-context@0.2.0";
const LOGGING_IMPORT: &str = "falcone:host/logging@0.2.0";

/// The documented limits of docs/contracts/worker-execution-api/execution-api-v1.md.
const MAX_HEADERS: usize = 16 * 1024;
const MAX_REQUEST: usize = 8 * 1024 * 1024;
const MAX_ACTION_BODY: usize = 1_048_576;
const MAX_CONTROL_BODY: usize = 64 * 1024;
const ADMISSION_ALLOWANCE: usize = 4;
const EXECUTE_HEADER_DEADLINE: Duration = Duration::from_secs(5);
const EXECUTE_BODY_IDLE: Duration = Duration::from_secs(5);
const EXECUTE_BODY_TOTAL: Duration = Duration::from_secs(30);
/// Slack on top of a documented deadline for scheduling and the 250 ms response drain.
const SLACK: Duration = Duration::from_secs(3);
const WAIT: Duration = Duration::from_secs(60);

type Check = Result<(), String>;

fn ensure(condition: bool, what: &str) -> Check {
    if condition {
        Ok(())
    } else {
        Err(what.to_owned())
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------------------------
// The worker process and its events

struct Events {
    seen: Mutex<Vec<Value>>,
    changed: Condvar,
}

impl Events {
    fn matching(&self, name: &str, listener: Option<&str>) -> usize {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|event| {
                event["event"] == name && listener.is_none_or(|l| event["listener"] == l)
            })
            .count()
    }
}

pub struct Config {
    pub concurrency: u32,
    pub control_connections: u32,
    pub control_deadline_ms: u64,
    pub cancel_reserve_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            concurrency: 1,
            control_connections: 8,
            control_deadline_ms: 5000,
            cancel_reserve_ms: 5000,
        }
    }
}

struct Worker {
    child: Child,
    root: PathBuf,
    execute: SocketAddr,
    control: SocketAddr,
    events: Arc<Events>,
}

impl Worker {
    fn start(binary: &Path, config: &Config) -> Result<Self, String> {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "execution-api-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let artifacts = root.join("cache/artifacts");
        std::fs::create_dir_all(&artifacts).map_err(|e| format!("scratch: {}", e.kind()))?;
        for component in [ECHO, BLOCK, CONTEXT, LOGGER, LOG_MANY, WASI] {
            let hex = digest(component).replace("sha256:", "");
            std::fs::write(artifacts.join(format!("{hex}.wasm")), component)
                .map_err(|e| format!("stage: {}", e.kind()))?;
        }
        let file = root.join("worker.toml");
        // Port 0 asks the OS for a free port; the `listening` event reports it.
        std::fs::write(
            &file,
            format!(
                "pool_id = \"qualification\"\nlisten_port = 0\ncontrol_port = 0\n\
                 concurrency = {}\ncontrol_max_connections = {}\n\
                 control_header_deadline_ms = {}\ncontrol_body_deadline_ms = {}\n\
                 cancel_reserve_ms = {}\ncache_path = \"{}\"\n\
                 falcone_api_endpoint = \"https://falcone.invalid/api\"\n\
                 falcone_dispatcher_endpoint = \"https://dispatcher.invalid\"\n",
                config.concurrency,
                config.control_connections,
                config.control_deadline_ms,
                config.control_deadline_ms,
                config.cancel_reserve_ms,
                root.join("cache").display(),
            ),
        )
        .map_err(|e| format!("config: {}", e.kind()))?;
        let mut child = Command::new(binary)
            .arg("serve")
            .arg(&file)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn worker: {}", e.kind()))?;
        let stderr = child.stderr.take().ok_or("worker stderr")?;
        let events = Arc::new(Events {
            seen: Mutex::new(Vec::new()),
            changed: Condvar::new(),
        });
        let reader = Arc::clone(&events);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Ok(event) = serde_json::from_str::<Value>(&line) {
                    reader
                        .seen
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(event);
                    reader.changed.notify_all();
                }
            }
            reader.changed.notify_all();
        });
        let mut worker = Self {
            child,
            root,
            execute: ([127, 0, 0, 1], 0).into(),
            control: ([127, 0, 0, 1], 0).into(),
            events,
        };
        worker.wait("listening", None, 1)?;
        let listening = worker.event("listening")?;
        let port = |key: &str| -> Result<u16, String> {
            listening[key]
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| "listening event has no port".to_owned())
        };
        worker.execute = ([127, 0, 0, 1], port("execute_port")?).into();
        worker.control = ([127, 0, 0, 1], port("control_port")?).into();
        Ok(worker)
    }

    fn event(&self, name: &str) -> Result<Value, String> {
        self.events
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|event| event["event"] == name)
            .cloned()
            .ok_or_else(|| format!("no {name} event"))
    }

    fn count(&self, name: &str) -> usize {
        self.events.matching(name, None)
    }

    /// Waits for at least `count` events named `name` (of `listener`, when given).
    fn wait(&self, name: &str, listener: Option<&str>, count: usize) -> Check {
        let deadline = Instant::now() + WAIT;
        let mut seen = self
            .events
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let have = seen
                .iter()
                .filter(|e| e["event"] == name && listener.is_none_or(|l| e["listener"] == l))
                .count();
            if have >= count {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "timed out waiting for {count} {name} events, saw {have}"
                ));
            }
            seen = self
                .events
                .changed
                .wait_timeout(seen, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    fn post(&self, listener: Listener, path: &str, body: &[u8]) -> Result<Response, String> {
        let addr = match listener {
            Listener::Execute => self.execute,
            Listener::Control => self.control,
        };
        exchange(addr, &http_request(path, body))?.ok_or_else(|| "connection closed".to_owned())
    }

    fn execute(&self, request: &Value) -> Result<Response, String> {
        self.post(
            Listener::Execute,
            "/v1/execute",
            request.to_string().as_bytes(),
        )
    }

    fn cancel(&self, activation: &str, attempt: &str) -> Result<Response, String> {
        let message = json!({
            "version": "1", "type": "cancel",
            "activation_id": activation, "attempt_id": attempt,
            "cancel": { "state": "requested" }
        });
        self.post(
            Listener::Control,
            "/v1/cancel",
            message.to_string().as_bytes(),
        )
    }

    /// A guest that never returns, started on its own thread; waits until it is running.
    fn block(&self, name: &str) -> Result<Blocker, String> {
        let before = self.count("execution_started");
        let worker = (
            self.execute,
            request(name, BLOCK, &[], json!({ "__ow_path": "/b" })),
        );
        let handle = std::thread::spawn(move || {
            exchange(
                worker.0,
                &http_request("/v1/execute", worker.1.to_string().as_bytes()),
            )
            .and_then(|r| r.ok_or_else(|| "connection closed".to_owned()))
        });
        self.wait("execution_started", None, before + 1)?;
        Ok(Blocker {
            name: name.to_owned(),
            handle,
        })
    }

    /// The server must serve a normal execute and a cancel right now: no capacity may have
    /// leaked on either listener.
    fn recovered(&self, label: &str) -> Check {
        let name = format!("recover-{label}");
        let response = self.execute(&request(&name, ECHO, &[], json!({ "__ow_path": "/r" })))?;
        ensure(
            response.status == 200,
            "a normal execute succeeds after the case",
        )?;
        let blocker = self.block(&format!("{name}-b"))?;
        blocker.cancelled(self)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone, Copy)]
enum Listener {
    Execute,
    Control,
}

struct Blocker {
    name: String,
    handle: std::thread::JoinHandle<Result<Response, String>>,
}

impl Blocker {
    /// Cancels it on the control listener within the documented deadline and checks that the
    /// execute response reports the cancellation.
    fn cancelled(self, worker: &Worker) -> Check {
        let started = Instant::now();
        let answer = worker.cancel(&self.name, &format!("{}-attempt", self.name))?;
        ensure(answer.status == 200, "cancel is accepted")?;
        ensure(
            answer.json()?["cancel"]["state"] == "acknowledged",
            "cancel is acknowledged",
        )?;
        ensure(
            started.elapsed() < Duration::from_secs(5),
            "cancel answered within the documented deadline",
        )?;
        let response = self
            .handle
            .join()
            .map_err(|_| "blocker thread panicked")??;
        ensure(response.status == 409, "the cancelled execute answers 409")?;
        // A failed execution keeps the envelope: the failure, the attempt and the pinned identity.
        let result = response.json()?;
        let message = request(&self.name, BLOCK, &[], json!({ "__ow_path": "/b" }));
        ensure(
            result["type"] == "result" && result["status"] == "failure",
            "the cancelled execute answers a failure result",
        )?;
        ensure(
            result["result"]["code"] == "execution_cancelled"
                && result["result"]["detail"]["reason"] == "cancelled"
                && result["result"]["status"] == 409
                && result["result"]["retryable"] == false
                && result["class"] == "non_retryable",
            "the cancelled execute says cancelled",
        )?;
        for key in ["activation_id", "attempt_id", "attempt_number"] {
            ensure(
                result[key] == message[key],
                "the failure repeats the attempt",
            )?;
        }
        let identity = &result["identity"];
        ensure(
            identity["function_revision"] == message["projection"]["revision"]
                && identity["runtime_mode"] == "wasm"
                && identity["artifact_digest"] == message["projection"]["artifact_digest"]
                && identity["wasmtime_version"] == "49.0.5",
            "the failure carries the pinned runtime identity",
        )
    }
}

// ---------------------------------------------------------------------------------------------
// HTTP client

struct Response {
    status: u16,
    body: Vec<u8>,
}

impl Response {
    fn json(&self) -> Result<Value, String> {
        serde_json::from_slice(&self.body).map_err(|_| "response body is not JSON".to_owned())
    }
}

fn http_request(path: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "POST {path} HTTP/1.1\r\nHost: worker\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn connect(addr: SocketAddr) -> Result<TcpStream, String> {
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| format!("connect: {}", e.kind()))?;
    stream
        .set_read_timeout(Some(WAIT))
        .map_err(|e| e.kind().to_string())?;
    stream.set_nodelay(true).map_err(|e| e.kind().to_string())?;
    Ok(stream)
}

/// Reads whatever the server sends until it closes. A reset after some data keeps the data.
fn read_response(stream: &mut TcpStream) -> Result<Option<Response>, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error)
                if bytes.is_empty() && error.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                return Ok(None);
            }
            Err(error) if bytes.is_empty() => return Err(format!("read: {}", error.kind())),
            Err(_) => break,
        }
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("response head is incomplete")?;
    let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or("response has no status")?;
    Ok(Some(Response {
        status,
        body: bytes[split + 4..].to_vec(),
    }))
}

/// Sends `bytes` (ignoring a server that closes early) and reads the answer, if any.
fn exchange(addr: SocketAddr, bytes: &[u8]) -> Result<Option<Response>, String> {
    let mut stream = connect(addr)?;
    let _ = stream.write_all(bytes);
    read_response(&mut stream)
}

/// Waits for the server to close `stream`; returns how long it took, or an error when it did
/// not close within `limit`.
fn closed_within(stream: &mut TcpStream, limit: Duration) -> Result<Duration, String> {
    let started = Instant::now();
    stream
        .set_read_timeout(Some(limit))
        .map_err(|e| e.kind().to_string())?;
    let mut chunk = [0_u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(started.elapsed()),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err("the connection was still open after its documented deadline".into());
            }
            Err(_) => return Ok(started.elapsed()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Request construction (synthetic values only)

const ISSUER: &str = "issuer-marker.example";
const AUDIENCE: &str = "audience-marker.example";
const PROFILE: &str = "profile-marker";
const TENANT: &str = "tenant-marker-1";
const WORKSPACE: &str = "workspace-marker-1";

/// A valid `wasm` execute request for the component. The attempt id is `<name>-attempt`.
fn request(name: &str, component: &[u8], imports: &[&str], params: Value) -> Value {
    let attempt = format!("{name}-attempt");
    json!({
        "version": "1",
        "type": "execute",
        "activation_id": name,
        "attempt_id": attempt,
        "attempt_number": 1,
        "deadline": "2030-01-01T00:00:00Z",
        "invocation": { "invocation_key": "inv-0000007" },
        "credential": {
            "iss": ISSUER, "aud": AUDIENCE, "tenant": TENANT, "workspace": WORKSPACE,
            "pool_profile": PROFILE, "revision": "rev-0003",
            "activation": name, "attempt": attempt,
            "iat": 1_794_398_400, "exp": 1_794_398_460,
            "allowed_capabilities": []
        },
        "projection": {
            "revision": "rev-0003",
            "runtime_mode": "wasm",
            "artifact_digest": digest(&[b"artifact:", component].concat()),
            "component_digest": digest(component),
            "language_profile": { "name": "profile-synthetic", "version": "2026.11.0" },
            "engine_version": "49.0.5",
            "wasi_version": "0.2.6",
            "interface_version": "0.2.0",
            "imports": imports,
            "budgets": { "fuel": 10_000_000, "memory_bytes": 67_108_864, "timeout_ms": 30_000, "max_host_calls": 1000 },
            "grants": []
        },
        "params": params
    })
}

// ---------------------------------------------------------------------------------------------
// AC1: the legacy envelope and the runtime identity

fn execute_envelope(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let mut with_base = request(
        "env-base",
        ECHO,
        &[],
        json!({ "__ow_method": "get", "__ow_path": "/hello", "name": "alpha" }),
    );
    let base_digest = format!("sha256:{}", "3".repeat(64));
    with_base["projection"]["base"] = json!({ "kind": "catalog", "id": "base-synthetic", "version": "1.4.2", "digest": base_digest });
    for (message, expect_base) in [
        (
            request(
                "env-plain",
                ECHO,
                &[],
                json!({ "__ow_method": "get", "__ow_path": "/hello", "name": "alpha" }),
            ),
            false,
        ),
        (with_base, true),
    ] {
        let response = worker.execute(&message)?;
        ensure(response.status == 200, "execute answers 200")?;
        let result = response.json()?;
        ensure(
            result["version"] == "1" && result["type"] == "result",
            "result message",
        )?;
        ensure(
            result["status"] == "success" && result.get("class").is_none(),
            "success without class",
        )?;
        for key in ["activation_id", "attempt_id", "attempt_number"] {
            ensure(result[key] == message[key], "the attempt is repeated")?;
        }
        ensure(
            result["result"]["statusCode"] == 200,
            "legacy handler result status",
        )?;
        ensure(
            result["result"]["body"] == message["params"],
            "the guest saw exactly the params",
        )?;
        ensure(result["logs"] == json!([]), "no logs")?;
        let identity = &result["identity"];
        let projection = &message["projection"];
        ensure(
            identity["function_revision"] == projection["revision"],
            "identity revision",
        )?;
        ensure(identity["runtime_mode"] == "wasm", "identity runtime mode")?;
        ensure(
            identity["artifact_digest"] == projection["artifact_digest"],
            "identity artifact digest",
        )?;
        ensure(
            identity["language_profile"] == projection["language_profile"],
            "identity language profile",
        )?;
        ensure(
            identity["interface_version"] == "0.2.0",
            "identity interface version",
        )?;
        // The pinned Wasmtime version, as a literal rather than recomputed from the request.
        ensure(
            identity["wasmtime_version"] == "49.0.5",
            "identity pinned Wasmtime version",
        )?;
        ensure(
            identity["wasmtime_version"] == projection["engine_version"],
            "identity engine version",
        )?;
        if expect_base {
            ensure(
                identity["base_digest"] == base_digest,
                "identity base digest",
            )?;
        } else {
            ensure(
                identity.get("base_digest").is_none(),
                "no base digest without a base",
            )?;
        }
    }
    ensure(
        worker.count("execution_started") == 2,
        "each execution created one instance",
    )?;
    // A request for another Wasmtime pin is refused, never run under a false identity.
    let mut other = request("env-pin", ECHO, &[], json!({ "__ow_path": "/p" }));
    other["projection"]["engine_version"] = json!("1.0.0");
    let refused = worker.execute(&other)?;
    ensure(refused.status == 409, "a different engine pin is refused")?;
    ensure(
        refused.json()?["error"] == "engine_version_mismatch",
        "engine pin error",
    )?;
    ensure(
        worker.count("execution_started") == 2,
        "the refusal created no instance",
    )
}

// ---------------------------------------------------------------------------------------------
// AC2: runtime mode precedence and contract refusals

fn runtime_mode(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let params = json!({ "__ow_path": "/m" });
    // An OCI revision, including one that names an image (a BYO base), is refused.
    let mut image = request("mode-oci", ECHO, &[], params.clone());
    image["projection"]["runtime_mode"] = json!("oci");
    image["projection"]["base"] = json!({
        "kind": "byo", "image_digest": format!("sha256:{}", "4".repeat(64)),
        "signer_identity": "signer-synthetic", "attestation_ref": "attestation-synthetic"
    });
    let refused = worker.execute(&image)?;
    ensure(refused.status == 409, "oci mode is refused with 409")?;
    let body = refused.json()?;
    ensure(
        body["status"] == "failure" && body["class"] == "non_retryable",
        "refusal envelope",
    )?;
    ensure(
        body["result"]["code"] == "runtime_mode_not_supported",
        "refusal code",
    )?;
    ensure(
        body["result"]["detail"]["reason"] == "runtime_mode_not_wasm",
        "refusal reason",
    )?;
    ensure(
        body["identity"]["runtime_mode"] == "oci",
        "the refusal identity names the revision",
    )?;
    // A runtime mode the schema does not know gets the same refusal.
    let mut unknown = request("mode-unknown", ECHO, &[], params.clone());
    unknown["projection"]["runtime_mode"] = json!("native");
    let refused = worker.execute(&unknown)?;
    ensure(refused.status == 409, "an unknown mode is refused with 409")?;
    ensure(
        refused.json()?["code"] == "runtime_mode_not_supported",
        "unknown mode code",
    )?;
    // Other contract refusals pass through unchanged.
    let mut missing = request("mode-path", ECHO, &[], json!({ "other": 1 }));
    missing["params"] = json!({ "other": 1 });
    let refused = worker.execute(&missing)?;
    ensure(refused.status == 400, "missing path is 400")?;
    let body = refused.json()?;
    ensure(
        body["result"]["code"] == "invalid_request",
        "missing path code",
    )?;
    ensure(
        body["result"]["detail"]["reason"] == "missing_path",
        "missing path reason",
    )?;
    let big = request(
        "mode-big",
        ECHO,
        &[],
        json!({ "__ow_path": "/m", "__ow_body": "a".repeat(MAX_ACTION_BODY + 1) }),
    );
    let refused = worker.execute(&big)?;
    ensure(refused.status == 413, "an oversized action body is 413")?;
    ensure(
        refused.json()?["result"]["code"] == "request_too_large",
        "oversized body code",
    )?;
    let non_json = request(
        "mode-json",
        ECHO,
        &[],
        json!({ "__ow_path": "/m", "__ow_headers": { "content-type": "application/json" }, "__ow_body": "{nope" }),
    );
    let refused = worker.execute(&non_json)?;
    ensure(refused.status == 400, "a non-JSON body is 400")?;
    ensure(
        refused.json()?["result"]["detail"]["reason"] == "non_json_body",
        "non-JSON reason",
    )?;
    ensure(
        worker.count("execution_started") == 0,
        "no refusal created an instance",
    )?;
    // Precedence: once the headers have arrived, a non-wasm request is refused with 409 even
    // when the worker is at capacity, while a wasm request is told it is busy.
    let blocker = worker.block("mode-busy")?;
    ensure(
        worker.execute(&image)?.status == 409,
        "runtime mode is decided before capacity",
    )?;
    let busy = worker.execute(&request("mode-extra", ECHO, &[], params))?;
    ensure(
        busy.status == 503 && busy.json()?["error"] == "busy",
        "a wasm request at capacity is busy",
    )?;
    ensure(
        worker.count("execution_started") == 1,
        "only the blocker ever had an instance",
    )?;
    blocker.cancelled(&worker)
}

// ---------------------------------------------------------------------------------------------
// AC3: the credential stays in the host binding layer

fn credential_boundary(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let markers = [ISSUER, AUDIENCE, PROFILE, TENANT, WORKSPACE];
    // The guest is given the params and nothing else: neither its input nor its output holds a
    // credential string.
    let echoed = worker.execute(&request(
        "cred-echo",
        ECHO,
        &[],
        json!({ "__ow_path": "/c" }),
    ))?;
    ensure(echoed.status == 200, "execute answers 200")?;
    let text = String::from_utf8_lossy(&echoed.body).into_owned();
    let result = echoed.json()?;
    let guest_view = format!("{}{}", result["result"], result["logs"]);
    for marker in markers {
        ensure(
            !guest_view.contains(marker),
            "no credential string reaches the guest or its result",
        )?;
    }
    ensure(text.contains("rev-0003"), "the identity is still reported")?;
    // A guest that logs what it was given: credential strings that appear in its input,
    // including one-character ones, are redacted from the returned logs.
    let mut logged = request(
        "cred-log",
        LOGGER,
        &[LOGGING_IMPORT],
        json!({ "__ow_path": "/c", "note": format!("{ISSUER} {TENANT} t w") }),
    );
    logged["credential"]["tenant"] = json!("t");
    logged["credential"]["workspace"] = json!("w");
    let response = worker.execute(&logged)?;
    ensure(response.status == 200, "the logging guest runs")?;
    let result = response.json()?;
    let lines = result["logs"].as_array().ok_or("logs is an array")?;
    ensure(lines.len() == 1, "one log line")?;
    let line = lines[0].as_str().ok_or("log line is text")?;
    ensure(
        line.contains("<redacted>"),
        "the line carries the redaction marker",
    )?;
    let remainder = line.replace("<redacted>", "");
    for secret in [ISSUER, "t", "w"] {
        ensure(
            !remainder.contains(secret),
            "no credential string survives in the logs",
        )?;
    }
    // Worker events are static text and numbers: no event carries a credential string.
    let events = worker
        .events
        .seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(Value::to_string)
        .collect::<String>();
    for marker in markers {
        ensure(
            !events.contains(marker),
            "no worker event carries a credential string",
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// AC5 end to end: bounded redaction inside the execution lifecycle

/// Distinct credential strings and the wall-clock bound for the whole request. The release
/// figures are the documented maximum (400,000 strings, redaction under one second; the bound
/// also covers parsing an ~5 MB request). The debug worker `cargo test` starts is an order of
/// magnitude slower and runs a smaller set under a larger bound. A quadratic or unbounded
/// implementation misses either bound by orders of magnitude.
fn redaction_scale() -> (usize, Duration) {
    if cfg!(debug_assertions) {
        (10_000, Duration::from_secs(30))
    } else {
        (400_000, Duration::from_secs(10))
    }
}

/// A request whose credential holds `entries` as storage prefixes (the projection grants
/// none, so nothing is authorized) and whose guest logs its params 1000 times.
fn many_credential_strings(name: &str, entries: Vec<String>, note: &str) -> Value {
    let mut request = request(
        name,
        LOG_MANY,
        &[LOGGING_IMPORT],
        json!({ "__ow_path": "/m", "note": note }),
    );
    request["credential"]["allowed_capabilities"] = json!([{
        "family": "storage",
        "interface": "falcone:host/storage@0.1.0",
        "scope": { "prefixes": entries }
    }]);
    request
}

fn redaction_load(binary: &Path) -> Check {
    // One execution slot: a slot leaked by either request below fails the next execute with 503.
    let worker = Worker::start(binary, &Config::default())?;
    // Many distinct credential strings, maximum log volume: 1000 lines of 8192 characters each,
    // every one repeating credential strings. Pattern bytes plus line bytes stay under the
    // budget, so the lines come back redacted, not dropped.
    let (count, bound) = redaction_scale();
    let entries: Vec<String> = (0..count).map(|n| format!("c{n:08}")).collect();
    let mut note = String::new();
    for n in [0, 42, count - 1] {
        note.push_str(&format!("c{n:08} "));
    }
    note.push_str(&"filler ".repeat(1400));
    let started = Instant::now();
    let response = worker.execute(&many_credential_strings("redact-many", entries, &note))?;
    let took = started.elapsed();
    ensure(response.status == 200, "the logging guest runs")?;
    ensure(
        took < bound,
        "redaction under load finishes within its bound",
    )?;
    let result = response.json()?;
    let lines = result["logs"].as_array().ok_or("logs is an array")?;
    ensure(lines.len() == 1000, "1000 log lines are kept")?;
    for line in lines {
        let line = line.as_str().ok_or("log line is text")?;
        ensure(line.contains("<redacted>"), "every line is redacted")?;
        ensure(
            !line.contains("c0000"),
            "no credential string survives in the logs",
        )?;
    }
    worker.recovered("redact-many")?;
    // Over the fixed work budget: 200 distinct credential strings of 30,000 bytes (each fits in a
    // line) plus the maximum log volume exceed 12 MiB. Fail closed: the only line returned is the
    // marker, and none of the text.
    let heavy: Vec<String> = (0..200)
        .map(|n| format!("c{n:08}{}", "p".repeat(30_000 - 9)))
        .collect();
    let started = Instant::now();
    let response = worker.execute(&many_credential_strings("redact-budget", heavy, &note))?;
    ensure(response.status == 200, "the over-budget request still runs")?;
    ensure(
        started.elapsed() < bound,
        "the fail-closed path finishes within its bound",
    )?;
    ensure(
        response.json()?["logs"] == json!(["<redacted>"]),
        "over budget, only the marker line is returned",
    )?;
    worker.recovered("redact-budget")?;
    // One credential string of 5 MiB can never occur in a log line: it costs no matcher, so the
    // request finishes within the bound instead of building a multi-million-state automaton.
    let started = Instant::now();
    let response = worker.execute(&many_credential_strings(
        "redact-huge",
        vec!["x".repeat(5 * 1024 * 1024)],
        &note,
    ))?;
    ensure(response.status == 200, "the huge-string request runs")?;
    ensure(
        started.elapsed() < bound,
        "a string longer than any line costs no redaction work",
    )?;
    worker.recovered("redact-huge")
}

// ---------------------------------------------------------------------------------------------
// AC4: declared imports

fn undeclared(response: &Response, import: &str) -> Check {
    let body = response.json()?;
    ensure(
        response.status == 422,
        &format!(
            "an undeclared import is 422, got {} {}",
            response.status, body["error"]
        ),
    )?;
    ensure(
        body["error"] == "undeclared_import",
        "error is undeclared_import",
    )?;
    ensure(body["import"] == import, "the offending import is named")
}

fn declared_imports(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let params = json!({ "__ow_path": "/i" });
    // Nothing is declared: the ungated Falcone interface is not allowed to run.
    let none = request("imp-none", CONTEXT, &[], params.clone());
    undeclared(&worker.execute(&none)?, CONTEXT_IMPORT)?;
    // A non-empty list that lacks the import is just as undeclared.
    let other = request("imp-other", CONTEXT, &[LOGGING_IMPORT], params.clone());
    undeclared(&worker.execute(&other)?, CONTEXT_IMPORT)?;
    // A declared import that is not in the allowed set is refused too.
    let wasi = request(
        "imp-wasi",
        WASI,
        &["wasi:cli/environment@0.2.0"],
        params.clone(),
    );
    undeclared(&worker.execute(&wasi)?, "wasi:cli/environment@0.2.0")?;
    ensure(
        worker.count("execution_started") == 0,
        "no guest code ran for a refused import",
    )?;
    // Fully declared imports run.
    let declared = request("imp-ok", CONTEXT, &[CONTEXT_IMPORT], params.clone());
    let response = worker.execute(&declared)?;
    ensure(response.status == 200, "a fully declared component runs")?;
    ensure(
        worker.count("execution_started") == 1,
        "one instance for the declared component",
    )?;
    // The component is now known to the worker; the declaration is still checked every time.
    undeclared(&worker.execute(&none)?, CONTEXT_IMPORT)?;
    ensure(
        worker.count("execution_started") == 1,
        "still no guest code for the undeclared call",
    )
}

// ---------------------------------------------------------------------------------------------
// AC6 and AC7: saturation and independent capacity

fn stalled_execute_connections(worker: &Worker) -> Result<Vec<TcpStream>, String> {
    let mut stalled = Vec::new();
    for index in 0..ADMISSION_ALLOWANCE {
        let mut stream = connect(worker.execute)?;
        if index % 2 == 1 {
            // Complete headers, then a body that never arrives.
            let head = "POST /v1/execute HTTP/1.1\r\nHost: worker\r\nContent-Length: 1000\r\n\r\n{";
            stream
                .write_all(head.as_bytes())
                .map_err(|e| e.kind().to_string())?;
        }
        stalled.push(stream);
    }
    Ok(stalled)
}

fn saturation(binary: &Path) -> Check {
    let worker = Worker::start(
        binary,
        &Config {
            concurrency: 2,
            ..Config::default()
        },
    )?;
    let first = worker.block("sat-1")?;
    let second = worker.block("sat-2")?;
    // Fill the rest of the execute listener with stalled connections (silent, and headers
    // without their body); wait until the server has admitted every one of them.
    let stalled = stalled_execute_connections(&worker)?;
    worker.wait("admitted", Some("execute"), 2 + ADMISSION_ALLOWANCE)?;
    // A further execute is answered 503 busy at once, without its body being read.
    let started = Instant::now();
    let extra = exchange(
        worker.execute,
        &http_request(
            "/v1/execute",
            request("sat-extra", ECHO, &[], json!({ "__ow_path": "/s" }))
                .to_string()
                .as_bytes(),
        ),
    )?
    .ok_or("the overflow connection got no answer")?;
    ensure(
        extra.status == 503 && extra.json()?["error"] == "busy",
        "overflow is 503 busy",
    )?;
    ensure(
        started.elapsed() < EXECUTE_HEADER_DEADLINE,
        "overflow is answered before any stalled connection times out",
    )?;
    // With every running slot and admission slot taken, a cancel on the control listener is
    // accepted and takes effect within the documented deadline.
    first.cancelled(&worker)?;
    // One running slot is free again: a normal execute succeeds although the stalled
    // connections still hold admission slots.
    let response = worker.execute(&request(
        "sat-after",
        ECHO,
        &[],
        json!({ "__ow_path": "/s" }),
    ))?;
    ensure(
        response.status == 200,
        "an execute succeeds once a slot is free",
    )?;
    drop(stalled);
    second.cancelled(&worker)?;
    let missing = worker.cancel("sat-gone", "sat-gone-attempt")?;
    ensure(
        missing.status == 404,
        "cancel of an execution that is not running is 404",
    )
}

fn control_stall(binary: &Path) -> Check {
    let deadline_ms = 2000;
    let worker = Worker::start(
        binary,
        &Config {
            control_connections: 4,
            control_deadline_ms: deadline_ms,
            ..Config::default()
        },
    )?;
    let blocker = worker.block("stall-1")?;
    let mut stalled = Vec::new();
    for index in 0..4 {
        let mut stream = connect(worker.control)?;
        if index % 2 == 1 {
            let head = "POST /v1/cancel HTTP/1.1\r\nHost: worker\r\nContent-Length: 100\r\n\r\n{";
            stream
                .write_all(head.as_bytes())
                .map_err(|e| e.kind().to_string())?;
        }
        stalled.push(stream);
    }
    worker.wait("admitted", Some("control"), 4)?;
    // The control listener is full: its own limit answers busy at once ...
    let full = worker.cancel("stall-1", "stall-1-attempt")?;
    ensure(
        full.status == 503 && full.json()?["error"] == "busy",
        "a full control listener is busy",
    )?;
    // ... and the running execution is unaffected: the execute listener still serves, and the
    // guest is still running.
    let busy = worker.execute(&request(
        "stall-extra",
        ECHO,
        &[],
        json!({ "__ow_path": "/s" }),
    ))?;
    ensure(
        busy.status == 503,
        "the execute listener is independent of the control listener",
    )?;
    ensure(
        worker.count("execution_finished") == 0,
        "the running execution was not disturbed",
    )?;
    // The stalled control connections are closed by the control deadlines.
    for stream in &mut stalled {
        let took = closed_within(stream, Duration::from_millis(deadline_ms) + SLACK)?;
        ensure(
            took <= Duration::from_millis(deadline_ms) + SLACK,
            "closed within the deadline",
        )?;
    }
    // The listener recovered: cancel works.
    blocker.cancelled(&worker)
}

fn cross_listener(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let body = request("cross-1", ECHO, &[], json!({ "__ow_path": "/x" })).to_string();
    // Control refuses execute, execute refuses cancel and update; no instance is created.
    let refused = worker.post(Listener::Control, "/v1/execute", body.as_bytes())?;
    ensure(
        refused.status == 404,
        "the control listener refuses execute",
    )?;
    for path in ["/v1/cancel", "/v1/update", "/v1/other", "/"] {
        let refused = worker.post(Listener::Execute, path, b"{}")?;
        ensure(
            refused.status == 404,
            "the execute listener serves only /v1/execute",
        )?;
    }
    for path in ["/v1/execute", "/v1/other", "/"] {
        let refused = worker.post(Listener::Control, path, b"{}")?;
        ensure(
            refused.status == 404,
            "the control listener serves only cancel and update",
        )?;
    }
    let get = exchange(
        worker.execute,
        b"GET /v1/execute HTTP/1.1\r\nHost: worker\r\n\r\n",
    )?
    .ok_or("GET got no answer")?;
    ensure(get.status == 405, "only POST is accepted")?;
    ensure(
        worker.count("execution_started") == 0,
        "no refused request created an instance",
    )?;
    // Each listener serves its own operation.
    ensure(
        worker
            .execute(&request("cross-2", ECHO, &[], json!({ "__ow_path": "/x" })))?
            .status
            == 200,
        "execute works",
    )?;
    let blocker = worker.block("cross-3")?;
    let update = |sequence: u64| {
        let message = json!({
            "version": "1", "type": "update",
            "activation_id": "cross-3", "attempt_id": "cross-3-attempt",
            "sequence": sequence, "update": { "kind": "heartbeat" }
        });
        worker.post(
            Listener::Control,
            "/v1/update",
            message.to_string().as_bytes(),
        )
    };
    ensure(
        update(1)?.status == 202,
        "an update of a running execution is accepted",
    )?;
    ensure(update(1)?.status == 409, "a repeated sequence is refused")?;
    ensure(update(2)?.status == 202, "a later sequence is accepted")?;
    let malformed = worker.post(Listener::Control, "/v1/cancel", b"{\"nope\":1}")?;
    ensure(malformed.status == 400, "a malformed cancel is 400")?;
    blocker.cancelled(&worker)
}

// ---------------------------------------------------------------------------------------------
// AC8: size ceilings and transfer deadlines over real TCP

fn oversized_headers(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    for (listener, path) in [
        (Listener::Execute, "/v1/execute"),
        (Listener::Control, "/v1/cancel"),
    ] {
        let addr = match listener {
            Listener::Execute => worker.execute,
            Listener::Control => worker.control,
        };
        let line = format!("POST {path} HTTP/1.1\r\nX-Pad: ");
        let fill = MAX_HEADERS + 1 - line.len() - 4;
        let bytes = [line.as_bytes(), "a".repeat(fill).as_bytes(), b"\r\n\r\n"].concat();
        ensure(
            bytes.len() == MAX_HEADERS + 1,
            "the header block is one byte over the ceiling",
        )?;
        let refused = exchange(addr, &bytes)?;
        // Refused with 431, or closed; never served.
        ensure(
            refused.as_ref().is_none_or(|r| r.status == 431),
            "oversized headers are refused",
        )?;
    }
    ensure(worker.count("execution_started") == 0, "no guest code ran")?;
    worker.recovered("headers")
}

fn oversized_request(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    // One byte over the ceiling is refused from the declared length, without reading the body.
    let head = format!(
        "POST /v1/execute HTTP/1.1\r\nHost: worker\r\nContent-Length: {}\r\n\r\n",
        MAX_REQUEST + 1
    );
    let refused = exchange(worker.execute, head.as_bytes())?;
    ensure(
        refused.as_ref().is_none_or(|r| r.status == 413),
        "an oversized request is 413",
    )?;
    // Exactly the ceiling is read in full and then judged as a message.
    let mut at_limit = format!(
        "POST /v1/execute HTTP/1.1\r\nHost: worker\r\nContent-Length: {MAX_REQUEST}\r\n\r\n"
    )
    .into_bytes();
    at_limit.extend(std::iter::repeat_n(b'x', MAX_REQUEST));
    let judged =
        exchange(worker.execute, &at_limit)?.ok_or("the at-limit request got no answer")?;
    ensure(
        judged.status == 400,
        "a request of exactly the ceiling is read and judged",
    )?;
    // The control listener has a much smaller ceiling.
    let head = format!(
        "POST /v1/cancel HTTP/1.1\r\nHost: worker\r\nContent-Length: {}\r\n\r\n",
        MAX_CONTROL_BODY + 1
    );
    let refused = exchange(worker.control, head.as_bytes())?;
    ensure(
        refused.as_ref().is_none_or(|r| r.status == 413),
        "an oversized control request is 413",
    )?;
    ensure(worker.count("execution_started") == 0, "no guest code ran")?;
    worker.recovered("request")
}

fn oversized_action_body(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let big = request(
        "body-big",
        ECHO,
        &[],
        json!({ "__ow_path": "/b", "__ow_body": "a".repeat(MAX_ACTION_BODY + 1) }),
    );
    let refused = worker.execute(&big)?;
    ensure(refused.status == 413, "an oversized action body is 413")?;
    ensure(
        refused.json()?["result"]["code"] == "request_too_large",
        "request_too_large",
    )?;
    ensure(worker.count("execution_started") == 0, "no guest code ran")?;
    worker.recovered("body")
}

fn silent_connection(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let mut silent = connect(worker.execute)?;
    closed_within(&mut silent, EXECUTE_HEADER_DEADLINE + SLACK)?;
    ensure(worker.count("execution_started") == 0, "no guest code ran")?;
    worker.recovered("silent")
}

fn header_drip(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let mut stream = connect(worker.execute)?;
    // One byte of a never-ending header per second: each read below doubles as the wait. The
    // absolute header deadline closes the connection although bytes keep arriving.
    let started = Instant::now();
    let drip =
        b"POST /v1/execute HTTP/1.1\r\nX-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mut closed = None;
    for byte in drip {
        if stream.write_all(&[*byte]).is_err() {
            closed = Some(started.elapsed());
            break;
        }
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.kind().to_string())?;
        let mut probe = [0_u8; 1];
        match stream.read(&mut probe) {
            Ok(_) => {
                closed = Some(started.elapsed());
                break;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                closed = Some(started.elapsed());
                break;
            }
        }
    }
    let took = closed.ok_or("a dripping connection was never closed")?;
    ensure(
        took <= EXECUTE_HEADER_DEADLINE + SLACK,
        "the drip was closed within the header deadline",
    )?;
    ensure(worker.count("execution_started") == 0, "no guest code ran")?;
    worker.recovered("drip")
}

fn stalled_body(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let mut stream = connect(worker.execute)?;
    let head =
        "POST /v1/execute HTTP/1.1\r\nHost: worker\r\nContent-Length: 1000\r\n\r\n0123456789";
    stream
        .write_all(head.as_bytes())
        .map_err(|e| e.kind().to_string())?;
    closed_within(&mut stream, EXECUTE_BODY_IDLE + SLACK)?;
    ensure(worker.count("execution_started") == 0, "no guest code ran")?;
    worker.recovered("stalled")
}

fn body_drip(binary: &Path) -> Check {
    let worker = Worker::start(binary, &Config::default())?;
    let mut stream = connect(worker.execute)?;
    // The head promises more body than the drip ever delivers. One byte per second keeps every
    // read inside the idle limit, so only the separate total body deadline can close it.
    let head = "POST /v1/execute HTTP/1.1\r\nHost: worker\r\nContent-Length: 4000\r\n\r\n";
    let started = Instant::now();
    stream
        .write_all(head.as_bytes())
        .map_err(|e| e.kind().to_string())?;
    let mut closed = None;
    while started.elapsed() < EXECUTE_BODY_TOTAL + SLACK + SLACK {
        if stream.write_all(b"0").is_err() {
            closed = Some(started.elapsed());
            break;
        }
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.kind().to_string())?;
        let mut probe = [0_u8; 1];
        match stream.read(&mut probe) {
            Ok(_) => {
                closed = Some(started.elapsed());
                break;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                closed = Some(started.elapsed());
                break;
            }
        }
    }
    let took = closed.ok_or("a body that keeps arriving was never closed")?;
    ensure(
        took > EXECUTE_BODY_IDLE,
        "the connection outlived the idle limit while bytes kept arriving",
    )?;
    ensure(
        took <= EXECUTE_BODY_TOTAL + SLACK,
        "the dripping body was closed within the total body deadline",
    )?;
    ensure(worker.count("execution_started") == 0, "no guest code ran")?;
    worker.recovered("body-drip")
}

/// A named case: it starts its own worker from the binary at the given path.
type Case = (&'static str, fn(&Path) -> Check);

pub const CASES: &[Case] = &[
    ("envelope", execute_envelope),
    ("runtime-mode", runtime_mode),
    ("credential", credential_boundary),
    ("imports", declared_imports),
    ("redaction-load", redaction_load),
    ("saturation", saturation),
    ("control-stall", control_stall),
    ("cross-listener", cross_listener),
    ("headers", oversized_headers),
    ("request", oversized_request),
    ("action-body", oversized_action_body),
    ("silent", silent_connection),
    ("drip", header_drip),
    ("stalled-body", stalled_body),
    ("body-drip", body_drip),
];

/// Runs one named case against the worker binary.
pub fn run(case: &str, worker: &Path) -> Check {
    let (_, check) = CASES
        .iter()
        .find(|(name, _)| *name == case)
        .ok_or_else(|| "unknown_execution_api_case".to_owned())?;
    check(worker)
}
