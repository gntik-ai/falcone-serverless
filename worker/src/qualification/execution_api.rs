//! Execution API (RUN-01.t3) cases, shared by the local integration test and the restricted pod.
//!
//! Each case starts its own server on loopback, so it exercises the real listeners, the real
//! cache and trusted compile child, and real guests. Tests synchronize on server-side events:
//! guest-start signals (the platform sees the guest's first host call), counters at the setup
//! and cache stages, and the server's own capacity probes. The only waits on a clock are for the
//! documented deadlines themselves.
//!
//! The qualification hooks used here exist only in test and qualification builds.

use falcone_serverless_worker::api::{self, CancelState, WorkerLimits};
use falcone_serverless_worker::artifact::cache_key::ENGINE_VERSION;
use falcone_serverless_worker::cache::Stage;
use falcone_serverless_worker::limits::GUEST_MEMORY_BYTES;
use falcone_serverless_worker::server::http::{MAX_CONTROL_BODY, MAX_HEADERS, MAX_REQUEST, Timing};
use falcone_serverless_worker::server::{
    ADMISSION_CONNECTIONS, CONTROL_CONNECTIONS, Hooks, Server, Settings,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const OK: &[u8] = include_bytes!("api_fixtures/ok.wasm");
const SPIN: &[u8] = include_bytes!("api_fixtures/spin.wasm");
const FOUR_LOGS: &[u8] = include_bytes!("api_fixtures/four-logs.wasm");
const LOG_PARAMS: &[u8] = include_bytes!("api_fixtures/log-params.wasm");
const MANY_LOGS: &[u8] = include_bytes!("api_fixtures/many-logs.wasm");
const BLOCK: &[u8] = include_bytes!("api_fixtures/block.wasm");
const IMPORTS_CONTEXT: &[u8] = include_bytes!("api_fixtures/imports-context.wasm");
const STORAGE_CALLS: &[u8] = include_bytes!("api_fixtures/storage-calls.wasm");
const WASI_IMPORT: &[u8] = include_bytes!("execute_fixtures/wasi-import.wasm");

const LOGGING: &str = "falcone:host/logging@0.2.0";
const CONTEXT: &str = "falcone:host/invocation-context@0.2.0";
const STORAGE: &str = "falcone:host/storage@0.2.0";

/// Slack on top of a documented bound, for scheduling on a loaded machine.
const SLACK: Duration = Duration::from_secs(1);
/// The termination budget of a cancel or an expired deadline.
const TERMINATION: Duration = Duration::from_millis(500);

/// The restricted pod qualifies the documented production deadlines (its runner sets this);
/// local runs scale them down.
pub static PRODUCTION_TIMING: AtomicBool = AtomicBool::new(false);

fn execute_timing() -> Timing {
    if PRODUCTION_TIMING.load(Ordering::Acquire) {
        Timing::EXECUTE
    } else {
        scaled()
    }
}

fn control_timing() -> Timing {
    if PRODUCTION_TIMING.load(Ordering::Acquire) {
        Timing::CONTROL
    } else {
        scaled()
    }
}

fn scaled() -> Timing {
    Timing {
        header_deadline: Duration::from_millis(400),
        body_inactivity: Duration::from_millis(400),
        response_deadline: Duration::from_millis(400),
    }
}

// --- the server under test ------------------------------------------------------------------

#[derive(Default)]
struct Seen {
    binds: AtomicUsize,
    calls: AtomicUsize,
    entries: AtomicUsize,
    stage_hits: AtomicUsize,
}

fn wait_for(condition: impl Fn() -> bool, what: &str) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn gate() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

fn block_on(gate: &AtomicBool) {
    let start = Instant::now();
    while !gate.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[derive(Clone)]
struct Options {
    concurrency: usize,
    max_memory_bytes: u64,
    max_wall_clock: Duration,
    execute_timing: Timing,
    control_timing: Timing,
    bind_gate: Option<Arc<AtomicBool>>,
    call_gate: Option<Arc<AtomicBool>>,
    /// Blocks the entry with this index until the instant stored in the cell.
    entry_block: Option<(u64, Arc<Mutex<Option<Instant>>>)>,
    stage_gate: Option<(Stage, Arc<AtomicBool>)>,
    /// Holds each guest start until this instant (after instantiation, before the handler).
    start_release: Option<Arc<Mutex<Option<Instant>>>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            concurrency: 1,
            max_memory_bytes: 512 * 1024 * 1024,
            max_wall_clock: Duration::from_secs(300),
            execute_timing: execute_timing(),
            control_timing: control_timing(),
            bind_gate: None,
            call_gate: None,
            entry_block: None,
            stage_gate: None,
            start_release: None,
        }
    }
}

struct Env {
    server: Server,
    root: PathBuf,
    seen: Arc<Seen>,
    concurrency: usize,
}

fn compiler() -> PathBuf {
    let exe = std::env::current_exe().expect("current executable");
    let parent = exe.parent().expect("executable directory");
    // Cargo integration tests live in deps/; the qualification binary is beside the compiler
    // in both the local build and the restricted qualification pod.
    let binaries = if parent.file_name().is_some_and(|name| name == "deps") {
        parent.parent().expect("target directory")
    } else {
        parent
    };
    binaries.join("artifact-compile-child")
}

