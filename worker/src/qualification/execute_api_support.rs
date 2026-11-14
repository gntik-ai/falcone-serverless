//! Shared harness of the execution API cases (local tests and the restricted-pod runner): a server on loopback with a test platform,
//! request builders and a raw HTTP client. Nothing here sleeps for a fixed time; tests wait on
//! server-side events (a guest started, a stage was reached) with a safety timeout only.

use falcone_serverless_worker::api::WorkerCeilings;
use falcone_serverless_worker::host::{Bindings, Context, HostError, Operation, Reply};
use falcone_serverless_worker::server::{
    ListenerLimits, Platform, Server, ServerConfig, Unavailable,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The longest any test waits for an event that must happen; never a tuning value.
pub const SAFETY: Duration = Duration::from_secs(60);

pub const ENGINE: &str = "49.0.5";

pub fn fixture(name: &str) -> Vec<u8> {
    match name {
        "api-ok" => &include_bytes!("execute_fixtures/api-ok.wasm")[..],
        "api-context" => &include_bytes!("execute_fixtures/api-context.wasm")[..],
        "api-log-ok" => &include_bytes!("execute_fixtures/api-log-ok.wasm")[..],
        "api-log-spin" => &include_bytes!("execute_fixtures/api-log-spin.wasm")[..],
        "api-log-four" => &include_bytes!("execute_fixtures/api-log-four.wasm")[..],
        "api-log-big" => &include_bytes!("execute_fixtures/api-log-big.wasm")[..],
        "api-probe" => &include_bytes!("execute_fixtures/api-probe.wasm")[..],
        "api-publish" => &include_bytes!("execute_fixtures/api-publish.wasm")[..],
        "wasi-import" => &include_bytes!("execute_fixtures/wasi-import.wasm")[..],
        _ => panic!("unknown fixture {name}"),
    }
    .to_vec()
}

/// The trusted compiler: `artifact-compile-child` beside the running executable, as the
/// worker finds it. Cargo puts integration tests one directory deeper, in `deps`.
pub fn compiler_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let parent = exe.parent().unwrap();
    let binaries = if parent.file_name().is_some_and(|name| name == "deps") {
        parent.parent().unwrap()
    } else {
        parent
    };
    binaries.join("artifact-compile-child")
}

pub fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn digest_text(digest: &[u8; 32]) -> String {
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

/// A counter that tests can wait on.
#[derive(Default)]
pub struct Signal {
    count: Mutex<usize>,
    changed: Condvar,
}

impl Signal {
    pub fn raise(&self) {
        *self.count.lock().unwrap() += 1;
        self.changed.notify_all();
    }

    pub fn count(&self) -> usize {
        *self.count.lock().unwrap()
    }

    /// Waits until the signal was raised `n` times.
    pub fn wait_for(&self, n: usize) {
        let deadline = Instant::now() + SAFETY;
        let mut count = self.count.lock().unwrap();
        while *count < n {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for event {n}");
            count = self.changed.wait_timeout(count, left).unwrap().0;
        }
    }
}

/// A gate a test holds closed to keep the server blocked inside a stage.
pub struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            open: Mutex::new(false),
            changed: Condvar::new(),
        })
    }

    pub fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    pub fn wait(&self) {
        let deadline = Instant::now() + SAFETY;
        let mut open = self.open.lock().unwrap();
        while !*open {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            open = self.changed.wait_timeout(open, left).unwrap().0;
        }
    }
}

/// What the host saw of the guests.
#[derive(Default)]
pub struct Probe {
    /// Calls that reached the host's bindings.
    pub host_calls: AtomicUsize,
    /// Raised by the first host call of a guest: the guest started.
    pub guest_started: Signal,
    /// While set, a host call blocks here (a call blocked inside the host).
    pub block: Mutex<Option<Arc<Gate>>>,
}

pub struct ProbeBindings(pub Arc<Probe>);

impl Bindings for ProbeBindings {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        self.0.host_calls.fetch_add(1, Ordering::SeqCst);
        self.0.guest_started.raise();
        let gate = self.0.block.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.wait();
        }
        Ok(Reply::Empty)
    }
}

/// How a stage of the test platform makes the execution stop while the stage is running, and
/// only then returns: the stop condition already holds when the stage's value is ready, so
/// no timing decides whether the server sees the stop or the value first.
#[derive(Clone, Copy, Debug)]
pub enum Interference {
    /// Raises the execution's cancellation.
    Cancel,
    /// Waits for the execution's deadline to pass (yielding, never sleeping).
    Expire,
}

impl Interference {
    fn apply(self, until: Instant, cancel: &AtomicBool) {
        match self {
            Self::Cancel => cancel.store(true, Ordering::Release),
            Self::Expire => {
                while Instant::now() < until {
                    std::thread::yield_now();
                }
            }
        }
    }
}

#[derive(Default)]
pub struct TestPlatform {
    artifacts: Mutex<HashMap<[u8; 32], Arc<[u8]>>>,
    pub probe: Arc<Probe>,
    /// Raised each time the server asks for an artifact.
    pub artifact_requests: Signal,
    /// Raised when `bind` is entered; the stage was reached.
    pub bind_entered: Signal,
    /// While set, `bind` blocks on it and ignores the deadline and the cancellation.
    pub bind_gate: Mutex<Option<Arc<Gate>>>,
    /// Applied by `artifact` before it answers.
    pub artifact_interference: Mutex<Option<Interference>>,
    /// Applied by `bind` before it fails: while set, `bind` fails.
    pub bind_interference: Mutex<Option<Interference>>,
}

impl TestPlatform {
    pub fn add(&self, bytes: Vec<u8>) -> [u8; 32] {
        let digest = digest(&bytes);
        self.artifacts
            .lock()
            .unwrap()
            .insert(digest, Arc::from(bytes));
        digest
    }
}

impl Platform for TestPlatform {
    fn artifact(
        &self,
        plan: &falcone_serverless_worker::api::Plan,
        until: Instant,
        cancel: &AtomicBool,
    ) -> Result<Arc<[u8]>, Unavailable> {
        self.artifact_requests.raise();
        let interference = *self.artifact_interference.lock().unwrap();
        if let Some(interference) = interference {
            interference.apply(until, cancel);
        }
        self.artifacts
            .lock()
            .unwrap()
            .get(&plan.artifact_digest)
            .cloned()
            .ok_or(Unavailable)
    }

    fn bind(
        &self,
        _: &falcone_serverless_worker::api::Plan,
        until: Instant,
        cancel: &AtomicBool,
    ) -> Result<Arc<dyn Bindings>, Unavailable> {
        self.bind_entered.raise();
        let interference = *self.bind_interference.lock().unwrap();
        if let Some(interference) = interference {
            interference.apply(until, cancel);
            return Err(Unavailable);
        }
        let gate = self.bind_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.wait();
        }
        Ok(Arc::new(ProbeBindings(self.probe.clone())))
    }
}

pub struct Cluster {
    pub server: Server,
    pub platform: Arc<TestPlatform>,
    directory: std::path::PathBuf,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl Cluster {
    pub fn execute(&self) -> SocketAddr {
        self.server.execute_addr()
    }

    pub fn control(&self) -> SocketAddr {
        self.server.control_addr()
    }
}

pub fn config(concurrency: usize, directory: &std::path::Path) -> ServerConfig {
    let loopback = |port| SocketAddr::from(([127, 0, 0, 1], port));
    ServerConfig {
        execute_bind: loopback(0),
        control_bind: loopback(0),
        concurrency,
        ceilings: WorkerCeilings {
            max_memory_bytes: 512 * 1024 * 1024,
            max_wall_clock_ms: 300_000,
        },
        execute: ListenerLimits::execute(concurrency),
        control: ListenerLimits::control(),
        cache_dir: directory.join("cache"),
        compiler: compiler_path(),
    }
}

pub fn start(concurrency: usize) -> Cluster {
    start_with(concurrency, |_| {})
}

pub fn start_with(concurrency: usize, tweak: impl FnOnce(&mut ServerConfig)) -> Cluster {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "execute-api-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let mut config = config(concurrency, &directory);
    tweak(&mut config);
    let platform = Arc::new(TestPlatform::default());
    let server = Server::start(config, platform.clone()).unwrap();
    Cluster {
        server,
        platform,
        directory,
    }
}

// --- requests -------------------------------------------------------------------------------

fn civil(seconds: u64) -> (i64, i64, i64, u64) {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // civil_from_days
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day, rest)
}

/// The deadline text for `from_now` after the current instant.
pub fn deadline_in(from_now: Duration) -> String {
    let at = SystemTime::now() + from_now;
    let since = at.duration_since(UNIX_EPOCH).unwrap();
    let (year, month, day, rest) = civil(since.as_secs());
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        since.subsec_millis()
    )
}

/// One execute request; every field a test varies is public.
#[derive(Clone)]
pub struct Spec {
    pub digest: [u8; 32],
    pub imports: Vec<String>,
    pub fuel: u64,
    pub memory_bytes: u64,
    pub timeout_ms: u64,
    pub max_host_calls: u64,
    pub deadline_in: Duration,
    pub runtime_mode: String,
    pub engine_version: String,
    /// Characters of `__ow_body`; zero omits it.
    pub body_chars: usize,
    pub activation: String,
    /// The credential's `tenant`: a string the redactor must remove from logs.
    pub tenant: String,
    /// A value placed in the action parameters (and so in guest memory).
    pub note: Option<String>,
    /// Event topics the revision is granted.
    pub topics: Vec<String>,
}

static IDS: AtomicUsize = AtomicUsize::new(1);