fn start(options: Options) -> Env {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "execution-api-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(root.join("artifacts")).expect("artifact directory");
    let seen = Arc::new(Seen::default());
    let mut settings = Settings::new(
        WorkerLimits {
            max_memory_bytes: options.max_memory_bytes,
            compatibility_memory_bytes: GUEST_MEMORY_BYTES as u64,
            max_wall_clock: options.max_wall_clock,
            engine_version: ENGINE_VERSION,
        },
        options.concurrency,
        root.join("artifacts"),
        root.join("cache"),
        compiler(),
    );
    settings.execute_timing = options.execute_timing;
    settings.control_timing = options.control_timing;
    let mut hooks = Hooks::default();
    {
        let (seen, gate) = (seen.clone(), options.bind_gate.clone());
        hooks.bind = Some(Arc::new(move || {
            seen.binds.fetch_add(1, Ordering::AcqRel);
            if let Some(gate) = &gate {
                block_on(gate);
            }
        }));
    }
    {
        let (seen, gate) = (seen.clone(), options.call_gate.clone());
        hooks.call = Some(Arc::new(move || {
            seen.calls.fetch_add(1, Ordering::AcqRel);
            if let Some(gate) = &gate {
                block_on(gate);
            }
        }));
    }
    {
        let (seen, block) = (seen.clone(), options.entry_block.clone());
        hooks.entry = Some(Arc::new(move |index| {
            seen.entries.fetch_add(1, Ordering::AcqRel);
            if let Some((target, release)) = &block
                && *target == index
            {
                let until = release.lock().unwrap().expect("release instant set");
                while Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }));
    }
    if let Some((stage, gate)) = options.stage_gate.clone() {
        let seen = seen.clone();
        hooks.stage = Some(Arc::new(move |at| {
            // Only the first operation to reach the stage is held: it is the one the execution
            // abandons, and the executions after it must pass while it is still stalled.
            if at == stage && seen.stage_hits.fetch_add(1, Ordering::AcqRel) == 0 {
                block_on(&gate);
            }
        }));
    }
    if let Some(release) = options.start_release.clone() {
        hooks.start = Some(Arc::new(move || {
            let until = release.lock().unwrap().expect("release instant set");
            while Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
        }));
    }
    settings.hooks = hooks;
    let local: SocketAddr = ([127, 0, 0, 1], 0).into();
    let server = Server::start(settings, local, local).expect("start the execution service");
    Env {
        server,
        root,
        seen,
        concurrency: options.concurrency,
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Env {
    /// Stores an artifact where the worker reads it and returns its digest.
    fn publish(&self, bytes: &[u8]) -> String {
        let digest = format!("{:x}", Sha256::digest(bytes));
        std::fs::write(
            self.root.join("artifacts").join(format!("{digest}.wasm")),
            bytes,
        )
        .expect("write artifact");
        format!("sha256:{digest}")
    }

    fn execute(&self, spec: &Spec) -> Reply {
        post(self.server.execute_addr(), "/v1/execute", &spec.bytes())
    }

    fn control(&self, path: &str, body: &Value) -> Reply {
        post(
            self.server.control_addr(),
            path,
            &serde_json::to_vec(body).unwrap(),
        )
    }

    fn cancel(&self, spec: &Spec) -> Reply {
        self.control("/v1/cancel", &spec.cancel_message())
    }

    fn cache_entries(&self) -> Vec<String> {
        std::fs::read_dir(self.server.cache_dir())
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The service is whole again: a normal execute succeeds, and a cancel takes effect.
    fn recovered(&self) {
        let ok = Spec::new(self, OK, &[]);
        let reply = self.execute(&ok);
        assert_eq!(reply.status, 200, "a normal execute after the case");
        assert_eq!(reply.json()["status"], "success");
        let blocked = Spec::new(self, BLOCK, &[LOGGING]);
        let before = self.seen.calls.load(Ordering::Acquire);
        let running = self.spawn_execute(&blocked);
        wait_for(
            || self.seen.calls.load(Ordering::Acquire) > before,
            "the blocked guest started",
        );
        self.server.reset_marks();
        let started = Instant::now();
        let cancel = self.cancel(&blocked);
        assert_eq!(cancel.status, 200);
        assert_eq!(cancel_state(&cancel), CancelState::Acknowledged);
        assert!(started.elapsed() < TERMINATION + SLACK, "the cancel answer");
        assert_eq!(running.join().expect("execute thread").status, 499);
        wait_for(|| self.server.running_executions() == 0, "slots released");
        self.assert_released_after_cancel();
    }

    /// The slot was released within the termination budget of the last accepted cancel, by the
    /// server's own clock: the bound is the documented one, with no transport tolerance.
    fn assert_released_after_cancel(&self) {
        let after = self
            .server
            .release_after_cancel()
            .expect("a cancel was accepted and the slot released");
        assert!(after <= TERMINATION, "slot released {after:?} after cancel");
    }

    /// The slot was released within the termination budget of the execution's absolute
    /// deadline, by the server's own clock.
    fn assert_released_after_deadline(&self) {
        let after = self
            .server
            .release_after_deadline()
            .expect("the deadline was set and the slot released");
        assert!(after <= TERMINATION, "slot released {after:?} after until");
    }

    fn spawn_execute(&self, spec: &Spec) -> std::thread::JoinHandle<Reply> {
        let (addr, body) = (self.server.execute_addr(), spec.bytes());
        std::thread::spawn(move || post(addr, "/v1/execute", &body))
    }
}

fn cancel_state(reply: &Reply) -> CancelState {
    api::parse_cancel(&reply.body)
        .expect("a cancel message")
        .state()
}

// --- requests -------------------------------------------------------------------------------

fn timestamp(millis: u64) -> String {
    let (seconds, ms) = (millis / 1000, millis % 1000);
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[derive(Clone)]
struct Spec {
    activation: String,
    digest: String,
    mode: String,
    imports: Vec<String>,
    grants: Vec<Value>,
    fuel: u64,
    memory_bytes: u64,
    timeout_ms: u64,
    max_host_calls: u64,
    deadline_in: Duration,
    /// iss, aud, tenant, workspace, pool_profile
    claims: [String; 5],
    /// Extra credential strings, carried as storage scope names.
    scope_names: Vec<String>,
    body: Option<String>,
}

fn unique(prefix: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

impl Spec {
    fn new(env: &Env, artifact: &[u8], imports: &[&str]) -> Self {
        Self::for_digest(env.publish(artifact), imports)
    }

    /// A request for the artifact already published under `digest` (`sha256:<hex>`).
    fn for_digest(digest: String, imports: &[&str]) -> Self {
        Self {
            activation: unique("act"),
            digest,
            mode: "wasm".into(),
            imports: imports.iter().map(|import| (*import).to_owned()).collect(),
            grants: Vec::new(),
            fuel: 1_000_000_000,
            memory_bytes: 64 * 1024 * 1024,
            timeout_ms: 30_000,
            max_host_calls: 10_000,
            deadline_in: Duration::from_secs(120),
            claims: [
                "iss.zeta.example".into(),
                "aud.zeta.example".into(),
                "tenant-zeta-0001".into(),
                "wspace-zeta-0001".into(),
                "pool-zeta-small".into(),
            ],
            scope_names: Vec::new(),
            body: None,
        }
    }

    fn json(&self) -> Value {
        let mut credential_grants = vec![json!({
            "family": "http",
            "interface": "falcone:host/http@0.1.0",
            "scope": {"hosts": ["api.qualification.example"]}
        })];
        if !self.scope_names.is_empty() {
            credential_grants.push(json!({
                "family": "storage",
                "interface": "falcone:host/storage@0.1.0",
                "scope": {"prefixes": self.scope_names}
            }));
        }
        let mut params = json!({"__ow_path": "/v1/qualification"});
        if let Some(body) = &self.body {
            params["__ow_body"] = json!(body);
        }
        json!({
            "version": "1",
            "type": "execute",
            "activation_id": self.activation,
            "attempt_id": format!("{}-attempt", self.activation),
            "attempt_number": 1,
            "deadline": timestamp(now_millis() + self.deadline_in.as_millis() as u64),
            "invocation": {"invocation_key": "inv-qualification-0001"},
            "credential": {
                "iss": self.claims[0], "aud": self.claims[1], "tenant": self.claims[2],
                "workspace": self.claims[3], "pool_profile": self.claims[4],
                "revision": "rev-qualification-0001",
                "activation": self.activation,
                "attempt": format!("{}-attempt", self.activation),
                "iat": 1, "exp": 2,
                "allowed_capabilities": credential_grants
            },
            "projection": {
                "revision": "rev-qualification-0001",
                "runtime_mode": self.mode,
                "artifact_digest": self.digest,
                "component_digest": self.digest,
                "language_profile": {"name": "profile-qualification", "version": "1"},
                "engine_version": ENGINE_VERSION,
                "wasi_version": "0.2.6",
                "interface_version": "0.2.0",
                "imports": self.imports,
                "budgets": {
                    "fuel": self.fuel,
                    "memory_bytes": self.memory_bytes,
                    "timeout_ms": self.timeout_ms,
                    "max_host_calls": self.max_host_calls
                },
                "grants": self.grants
            },
            "params": params
        })
    }

    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.json()).unwrap()
    }

    /// The parsed request, against which a result's identity and attempt are checked.
    fn parsed(&self) -> api::ExecuteRequest {
        api::parse_request(&self.bytes()).expect("a request the worker admits")
    }

    fn cancel_message(&self) -> Value {
        json!({
            "version": "1", "type": "cancel",
            "activation_id": self.activation,
            "attempt_id": format!("{}-attempt", self.activation),
            "cancel": {"state": "requested"}
        })
    }

    fn update_message(&self, sequence: u64) -> Value {
        json!({
            "version": "1", "type": "update",
            "activation_id": self.activation,
            "attempt_id": format!("{}-attempt", self.activation),
            "sequence": sequence,
            "update": {"kind": "heartbeat"}
        })
    }

    fn all_secrets(&self) -> Vec<String> {
        let mut secrets: Vec<String> = self.claims.to_vec();
        secrets.extend(self.scope_names.iter().cloned());
        secrets
    }
}

// --- the wire -------------------------------------------------------------------------------

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }

    fn code(&self) -> String {
        let value = self.json();
        let failure = if value.get("result").is_some_and(Value::is_object) {
            &value["result"]
        } else {
            &value
        };
        failure["code"].as_str().unwrap_or_default().to_owned()
    }

    fn reason(&self) -> String {
        let value = self.json();
        let failure = if value.get("result").is_some_and(Value::is_object) {
            &value["result"]
        } else {
            &value
        };
        failure["detail"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn logs(&self) -> Vec<String> {
        self.json()["logs"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .map(|line| line.as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    stream
}

fn request_bytes(path: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "POST {path} HTTP/1.1\r\nHost: worker\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn read_reply(stream: &mut TcpStream) -> Reply {
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    parse_reply(&bytes)
}

fn parse_reply(bytes: &[u8]) -> Reply {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a complete response head");
    let head = std::str::from_utf8(&bytes[..split]).unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    Reply {
        status,
        body: bytes[split + 4..].to_vec(),
    }
}

fn post(addr: SocketAddr, path: &str, body: &[u8]) -> Reply {
    let mut stream = connect(addr);
    stream.write_all(&request_bytes(path, body)).unwrap();
    read_reply(&mut stream)
}

/// Whether the peer has closed the connection (end of stream or reset), waiting at most `wait`.
fn closed_within(stream: &mut TcpStream, wait: Duration) -> Option<Duration> {
    let start = Instant::now();
    stream.set_read_timeout(Some(wait)).unwrap();
    let mut buffer = [0u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Some(start.elapsed()),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return None;
            }
            Err(_) => return Some(start.elapsed()),
        }
    }
}

/// Opens connections that never finish their request, until `addr` answers a fresh connection at
/// once with `503 busy`: the listener's capacity is then provably full.
fn fill(addr: SocketAddr, count: usize) -> Vec<TcpStream> {
    let stalled: Vec<TcpStream> = (0..count)
        .map(|_| {
            let mut stream = connect(addr);
            stream.write_all(b"POST /v1/").unwrap();
            stream
        })
        .collect();
    wait_for(|| is_full(addr), "the listener is full");
    stalled
}

fn is_full(addr: SocketAddr) -> bool {
    let mut probe = connect(addr);
    probe
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut buffer = [0u8; 512];
    match probe.read(&mut buffer) {
        Ok(count) if count > 0 => buffer[..count].starts_with(b"HTTP/1.1 503"),
        _ => false,
    }
}

fn assert_failure(reply: &Reply, status: u16, code: &str, reason: &str) {
    assert_eq!(
        reply.status,
        status,
        "status of {}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(reply.code(), code);
    assert_eq!(reply.reason(), reason);
}

/// A failure result must parse against its request: identity, attribution and class included.
fn assert_answers(reply: &Reply, spec: &Spec, code: &str, reason: &str) {
    // A refused request (another runtime mode) has no admitted request to compare with: its
    // identity is checked against its projection.
    let matches = match api::parse_request(&spec.bytes()) {
        Ok(request) => api::parse_result(&reply.body, &request).is_ok(),
        Err(_) => {
            let projection = serde_json::to_vec(&spec.json()["projection"]).unwrap();
            let projection = api::parse_projection(&projection).expect("a projection");
            api::parse_result(&reply.body, &projection).is_ok()
        }
    };
    assert!(matches, "the result matches its request and projection");
    assert_eq!(reply.code(), code);
    assert_eq!(reply.reason(), reason);
    assert_eq!(reply.json()["activation_id"], spec.activation.as_str());
}

// --- identity, refusals ---------------------------------------------------------------------

/// A wasm revision returns the legacy envelope; the identity matches the projection and the
/// pinned Wasmtime version; the artifact is validated once per execution.
pub fn legacy_envelope_and_identity() {
    let env = start(Options::default());
    let spec = Spec::new(&env, OK, &[]);
    let reply = env.execute(&spec);
    assert_eq!(reply.status, 200);
    api::parse_result(&reply.body, &spec.parsed()).expect("a result that matches its request");
    let value = reply.json();
    assert_eq!(value["status"], "success");
    assert_eq!(value["result"], json!({"statusCode": 200, "body": "ok"}));
    assert!(value.get("class").is_none());
    let identity = &value["identity"];
    assert_eq!(identity["wasmtime_version"], ENGINE_VERSION);
    assert_eq!(identity["runtime_mode"], "wasm");
    assert_eq!(identity["artifact_digest"], spec.digest.as_str());
    assert_eq!(identity["function_revision"], "rev-qualification-0001");
    assert_eq!(identity["interface_version"], "0.2.0");
    assert_eq!(env.server.validations(), 1);
    assert_eq!(env.execute(&Spec::new(&env, OK, &[])).status, 200);
    assert_eq!(
        env.server.validations(),
        2,
        "validation runs once per execution"
    );
}

/// A non-wasm runtime mode, or an image revision, is refused and creates no instance.
pub fn non_wasm_refused_without_instance() {
    let env = start(Options::default());
    let mut oci = Spec::new(&env, OK, &[]);
    oci.mode = "oci".into();
    let reply = env.execute(&oci);
    assert_eq!(reply.status, 409);
    assert_answers(
        &reply,
        &oci,
        "runtime_mode_not_supported",
        "runtime_mode_not_wasm",
    );
    let mut image = oci.clone();
    image.activation = unique("act");
    let mut document = image.json();
    document["projection"]["base"] = json!({
        "kind": "byo",
        "image_digest": image.digest,
        "signer_identity": "signer.qualification.example",
        "attestation_ref": "attestation-qualification-0001"
    });
    let reply = post(
        env.server.execute_addr(),
        "/v1/execute",
        &serde_json::to_vec(&document).unwrap(),
    );
    assert_failure(
        &reply,
        409,
        "runtime_mode_not_supported",
        "runtime_mode_not_wasm",
    );
    let mut unknown = Spec::new(&env, OK, &[]).json();
    unknown["projection"]["runtime_mode"] = json!("quantum");
    let reply = post(
        env.server.execute_addr(),
        "/v1/execute",
        &serde_json::to_vec(&unknown).unwrap(),
    );
    assert_failure(
        &reply,
        409,
        "runtime_mode_not_supported",
        "runtime_mode_not_wasm",
    );
    assert_eq!(env.server.validations(), 0, "nothing was loaded");
    assert_eq!(
        env.seen.binds.load(Ordering::Acquire),
        0,
        "nothing was bound"
    );
    assert_eq!(
        env.seen.entries.load(Ordering::Acquire),
        0,
        "no instance ran"
    );
    env.recovered();
}

/// The projection's engine must be the pinned Wasmtime, or no identity could match it.
pub fn another_engine_version_is_refused() {
    let env = start(Options::default());
    let mut document = Spec::new(&env, OK, &[]).json();
    document["projection"]["engine_version"] = json!("1.0.0");
    let reply = post(
        env.server.execute_addr(),
        "/v1/execute",
        &serde_json::to_vec(&document).unwrap(),
    );
    assert_eq!(reply.status, 400);
    assert_eq!(reply.reason(), "engine_version_mismatch");
    assert_eq!(env.server.validations(), 0);
}

// --- the credential -------------------------------------------------------------------------

/// Whether a line holds 8 or more consecutive characters of the secret, or all of a short one.
fn leaks(line: &str, secret: &str) -> bool {
    let secret: Vec<char> = secret.chars().collect();
    if secret.len() < 8 {
        return line.contains(&secret.iter().collect::<String>());
    }
    let line: Vec<char> = line.chars().collect();
    secret
        .windows(8)
        .any(|window| line.windows(8).any(|candidate| candidate == window))
}

fn assert_no_leak(reply: &Reply, secrets: &[String]) {
    let text = String::from_utf8_lossy(&reply.body);
    for (at, secret) in secrets.iter().enumerate() {
        assert!(
            !leaks(&text, secret),
            "the response carries credential string {at} (length {}) in logs {:?}",
            secret.len(),
            reply
                .logs()
                .iter()
                .map(|l| l.chars().take(120).collect::<String>())
                .collect::<Vec<_>>()
        );
    }
    for line in reply.logs() {
        assert!(line.chars().count() <= api::MAX_LOG_LINE_CHARS);
    }
}

fn secret(seed: char, length: usize) -> String {
    (0..length)
        .map(|at| match (at * 7 + seed as usize) % 36 {
            n @ 0..=9 => char::from(b'0' + n as u8),
            n => char::from(b'A' + (n - 10) as u8),
        })
        .collect()
}

/// The credential never appears in guest-visible data or in logs, even when the guest logs
/// text that contains it, and the guest has no environment or files to look in.
pub fn credential_never_leaves_the_host_layer() {
    let env = start(Options::default());
    let mut spec = Spec::new(&env, LOG_PARAMS, &[LOGGING]);
    spec.claims[2] = secret('t', 30);
    spec.claims[3] = "wsp".into();
    spec.scope_names = vec![secret('s', 24)];
    spec.body = Some(format!(
        "before {} between {} after {} end",
        spec.claims[2], spec.scope_names[0], spec.claims[3]
    ));
    let reply = env.execute(&spec);
    assert_eq!(reply.status, 200);
    assert_no_leak(&reply, &spec.all_secrets());
    let logs = reply.logs();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains("<redacted>"));
    assert!(
        logs[0].contains("before <redacted> between <redacted> after <redacted> end"),
        "{logs:?}"
    );
    // No environment or file reaches a guest: WASI is not importable even when declared.
    let wasi = Spec::new(&env, WASI_IMPORT, &["wasi:cli/environment@0.2.0"]);
    assert_failure(
        &env.execute(&wasi),
        422,
        "undeclared_import",
        "import_not_declared",
    );
}

/// A log line is redacted before it is cut: a secret longer than the cut, one that crosses it,
/// one that ends at it and several of which only some fit before it all leave nothing behind.
pub fn redaction_holds_across_the_cut() {
    let env = start(Options::default());
    let prefix = "debug {\"__ow_body\":\"".len();
    let run = |build: &dyn Fn(&mut Spec) -> String| {
        let mut spec = Spec::new(&env, LOG_PARAMS, &[LOGGING]);
        spec.claims = [
            secret('a', 40),
            secret('b', 40),
            secret('c', 40),
            secret('d', 40),
            secret('e', 40),
        ];
        spec.body = Some(build(&mut spec));
        let reply = env.execute(&spec);
        assert_eq!(reply.status, 200);
        assert_no_leak(&reply, &spec.all_secrets());
        let logs = reply.logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains("<redacted>"));
    };
    // Longer than the cut: 9000 characters.
    run(&|spec| {
        spec.claims[2] = secret('t', 9000);
        format!("x{}y", spec.claims[2])
    });
    // Crossing the cut: starts 20 characters before 8192 and ends 20 after it.
    run(&|spec| {
        let before = 8192 - prefix - 20;
        format!(
            "{}{}{}",
            "a".repeat(before),
            spec.claims[0],
            "z".repeat(100)
        )
    });
    // Ending exactly at the cut.
    run(&|spec| {
        let before = 8192 - prefix - spec.claims[0].len();
        format!(
            "{}{}{}",
            "a".repeat(before),
            spec.claims[0],
            "z".repeat(100)
        )
    });
    // Several secrets, only some of which fit before the cut.
    run(&|spec| {
        let before = 8192 - prefix - 60;
        format!(
            "{}{}.{}.{}.{}",
            "a".repeat(before),
            spec.claims[0],
            spec.claims[1],
            spec.claims[2],
            spec.claims[3]
        )
    });
    // Overlapping secrets that share a prefix and a suffix, and a piece of a secret on its own.
    run(&|spec| {
        spec.claims[1] = format!("{}{}", &spec.claims[0][..30], secret('o', 20));
        format!(
            "{}{}{}",
            spec.claims[0],
            &spec.claims[1][30..],
            &spec.claims[2][5..25]
        )
    });
    // More credential bytes than the matcher's budget: all logs fail closed to the marker.
    let mut spec = Spec::new(&env, LOG_PARAMS, &[LOGGING]);
    spec.claims[0] = "Q".repeat(300 * 1024);
    spec.body = Some("plain text".into());
    let reply = env.execute(&spec);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.logs(), ["<redacted>"]);
    env.recovered();
}

// --- declared imports -----------------------------------------------------------------------

/// Every import must be declared by the projection and be an allowed interface; otherwise 422
/// and no guest code runs.
pub fn imports_must_be_declared_and_allowed() {
    let env = start(Options::default());
    let undeclared = |spec: &Spec| {
        let reply = env.execute(spec);
        assert_failure(&reply, 422, "undeclared_import", "import_not_declared");
        assert_eq!(
            env.seen.entries.load(Ordering::Acquire),
            0,
            "no guest code ran"
        );
        assert_eq!(
            env.seen.binds.load(Ordering::Acquire),
            0,
            "nothing was bound"
        );
        assert_eq!(env.server.running_executions(), 0);
    };
    // An allowed interface the projection does not declare: `imports: []` means none.
    undeclared(&Spec::new(&env, IMPORTS_CONTEXT, &[]));
    // Declared others do not declare this one.
    undeclared(&Spec::new(&env, IMPORTS_CONTEXT, &[LOGGING]));
    // Declared, but not an interface this worker allows.
    undeclared(&Spec::new(
        &env,
        WASI_IMPORT,
        &["wasi:cli/environment@0.2.0"],
    ));
    // Every import declared and allowed.
    let allowed = Spec::new(&env, IMPORTS_CONTEXT, &[CONTEXT]);
    let reply = env.execute(&allowed);
    assert_eq!(reply.status, 200);
    api::parse_result(&reply.body, &allowed.parsed()).expect("an accepted result");
    env.recovered();
}

// --- listeners ------------------------------------------------------------------------------

/// Each listener refuses the other's operations, and neither runs anything for them.
pub fn each_listener_refuses_the_others_operations() {
    let env = start(Options::default());
    let spec = Spec::new(&env, OK, &[]);
    let (execute, control) = (env.server.execute_addr(), env.server.control_addr());
    for path in ["/v1/cancel", "/v1/update", "/v1/other"] {
        let reply = post(
            execute,
            path,
            &serde_json::to_vec(&spec.cancel_message()).unwrap(),
        );
        assert_failure(&reply, 404, "not_found", "execution_not_found");
    }
    for path in ["/v1/execute", "/v1/other"] {
        let reply = post(control, path, &spec.bytes());
        assert_failure(&reply, 404, "not_found", "execution_not_found");
    }
    assert_eq!(env.server.validations(), 0, "nothing was loaded or run");
    assert_eq!(env.seen.binds.load(Ordering::Acquire), 0);
    assert_eq!(env.execute(&spec).status, 200);
}

/// With every execution slot blocked and the execute listener full of stalled connections, a
/// cancel on the control listener still takes effect within the termination budget, and a
/// further execute is answered `503 busy`.
pub fn cancel_survives_a_saturated_execute_listener() {
    let env = start(Options {
        concurrency: 2,
        ..Options::default()
    });
    let blocked: Vec<Spec> = (0..2).map(|_| Spec::new(&env, BLOCK, &[LOGGING])).collect();
    let running: Vec<_> = blocked.iter().map(|spec| env.spawn_execute(spec)).collect();
    wait_for(
        || env.seen.calls.load(Ordering::Acquire) == 2,
        "both guests started",
    );
    let stalled = fill(env.server.execute_addr(), ADMISSION_CONNECTIONS);
    // A further execute is answered at once, without its body being read.
    let extra = env.execute(&Spec::new(&env, OK, &[]));
    assert_failure(&extra, 503, "busy", "at_capacity");
    assert_eq!(extra.json()["retryable"], true);
    // The control listener is untouched by the saturation.
    env.server.reset_marks();
    let cancel = env.cancel(&blocked[0]);
    assert_eq!(cancel.status, 200);
    assert_eq!(cancel_state(&cancel), CancelState::Acknowledged);
    let first = running.into_iter().next().unwrap().join().unwrap();
    assert_answers(&first, &blocked[0], "cancelled", "cancel_requested");
    assert_eq!(first.status, 499);
    env.assert_released_after_cancel();
    drop(stalled);
    let second = env.cancel(&blocked[1]);
    assert_eq!(cancel_state(&second), CancelState::Acknowledged);
    wait_for(|| env.server.running_executions() == 0, "slots released");
    env.recovered();
}

/// With every execution slot busy, capacity is checked once the head has arrived and before the
/// body is read: a complete non-wasm request still gets 409, anything else gets 503 at once.
pub fn admission_order_when_every_slot_is_busy() {
    let env = start(Options::default());
    let blocked = Spec::new(&env, BLOCK, &[LOGGING]);
    let running = env.spawn_execute(&blocked);
    wait_for(
        || env.seen.calls.load(Ordering::Acquire) == 1,
        "the guest started",
    );
    let wasm = Spec::new(&env, OK, &[]);
    assert_failure(&env.execute(&wasm), 503, "busy", "at_capacity");
    let mut oci = Spec::new(&env, OK, &[]);
    oci.mode = "oci".into();
    assert_answers(
        &env.execute(&oci),
        &oci,
        "runtime_mode_not_supported",
        "runtime_mode_not_wasm",
    );
    // The head has arrived, the body has not: answered `503` at once, without waiting for it.
    let mut partial = connect(env.server.execute_addr());
    partial
        .write_all(b"POST /v1/execute HTTP/1.1\r\nContent-Length: 100000\r\n\r\n0123456789")
        .unwrap();
    let started = Instant::now();
    assert_failure(&read_reply(&mut partial), 503, "busy", "at_capacity");
    assert!(started.elapsed() < execute_timing().body_inactivity);
    let cancel = env.cancel(&blocked);
    assert_eq!(cancel_state(&cancel), CancelState::Acknowledged);
    assert_eq!(running.join().unwrap().status, 499);
}

/// With the control listener full of stalled connections, running executions are unaffected,
/// and the control listener recovers once its deadlines have closed the stalled connections.
pub fn a_stalled_control_listener_does_not_touch_executions() {
    let env = start(Options {
        concurrency: 2,
        ..Options::default()
    });
    let blocked = Spec::new(&env, BLOCK, &[LOGGING]);
    let running = env.spawn_execute(&blocked);
    wait_for(
        || env.seen.calls.load(Ordering::Acquire) == 1,
        "the guest started",
    );
    let mut stalled = fill(env.server.control_addr(), CONTROL_CONNECTIONS);
    let busy = env.cancel(&blocked);
    assert_failure(&busy, 503, "busy", "at_capacity");
    // Executions proceed while the control listener is full.
    let ok = Spec::new(&env, OK, &[]);
    assert_eq!(env.execute(&ok).status, 200);
    // The control deadlines close the stalled connections; nothing evicts them.
    let bound = control_timing().header_deadline + SLACK;
    for stream in &mut stalled {
        assert!(
            closed_within(stream, bound).is_some(),
            "a stalled connection is closed"
        );
    }
    let cancel = env.cancel(&blocked);
    assert_eq!(cancel_state(&cancel), CancelState::Acknowledged);
    assert_eq!(running.join().unwrap().status, 499);
    env.recovered();
}

/// Cancel and update: unknown pairs are 404, malformed bodies 400, a stale sequence 409.
pub fn update_and_cancel_semantics() {
    let env = start(Options::default());
    let blocked = Spec::new(&env, BLOCK, &[LOGGING]);
    let unknown = env.cancel(&blocked);
    assert_failure(&unknown, 404, "not_found", "execution_not_found");
    let running = env.spawn_execute(&blocked);
    wait_for(
        || env.seen.calls.load(Ordering::Acquire) == 1,
        "the guest started",
    );
    let malformed = env.control("/v1/cancel", &json!({"type": "cancel"}));
    assert_eq!(malformed.status, 400);
    let wrong_state = env.control(
        "/v1/cancel",
        &json!({"version": "1", "type": "cancel", "activation_id": blocked.activation,
                "attempt_id": format!("{}-attempt", blocked.activation),
                "cancel": {"state": "acknowledged"}}),
    );
    assert_eq!(wrong_state.status, 400);
    assert_eq!(
        env.control("/v1/update", &blocked.update_message(1)).status,
        200
    );
    let stale = env.control("/v1/update", &blocked.update_message(1));
    assert_failure(&stale, 409, "stale_update", "sequence_not_newer");
    assert_eq!(
        env.control("/v1/update", &blocked.update_message(2)).status,
        200
    );
    let other = Spec::new(&env, OK, &[]);
    let missing = env.control("/v1/update", &other.update_message(1));
    assert_failure(&missing, 404, "not_found", "execution_not_found");
    let cancel = env.cancel(&blocked);
    assert_eq!(cancel_state(&cancel), CancelState::Acknowledged);
    assert_eq!(running.join().unwrap().status, 499);
}

// --- HTTP ceilings and deadlines ------------------------------------------------------------

/// What differs between the two listeners in the transfer cases.
struct Listener {
    addr: SocketAddr,
    path: &'static str,
    timing: Timing,
}

/// The cases both listeners share: an oversized head is refused with the documented status, and
/// connections that send nothing, drip a head or stall a body are closed within their bounds.
/// After each, a normal execute and a cancel both succeed: no slot or permit leaked.
fn transfer_ceilings_and_deadlines(env: &Env, listener: &Listener) {
    let Listener { addr, path, timing } = *listener;

    // Oversized head.
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nX-Pad: {}\r\n\r\n",
        "p".repeat(MAX_HEADERS + 1)
    );
    head.push_str("{}");
    let mut stream = connect(addr);
    let _ = stream.write_all(head.as_bytes());
    assert_failure(
        &read_reply(&mut stream),
        413,
        "request_too_large",
        "body_too_large",
    );
    env.recovered();

    // A connection that sends no bytes is closed at the header deadline.
    let mut silent = connect(addr);
    let elapsed = closed_within(&mut silent, timing.header_deadline + SLACK)
        .expect("the silent connection is closed");
    assert!(elapsed <= timing.header_deadline + SLACK);
    env.recovered();

    // A head dripped past the absolute deadline is closed at it, however steady the drip.
    let mut drip = connect(addr);
    let start = Instant::now();
    drip.write_all(format!("POST {path} HTTP/1.1\r\n").as_bytes())
        .unwrap();
    let step = timing.header_deadline / 8;
    let mut closed = None;
    while start.elapsed() < timing.header_deadline + SLACK {
        if drip.write_all(b"X: y\r\n").is_err() {
            closed = Some(start.elapsed());
            break;
        }
        if let Some(at) = closed_within(&mut drip, step) {
            closed = Some(start.elapsed().max(at));
            break;
        }
    }
    let closed = closed.expect("the dripped head is closed");
    assert!(closed <= timing.header_deadline + SLACK);
    env.recovered();

    // A body that stalls is closed at the inactivity timeout, and frees its connection slot.
    let mut stalled = connect(addr);
    stalled
        .write_all(
            format!("POST {path} HTTP/1.1\r\nContent-Length: 100\r\n\r\n0123456789").as_bytes(),
        )
        .unwrap();
    let elapsed = closed_within(&mut stalled, timing.body_inactivity + SLACK)
        .expect("the stalled body is closed");
    assert!(elapsed <= timing.body_inactivity + SLACK);
    env.recovered();
}

/// Oversized heads, requests and action bodies are refused, and connections that send nothing,
/// drip a head or stall a body are closed within their bounds; no capacity leaks in any case.
pub fn http_ceilings_and_deadlines() {
    let env = start(Options::default());
    let addr = env.server.execute_addr();
    transfer_ceilings_and_deadlines(
        &env,
        &Listener {
            addr,
            path: "/v1/execute",
            timing: execute_timing(),
        },
    );

    // Oversized request: refused before any body is read.
    let mut stream = connect(addr);
    stream
        .write_all(
            format!(
                "POST /v1/execute HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                MAX_REQUEST + 1
            )
            .as_bytes(),
        )
        .unwrap();
    assert_failure(
        &read_reply(&mut stream),
        413,
        "request_too_large",
        "body_too_large",
    );
    env.recovered();

    // Oversized action body: refused with the documented status and no guest code runs.
    let loaded = env.server.validations();
    let mut spec = Spec::new(&env, OK, &[]);
    spec.body = Some("b".repeat(1_048_577));
    let reply = env.execute(&spec);
    assert_answers(&reply, &spec, "request_too_large", "body_too_large");
    assert_eq!(reply.status, 413);
    assert_eq!(
        env.server.validations(),
        loaded,
        "no guest or artifact was touched"
    );
    env.recovered();
}

/// The control listener has the same head and transfer bounds, and its own 64 KiB body ceiling:
/// a body at the ceiling is read, one byte more is refused before it is read. Nothing the
/// stalled or oversized connections did leaves the listener, or an execution, worse off.
pub fn control_ceilings_and_deadlines() {
    let env = start(Options::default());
    let addr = env.server.control_addr();
    transfer_ceilings_and_deadlines(
        &env,
        &Listener {
            addr,
            path: "/v1/cancel",
            timing: control_timing(),
        },
    );

    // One byte over the ceiling is refused on the announced length, with the body unsent.
    let mut stream = connect(addr);
    stream
        .write_all(
            format!(
                "POST /v1/cancel HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                MAX_CONTROL_BODY + 1
            )
            .as_bytes(),
        )
        .unwrap();
    assert_failure(
        &read_reply(&mut stream),
        413,
        "request_too_large",
        "body_too_large",
    );
    env.recovered();

    // The same oversized body, sent in full, is refused all the same, and nothing ran.
    let reply = post(addr, "/v1/cancel", &vec![b'x'; MAX_CONTROL_BODY + 1]);
    assert_failure(&reply, 413, "request_too_large", "body_too_large");
    env.recovered();

    // A body exactly at the ceiling is read, and then judged on its content.
    let loaded = env.server.validations();
    let reply = post(addr, "/v1/cancel", &vec![b'x'; MAX_CONTROL_BODY]);
    assert_failure(&reply, 400, "invalid_request", "malformed_request");
    assert_eq!(
        env.server.validations(),
        loaded,
        "nothing was loaded or run"
    );
    env.recovered();
}

/// A reader that keeps taking a maximum-size response, steadily but slower than the absolute
/// response deadline allows, is cut off at that deadline, and its connection permit is recovered.
///
/// Each reader takes a chunk every few milliseconds from the first response byte on, so the
/// server's partial writes keep progressing across the deadline: a per-write timeout would never
/// fire, only an absolute deadline ends the response. The cutoff is read on the server's side,
/// as its own counters, while every reader still holds its socket open: no transport buffering
/// and no client-side timeout can stand in for it. A server that had kept writing would hold its
/// permits for as long as the readers go on reading.
pub fn a_slow_reader_is_cut_off_at_the_response_deadline() {
    let env = start(Options {
        concurrency: 2,
        ..Options::default()
    });
    let deadline = execute_timing().response_deadline;
    let stop = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..env.concurrency)
        .map(|_| {
            let mut spec = Spec::new(&env, MANY_LOGS, &[LOGGING]);
            spec.body = Some("m".repeat(9000));
            let addr = env.server.execute_addr();
            let (stop, started) = (stop.clone(), started.clone());
            std::thread::spawn(move || {
                let mut stream = connect(addr);
                stream
                    .write_all(&request_bytes("/v1/execute", &spec.bytes()))
                    .unwrap();
                let mut received = Vec::new();
                let mut chunk = [0u8; 4096];
                let first = stream.read(&mut chunk).unwrap();
                assert!(first > 0, "the response started");
                received.extend_from_slice(&chunk[..first]);
                started.fetch_add(1, Ordering::AcqRel);
                // Steady progress, far slower than a response of this size needs to finish
                // within the deadline; the socket stays open until the case ends.
                while !stop.load(Ordering::Acquire) {
                    match stream.read(&mut chunk) {
                        Ok(count) if count > 0 => received.extend_from_slice(&chunk[..count]),
                        _ => break,
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                while !stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                received
            })
        })
        .collect();
    wait_for(
        || started.load(Ordering::Acquire) == env.concurrency,
        "every reader took the first bytes of its response",
    );
    // Every response started before this instant, so each absolute deadline ends no later than
    // here plus the deadline, while every reader still holds its socket open.
    let first_bytes = Instant::now();
    wait_for(
        || env.server.execute_connections() == 0,
        "the connection permits were recovered",
    );
    let recovered_after = first_bytes.elapsed();
    assert!(
        recovered_after <= deadline + SLACK,
        "the response ended at its absolute deadline ({recovered_after:?}), not when the readers \
         were done"
    );
    assert_eq!(
        env.server.responses_cut_off(),
        env.concurrency,
        "each response was cut off by the server"
    );
    stop.store(true, Ordering::Release);
    for reader in readers {
        let received = reader.join().unwrap();
        let split = received
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("a complete response head");
        let head = String::from_utf8_lossy(&received[..split]).to_ascii_lowercase();
        let announced: usize = head
            .split("content-length: ")
            .nth(1)
            .and_then(|rest| rest.split("\r\n").next())
            .and_then(|value| value.trim().parse().ok())
            .expect("an announced length");
        assert!(announced > 4_000_000, "a maximum-size response");
        assert!(
            received.len() - split - 4 < announced,
            "the reader never got the whole response"
        );
    }
    env.recovered();
}

// --- limits ---------------------------------------------------------------------------------

/// The shortest of the request deadline, the projection limit and the worker wall clock wins,
/// and the answer is the documented timeout envelope with attribution and identity.
pub fn the_shortest_timeout_wins() {
    let shortest = Duration::from_millis(600);
    let expect = |env: &Env, spec: &Spec, started: Instant| {
        env.server.reset_marks();
        let reply = env.execute(spec);
        let elapsed = started.elapsed();
        assert_eq!(reply.status, 504);
        env.assert_released_after_deadline();
        assert_answers(&reply, spec, "timeout", "deadline_exceeded");
        assert!(
            env.seen.calls.load(Ordering::Acquire) >= 1,
            "the guest started"
        );
        assert!(
            elapsed >= shortest - Duration::from_millis(100),
            "the shortest limit, not less"
        );
        assert!(
            elapsed <= shortest + TERMINATION + SLACK,
            "the shortest limit, not more, as the client saw it"
        );
        env.recovered();
    };
    // The request's own deadline is the shortest.
    let env = start(Options::default());
    let mut spec = Spec::new(&env, BLOCK, &[LOGGING]);
    spec.deadline_in = shortest;
    expect(&env, &spec, Instant::now());
    // The projection's limit is the shortest.
    let env = start(Options::default());
    let mut spec = Spec::new(&env, BLOCK, &[LOGGING]);
    spec.timeout_ms = shortest.as_millis() as u64;
    expect(&env, &spec, Instant::now());
    // The worker's wall clock is the shortest.
    let env = start(Options {
        max_wall_clock: shortest,
        ..Options::default()
    });
    let spec = Spec::new(&env, BLOCK, &[LOGGING]);
    expect(&env, &spec, Instant::now());
}

/// Memory, fuel and host-call budgets are limits, enforced with distinct reasons.
pub fn budgets_are_enforced() {
    let env = start(Options::default());
    // Memory: the 4 MiB fixture does not fit in 1 MiB, and fits in 8 MiB.
    let mut spec = Spec::new(&env, OK, &[]);
    spec.memory_bytes = 1024 * 1024;
    let reply = env.execute(&spec);
    assert_answers(&reply, &spec, "execution_failed", "memory_budget_exceeded");
    assert_eq!(reply.status, 500);
    assert_eq!(reply.json()["result"]["retryable"], false);
    let mut spec = Spec::new(&env, OK, &[]);
    spec.memory_bytes = 8 * 1024 * 1024;
    assert_eq!(env.execute(&spec).status, 200);
    // Fuel: a loop stops long before the 30 s timeout.
    let mut spec = Spec::new(&env, SPIN, &[]);
    spec.fuel = 1_000_000;
    let started = Instant::now();
    let reply = env.execute(&spec);
    assert_answers(&reply, &spec, "execution_failed", "fuel_exhausted");
    assert!(started.elapsed() < Duration::from_secs(20));
    // Host calls: the call after the budget is refused, and the host saw exactly the budget.
    let before = env.seen.calls.load(Ordering::Acquire);
    let mut spec = Spec::new(&env, FOUR_LOGS, &[LOGGING]);
    spec.max_host_calls = 3;
    let reply = env.execute(&spec);
    assert_answers(
        &reply,
        &spec,
        "execution_failed",
        "host_call_budget_exceeded",
    );
    assert_eq!(env.seen.calls.load(Ordering::Acquire) - before, 3);
    assert_eq!(
        reply.logs().len(),
        3,
        "calls already made are not rolled back"
    );
    let before = env.seen.calls.load(Ordering::Acquire);
    spec.activation = unique("act");
    spec.max_host_calls = 4;
    let reply = env.execute(&spec);
    assert_eq!(reply.status, 200);
    assert_eq!(env.seen.calls.load(Ordering::Acquire) - before, 4);
    // A budget above the worker ceiling is refused before anything is compiled.
    let env = start(Options {
        max_memory_bytes: 2 * 1024 * 1024,
        ..Options::default()
    });
    let mut spec = Spec::new(&env, OK, &[]);
    spec.memory_bytes = 4 * 1024 * 1024;
    let reply = env.execute(&spec);
    assert_answers(
        &reply,
        &spec,
        "invalid_request",
        "budget_above_worker_ceiling",
    );
    assert_eq!(reply.status, 400);
    assert_eq!(env.server.validations(), 0, "refused before compile");
    assert_eq!(env.seen.binds.load(Ordering::Acquire), 0);
}

/// One absolute deadline: setup that crosses it never starts the guest; a host call near expiry
/// ends at it; a host call that starts after it is refused unseen; the deadline outranks the
/// host-call budget.
pub fn one_absolute_deadline_end_to_end() {
    let timeout = Duration::from_millis(500);
    // Setup crosses `until`: the guest never runs.
    let bind = gate();
    let env = start(Options {
        bind_gate: Some(bind.clone()),
        ..Options::default()
    });
    let mut spec = Spec::new(&env, OK, &[]);
    spec.timeout_ms = timeout.as_millis() as u64;
    let started = Instant::now();
    let reply = env.execute(&spec);
    assert_answers(&reply, &spec, "timeout", "deadline_exceeded");
    assert_eq!(reply.status, 504);
    env.assert_released_after_deadline();
    assert!(
        started.elapsed() <= timeout + TERMINATION + SLACK,
        "as the client saw it"
    );
    assert_eq!(
        env.seen.binds.load(Ordering::Acquire),
        1,
        "the setup stage was reached"
    );
    assert_eq!(
        env.seen.entries.load(Ordering::Acquire),
        0,
        "the guest never ran"
    );
    assert_eq!(env.server.running_executions(), 0);
    bind.store(true, Ordering::Release);
    wait_for(
        || env.server.active_steps() == 0,
        "the abandoned setup ends",
    );
    env.recovered();

    // A host call that blocks near expiry ends at `until`, not at `until` plus the call timeout.
    let call = gate();
    let env = start(Options {
        call_gate: Some(call.clone()),
        ..Options::default()
    });
    let mut spec = Spec::new(&env, BLOCK, &[LOGGING]);
    spec.timeout_ms = timeout.as_millis() as u64;
    let started = Instant::now();
    let reply = env.execute(&spec);
    assert_answers(&reply, &spec, "timeout", "deadline_exceeded");
    env.assert_released_after_deadline();
    assert!(
        started.elapsed() <= timeout + TERMINATION + SLACK,
        "as the client saw it"
    );
    assert_eq!(
        env.seen.calls.load(Ordering::Acquire),
        1,
        "the host saw the blocked call"
    );
    call.store(true, Ordering::Release);
    env.recovered();

    // Instantiation ends after `until`: the guest never starts, though the handler was next.
    let release = Arc::new(Mutex::new(None));
    let env = start(Options {
        start_release: Some(release.clone()),
        ..Options::default()
    });
    let mut spec = Spec::new(&env, BLOCK, &[LOGGING]);
    spec.timeout_ms = timeout.as_millis() as u64;
    *release.lock().unwrap() = Some(Instant::now() + timeout + Duration::from_millis(300));
    let reply = env.execute(&spec);
    assert_answers(&reply, &spec, "timeout", "deadline_exceeded");
    assert_eq!(
        env.seen.calls.load(Ordering::Acquire),
        0,
        "the handler never started"
    );
    assert_eq!(env.seen.entries.load(Ordering::Acquire), 0);
    *release.lock().unwrap() = Some(Instant::now());
    env.recovered();

    // A host call that starts after `until` is refused, and the host sees none; the deadline
    // also outranks the host-call budget when the late call would have been over it.
    for max_host_calls in [10_000, 1] {
        let release = Arc::new(Mutex::new(None));
        let env = start(Options {
            entry_block: Some((u64::from(max_host_calls == 1), release.clone())),
            ..Options::default()
        });
        let mut spec = Spec::new(&env, FOUR_LOGS, &[LOGGING]);
        spec.timeout_ms = timeout.as_millis() as u64;
        spec.max_host_calls = max_host_calls;
        // The entry is held until after the deadline has passed.
        *release.lock().unwrap() = Some(Instant::now() + timeout + Duration::from_millis(300));
        let reply = env.execute(&spec);
        assert_answers(&reply, &spec, "timeout", "deadline_exceeded");
        assert_eq!(reply.status, 504);
        let seen = env.seen.calls.load(Ordering::Acquire);
        assert_eq!(
            seen,
            usize::from(max_host_calls == 1),
            "the host saw no late call"
        );
        env.recovered();
    }
}

/// How an execution that waits on a stalled step is ended.
#[derive(Clone, Copy, Debug)]
enum Ending {
    Deadline,
    Cancel,
}

/// The first operation to reach `stage` stalls. The execution leaves it on its deadline or on
/// cancel, releases its slot within the termination budget (by the server's own clock), and the
/// next execute is admitted and completes while the abandoned operation is still stalled. Once
/// the abandoned operation ends, the cache holds only the next execute's complete entry.
fn a_stalled_step_is_abandoned(stage: Stage, artifact: &[u8], ending: Ending) {
    let stall = gate();
    let env = start(Options {
        stage_gate: Some((stage, stall.clone())),
        ..Options::default()
    });
    let mut spec = Spec::new(&env, artifact, &[]);
    if let Ending::Deadline = ending {
        spec.timeout_ms = 500;
    }
    env.server.reset_marks();
    let running = env.spawn_execute(&spec);
    wait_for(
        || env.seen.stage_hits.load(Ordering::Acquire) >= 1,
        "the stage was reached",
    );
    match ending {
        Ending::Deadline => {
            let reply = running.join().unwrap();
            assert_answers(&reply, &spec, "timeout", "deadline_exceeded");
            assert_eq!(reply.status, 504);
            env.assert_released_after_deadline();
        }
        Ending::Cancel => {
            let cancel = env.cancel(&spec);
            assert_eq!(cancel_state(&cancel), CancelState::Acknowledged);
            let reply = running.join().unwrap();
            assert_answers(&reply, &spec, "cancelled", "cancel_requested");
            assert_eq!(reply.status, 499);
            env.assert_released_after_cancel();
        }
    }
    assert_eq!(
        env.server.running_executions(),
        0,
        "the slot is free at {stage:?} after {ending:?}"
    );
    assert!(
        env.server.active_steps() >= 1,
        "the abandoned operation is still stalled"
    );
    // A temporary file may exist while publication is stalled; only the rename publishes.
    assert!(
        env.cache_entries()
            .iter()
            .all(|entry| !entry.ends_with(".cache")),
        "nothing is published while it is stalled"
    );
    // Admitted and complete while the abandoned operation is still stalled.
    let next = Spec::new(&env, OK, &[]);
    assert_eq!(
        env.execute(&next).status,
        200,
        "the next execute is admitted at {stage:?} after {ending:?}"
    );
    assert!(
        env.server.active_steps() >= 1,
        "still stalled when the next execute finished"
    );
    stall.store(true, Ordering::Release);
    wait_for(|| env.server.active_steps() == 0, "the abandoned step ends");
    let entries = env.cache_entries();
    assert_eq!(
        entries.len(),
        1,
        "no partial or abandoned entry at {stage:?} after {ending:?}: {entries:?}"
    );
    assert!(entries[0].ends_with(".cache"));
    assert_eq!(env.execute(&next).status, 200, "the entry loads");
}

/// Cache operations (lookup and read, compile, publish) are supervised: on the deadline or on
/// cancel the slot is released within the termination budget, the following execute is admitted
/// while the abandoned operation is still stalled, and nothing partial is published.
pub fn cache_stalls_are_supervised() {
    for stage in [Stage::Read, Stage::Compile, Stage::Publish] {
        for ending in [Ending::Deadline, Ending::Cancel] {
            a_stalled_step_is_abandoned(stage, OK, ending);
        }
    }
}

/// Storage open and drop callbacks, and calls the authorization refuses, count against
/// `max_host_calls` like every other guest-to-host entry. The guest opens one object twice and
/// drops each handle it gets: with the key granted that is four entries (two opens, two drops),
/// with no grant it is two refused opens. The call after the budget is refused, and none of
/// these entries reaches the platform bindings.
pub fn storage_and_refused_entries_count_against_the_budget() {
    let env = start(Options::default());
    // The guest opens `objects/a`. A grant over `objects/` allows it; a grant over another
    // prefix links the interface but refuses the key.
    let with_prefix = |env: &Env, prefix: &str, max_host_calls: u64| {
        let mut spec = Spec::new(env, STORAGE_CALLS, &[STORAGE]);
        spec.grants = vec![json!({
            "family": "storage",
            "interface": STORAGE,
            "scope": {"prefixes": [prefix]},
            "secretRef": {"name": "storage-binding-qualification", "version": "1"}
        })];
        spec.max_host_calls = max_host_calls;
        spec
    };
    let granted = |env: &Env, max_host_calls: u64| with_prefix(env, "objects/", max_host_calls);
    let refused = |env: &Env, max_host_calls: u64| with_prefix(env, "elsewhere/", max_host_calls);
    let entries = |env: &Env| env.seen.entries.load(Ordering::Acquire);
    let over_budget = |reply: &Reply, spec: &Spec| {
        assert_answers(reply, spec, "execution_failed", "host_call_budget_exceeded");
        assert_eq!(reply.json()["result"]["retryable"], false);
    };

    // Granted: open, drop, open, drop. Exactly four entries fit; a fifth would not be needed.
    let before = entries(&env);
    let spec = granted(&env, 4);
    let reply = env.execute(&spec);
    assert_eq!(reply.status, 200, "four entries fit a budget of four");
    assert_eq!(entries(&env) - before, 4, "open and drop both enter");
    // One short: the second drop is the entry that is refused, so drop callbacks count.
    let before = entries(&env);
    let spec = granted(&env, 3);
    over_budget(&env.execute(&spec), &spec);
    assert_eq!(entries(&env) - before, 4, "the refused entry is the drop");
    // Two short: the first drop is refused.
    let before = entries(&env);
    let spec = granted(&env, 1);
    over_budget(&env.execute(&spec), &spec);
    assert_eq!(entries(&env) - before, 2);

    // Refused by authorization: no handle, two entries, both counted.
    let before = entries(&env);
    let spec = refused(&env, 2);
    let reply = env.execute(&spec);
    assert_eq!(reply.status, 200, "refused opens are answered to the guest");
    assert_eq!(entries(&env) - before, 2, "a refused call still enters");
    let before = entries(&env);
    let spec = refused(&env, 1);
    over_budget(&env.execute(&spec), &spec);
    assert_eq!(entries(&env) - before, 2, "a refused call counts");
    assert_eq!(
        env.seen.calls.load(Ordering::Acquire),
        0,
        "no storage entry reached the platform"
    );
    env.recovered();
}

// --- the production worker -----------------------------------------------------------------

/// The artifact the production-worker cases publish and execute.
pub fn production_artifact() -> &'static [u8] {
    OK
}

/// The artifact's digest, as the file name the worker reads it by.
pub fn production_digest() -> String {
    format!("{:x}", Sha256::digest(production_artifact()))
}

/// The worker's configuration file, as the worker pod mounts it: the four listener ports, and
/// the artifact and cache directories under `root`.
pub fn worker_config(execute: u16, control: u16, metrics: u16, probe: u16, root: &Path) -> String {
    format!(
        "listen_port = {execute}\ncontrol_port = {control}\nmetrics_port = {metrics}\n\
         probe_port = {probe}\npool_id = \"pool-serve-test\"\ncache_path = \"{}\"\n\
         artifact_path = \"{}\"\nfalcone_api_endpoint = \"https://falcone.example.test/api\"\n\
         falcone_dispatcher_endpoint = \"https://dispatcher.example.test\"\n",
        root.join("cache").display(),
        root.join("artifacts").display()
    )
}

/// The worker binary, started from its configuration (locally as a process, in the pod as the
/// production image), serves the execution API: a wasm revision returns the legacy envelope with
/// the pinned runtime identity and no credential string, a non-wasm revision is refused, and each
/// listener refuses the other's operations. `production_artifact` is published under its digest.
pub fn the_production_worker_serves_the_execution_api(execute: SocketAddr, control: SocketAddr) {
    let listening = Instant::now();
    for addr in [execute, control] {
        while TcpStream::connect(addr).is_err() {
            assert!(
                listening.elapsed() < Duration::from_secs(60),
                "the worker listens on {addr}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let spec = Spec::for_digest(format!("sha256:{}", production_digest()), &[]);
    let reply = post(execute, "/v1/execute", &spec.bytes());
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    api::parse_result(&reply.body, &spec.parsed()).expect("a result that matches its request");
    let value = reply.json();
    assert_eq!(value["status"], "success");
    assert_eq!(value["result"], json!({"statusCode": 200, "body": "ok"}));
    let identity = &value["identity"];
    assert_eq!(identity["wasmtime_version"], ENGINE_VERSION);
    assert_eq!(identity["runtime_mode"], "wasm");
    assert_eq!(identity["artifact_digest"], spec.digest.as_str());
    assert_eq!(identity["function_revision"], "rev-qualification-0001");
    assert_no_leak(&reply, &spec.all_secrets());

    // A non-wasm revision is refused, and nothing runs.
    let mut oci = Spec::for_digest(spec.digest.clone(), &[]);
    oci.mode = "oci".into();
    let reply = post(execute, "/v1/execute", &oci.bytes());
    assert_answers(
        &reply,
        &oci,
        "runtime_mode_not_supported",
        "runtime_mode_not_wasm",
    );
    assert_eq!(reply.status, 409);

    // Each listener refuses the other's operations; a cancel for nothing is not found.
    let reply = post(control, "/v1/execute", &spec.bytes());
    assert_failure(&reply, 404, "not_found", "execution_not_found");
    let reply = post(
        execute,
        "/v1/cancel",
        &serde_json::to_vec(&spec.cancel_message()).unwrap(),
    );
    assert_failure(&reply, 404, "not_found", "execution_not_found");
    let reply = post(
        control,
        "/v1/cancel",
        &serde_json::to_vec(&spec.cancel_message()).unwrap(),
    );
    assert_failure(&reply, 404, "not_found", "execution_not_found");
}

/// A component near the artifact bound: a valid component with a large custom section.
fn large_artifact() -> Vec<u8> {
    let name = b"padding";
    let size = 63 * 1024 * 1024;
    let mut section = vec![0u8];
    let mut length = name.len() + 1 + size;
    loop {
        let byte = (length & 0x7f) as u8;
        length >>= 7;
        if length == 0 {
            section.push(byte);
            break;
        }
        section.push(byte | 0x80);
    }
    section.push(name.len() as u8);
    section.extend_from_slice(name);
    section.resize(section.len() + size, 0);
    let mut bytes = OK.to_vec();
    bytes.extend_from_slice(&section);
    bytes
}

/// A cold, large artifact cancelled or timed out during validation: the outcome is `cancelled`
/// or `timeout`, the slot is free within the termination budget, and nothing is published.
pub fn validation_is_cancellable_and_bounded() {
    let big = large_artifact();
    for ending in [Ending::Cancel, Ending::Deadline] {
        a_stalled_step_is_abandoned(Stage::Validate, &big, ending);
    }

    // No stall at all: a 1 ms timeout cannot cover reading and validating the artifact.
    let env = start(Options::default());
    let mut spec = Spec::new(&env, &big, &[]);
    spec.timeout_ms = 1;
    env.server.reset_marks();
    let reply = env.execute(&spec);
    assert_answers(&reply, &spec, "timeout", "deadline_exceeded");
    env.assert_released_after_deadline();
    wait_for(
        || env.server.active_steps() == 0,
        "the abandoned validation ends",
    );
    assert!(env.cache_entries().is_empty());
    env.recovered();
}

/// Every case of this module, by the name the restricted pod selects it with.
pub const CASES: &[(&str, fn())] = &[
    ("identity", legacy_envelope_and_identity),
    ("non-wasm", non_wasm_refused_without_instance),
    ("engine-version", another_engine_version_is_refused),
    ("credential", credential_never_leaves_the_host_layer),
    ("redaction", redaction_holds_across_the_cut),
    ("imports", imports_must_be_declared_and_allowed),
    ("listeners", each_listener_refuses_the_others_operations),
    ("saturation", cancel_survives_a_saturated_execute_listener),
    (
        "saturation-control",
        a_stalled_control_listener_does_not_touch_executions,
    ),
    ("admission-order", admission_order_when_every_slot_is_busy),
    ("update-cancel", update_and_cancel_semantics),
    ("http", http_ceilings_and_deadlines),
    ("control-http", control_ceilings_and_deadlines),
    (
        "slow-reader",
        a_slow_reader_is_cut_off_at_the_response_deadline,
    ),
    ("timeout-selection", the_shortest_timeout_wins),
    ("budgets", budgets_are_enforced),
    (
        "budget-entries",
        storage_and_refused_entries_count_against_the_budget,
    ),
    ("deadline", one_absolute_deadline_end_to_end),
    ("cache-stalls", cache_stalls_are_supervised),
    ("validation", validation_is_cancellable_and_bounded),
];