impl Spec {
    pub fn new(digest: [u8; 32]) -> Self {
        let id = IDS.fetch_add(1, Ordering::Relaxed);
        Self {
            digest,
            imports: Vec::new(),
            fuel: 100_000_000_000,
            memory_bytes: 64 * 1024 * 1024,
            timeout_ms: 30_000,
            max_host_calls: 10_000,
            deadline_in: Duration::from_secs(30),
            runtime_mode: "wasm".into(),
            engine_version: ENGINE.into(),
            body_chars: 0,
            activation: format!("act-local-{id:05}"),
            tenant: "tenant-local".into(),
            note: None,
            topics: Vec::new(),
        }
    }

    pub fn imports(mut self, imports: &[&str]) -> Self {
        self.imports = imports.iter().map(|import| (*import).to_owned()).collect();
        self
    }

    pub fn attempt(&self) -> String {
        format!("att-{}", self.activation)
    }

    pub fn value(&self) -> Value {
        let attempt = self.attempt();
        let mut params = json!({
            "__ow_method": "get",
            "__ow_path": "/v1/local",
            "__ow_headers": {},
            "__ow_query": {}
        });
        if let Some(note) = &self.note {
            params["note"] = note.clone().into();
        }
        if self.body_chars > 0 {
            params["__ow_body"] = "x".repeat(self.body_chars).into();
        }
        json!({
            "version": "1",
            "type": "execute",
            "activation_id": self.activation,
            "attempt_id": attempt,
            "attempt_number": 1,
            "deadline": deadline_in(self.deadline_in),
            "invocation": {"invocation_key": "inv-local-1"},
            "credential": {
                "iss": "issuer-canary-local",
                "aud": "worker-canary-local",
                "tenant": self.tenant,
                "workspace": "workspace-local",
                "pool_profile": "profile-local",
                "revision": "rev-local-1",
                "activation": self.activation,
                "attempt": attempt,
                "iat": 1,
                "exp": 2,
                "allowed_capabilities": []
            },
            "projection": {
                "revision": "rev-local-1",
                "runtime_mode": self.runtime_mode,
                "artifact_digest": digest_text(&self.digest),
                "component_digest": digest_text(&self.digest),
                "language_profile": {"name": "profile-local", "version": "1"},
                "engine_version": self.engine_version,
                "wasi_version": "0.2.6",
                "interface_version": "0.2.0",
                "imports": self.imports,
                "budgets": {
                    "fuel": self.fuel,
                    "memory_bytes": self.memory_bytes,
                    "timeout_ms": self.timeout_ms,
                    "max_host_calls": self.max_host_calls
                },
                "grants": if self.topics.is_empty() {
                    json!([])
                } else {
                    json!([{
                        "family": "event",
                        "interface": "falcone:host/events@0.2.0",
                        "scope": {"topics": self.topics}
                    }])
                }
            },
            "params": params
        })
    }

    pub fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.value()).unwrap()
    }

    pub fn cancel(&self) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "version": "1",
            "type": "cancel",
            "activation_id": self.activation,
            "attempt_id": self.attempt(),
            "cancel": {"state": "requested"}
        }))
        .unwrap()
    }
}

// --- client ---------------------------------------------------------------------------------

pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Answer {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!(
                "not JSON: {}",
                String::from_utf8_lossy(&self.body[..self.body.len().min(200)])
            )
        })
    }

    /// `result.code` and `result.detail.reason` of a failure envelope.
    pub fn failure(&self) -> (String, String) {
        let body = self.json();
        let result = body.get("result").unwrap_or(&body);
        (
            result["code"].as_str().unwrap_or_default().to_owned(),
            result["detail"]["reason"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    }
}

pub fn connect(address: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(SAFETY)).unwrap();
    stream
}

pub fn head(path: &str, length: usize) -> Vec<u8> {
    format!("POST {path} HTTP/1.1\r\nHost: worker\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n")
        .into_bytes()
}

/// Reads a response to the end of the stream. Whatever arrived is returned even when the
/// connection then fails, as a peer that closes with unread data may reset it.
pub fn read_answer(stream: &mut TcpStream) -> Option<Answer> {
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
        }
    }
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n")?;
    let status = std::str::from_utf8(&bytes[..split])
        .ok()?
        .split(' ')
        .nth(1)?
        .parse()
        .ok()?;
    Some(Answer {
        status,
        body: bytes[split + 4..].to_vec(),
    })
}

pub fn post(address: SocketAddr, path: &str, body: &[u8]) -> Answer {
    let mut stream = connect(address);
    let _ = stream.write_all(&head(path, body.len()));
    let _ = stream.write_all(body);
    read_answer(&mut stream).expect("a response")
}

pub fn spawn_post(
    address: SocketAddr,
    path: &'static str,
    body: Vec<u8>,
) -> std::thread::JoinHandle<Answer> {
    std::thread::spawn(move || post(address, path, &body))
}

/// The status of a cancel answered by the control listener, and its state.
pub fn cancel(cluster: &Cluster, spec: &Spec) -> (u16, String) {
    let answer = post(cluster.control(), "/v1/cancel", &spec.cancel());
    let state = answer
        .json()
        .get("cancel")
        .and_then(|cancel| cancel["state"].as_str())
        .unwrap_or_default()
        .to_owned();
    (answer.status, state)
}
