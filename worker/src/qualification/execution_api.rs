//! RUN-01 qualification of the worker execution API (`falcone-serverless-worker serve`).
//!
//! Black-box cases: each starts the real worker binary on ephemeral ports in a scratch
//! directory, publishes embedded fixture components to its artifact store, and talks to it over
//! TCP like the dispatcher would. The same cases run
//!
//! - locally, from the `execute_api_local` integration test under `cargo test`, and
//! - in the restricted fs-test pod, through `compile-cache-qualification --execution-api=<case>`
//!   (the `cluster_execute_api` integration test), where the pod runs under Pod Security restricted,
//!   seccomp `RuntimeDefault`, a read-only root filesystem and an arbitrary non-root UID.
//!
//! Synchronization uses server-side events (the worker's structured log on standard error:
//! `listening`, `guest_started`, `execution_finished`, `connection_closed`,
//! `response_deadline_exceeded`), never a wait shorter than a documented deadline. The waits
//! below that are timeouts are fail-safes that turn a hang into a failure.
//!
//! Expected values are literals from the contract and the documented bounds, not values
//! recomputed the way the worker computes them.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ECHO: &[u8] = include_bytes!("execute_fixtures/handler.wasm");
const WASI_IMPORT: &[u8] = include_bytes!("execute_fixtures/wasi-import.wasm");
const SPIN: &[u8] = include_bytes!("api_fixtures/spin.wasm");
const HOST_CALLS_11: &[u8] = include_bytes!("api_fixtures/host-calls-11.wasm");
const LOG_THEN_SPIN: &[u8] = include_bytes!("api_fixtures/log-then-spin.wasm");
const LOG_ECHO: &[u8] = include_bytes!("api_fixtures/log-echo.wasm");
const MAX_LOGS: &[u8] = include_bytes!("api_fixtures/max-logs.wasm");
const MEMORY_DUMP: &[u8] = include_bytes!("api_fixtures/memory-dump.wasm");
const CONTEXT_IMPORT: &[u8] = include_bytes!("api_fixtures/context-import.wasm");

const LOGGING: &str = "falcone:host/logging@0.2.0";
const CONTEXT: &str = "falcone:host/invocation-context@0.2.0";
const PINNED_WASMTIME: &str = "49.0.5";

// The documented bounds (execution-api-v1.md), as literals.
const HEADER_DEADLINE: Duration = Duration::from_secs(5);
const BODY_INACTIVITY: Duration = Duration::from_secs(5);
const RESPONSE_DEADLINE: Duration = Duration::from_secs(5);
const ADMISSION_ALLOWANCE: usize = 8;
const CONTROL_CONNECTIONS: usize = 16;
/// Slack for scheduling on a loaded machine; a bound, never a synchronization.
const TOLERANCE: Duration = Duration::from_secs(3);
/// A hang becomes a failure after this long.
const FAILSAFE: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------------------------------------
// The worker under test
// ---------------------------------------------------------------------------------------------

struct Events {
    log: Mutex<Vec<(Instant, Value)>>,
    changed: Condvar,
}

impl Events {
    fn len(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    /// The first event at or after `from` that satisfies `pred`: its index, arrival and value.
    fn wait(&self, from: usize, pred: impl Fn(&Value) -> bool) -> (usize, Instant, Value) {
        let deadline = Instant::now() + FAILSAFE;
        let mut log = self.log.lock().unwrap();
        loop {
            if let Some(found) = log
                .iter()
                .enumerate()
                .skip(from)
                .find(|(_, (_, event))| pred(event))
            {
                return (found.0, found.1.0, found.1.1.clone());
            }
            let left = deadline
                .checked_duration_since(Instant::now())
                .expect("fail-safe: the expected worker event never came");
            log = self.changed.wait_timeout(log, left).unwrap().0;
        }
    }

    fn count(&self, pred: impl Fn(&Value) -> bool) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| pred(e))
            .count()
    }

    fn lines(&self) -> String {
        let log = self.log.lock().unwrap();
        log.iter()
            .map(|(_, event)| event.to_string() + "\n")
            .collect()
    }
}

fn event_is(name: &'static str, id: &str) -> impl Fn(&Value) -> bool {
    let id = id.to_owned();
    move |event| event["event"] == name && event["activation_id"] == id
}

#[derive(Clone)]
struct Settings {
    concurrency: u32,
    max_wall_clock_ms: u64,
    max_memory_bytes: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            concurrency: 2,
            max_wall_clock_ms: 300_000,
            max_memory_bytes: 536_870_912,
        }
    }
}

struct Worker {
    child: Child,
    execute: SocketAddr,
    control: SocketAddr,
    events: Arc<Events>,
    root: PathBuf,
}

fn binaries() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let parent = exe.parent().unwrap();
    // Cargo integration tests live in deps/; in the pod every binary is in one directory.
    if parent.file_name().is_some_and(|name| name == "deps") {
        parent.parent().unwrap().to_path_buf()
    } else {
        parent.to_path_buf()
    }
}

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Worker {
    fn start(settings: Settings) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "execution-api-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let config = root.join("worker.toml");
        std::fs::write(
            &config,
            format!(
                "pool_id = \"qualification\"\n\
                 falcone_api_endpoint = \"https://falcone.invalid/api\"\n\
                 falcone_dispatcher_endpoint = \"https://dispatcher.invalid\"\n\
                 listen_port = 0\ncontrol_port = 0\nconcurrency = {}\n\
                 default_wall_clock_ms = {}\nmax_wall_clock_ms = {}\n\
                 default_memory_bytes = {}\nmax_memory_bytes = {}\n\
                 cache_path = \"{}\"\n",
                settings.concurrency,
                settings.max_wall_clock_ms.min(30_000),
                settings.max_wall_clock_ms,
                settings.max_memory_bytes.min(134_217_728),
                settings.max_memory_bytes,
                root.join("cache").display(),
            ),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("cache/artifacts")).unwrap();
        let mut child = Command::new(binaries().join("falcone-serverless-worker"))
            .arg("serve")
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the worker binary is built beside the tests");
        let events = Arc::new(Events {
            log: Mutex::new(Vec::new()),
            changed: Condvar::new(),
        });
        let stderr = child.stderr.take().unwrap();
        let sink = events.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let event = serde_json::from_str(&line)
                    .unwrap_or_else(|_| json!({"event": "stderr", "line": line}));
                sink.log.lock().unwrap().push((Instant::now(), event));
                sink.changed.notify_all();
            }
        });
        let (_, _, listening) = events.wait(0, |event| event["event"] == "listening");
        let port = |key: &str| -> SocketAddr {
            let bound: SocketAddr = listening[key].as_str().unwrap().parse().unwrap();
            SocketAddr::from(([127, 0, 0, 1], bound.port()))
        };
        Self {
            child,
            execute: port("execute"),
            control: port("control"),
            events,
            root,
        }
    }

    /// Stores a component in the worker's trusted artifact store; returns its digest.
    fn publish(&self, bytes: &[u8]) -> String {
        let digest = digest_of(bytes);
        let name = format!("{}.wasm", &digest["sha256:".len()..]);
        std::fs::write(self.root.join("cache/artifacts").join(name), bytes).unwrap();
        digest
    }

    fn execute(&self, request: &Value) -> Reply {
        post(self.execute, "/v1/execute", request.to_string().as_bytes())
    }

    fn spawn_execute(&self, request: &Value) -> std::thread::JoinHandle<Reply> {
        let (addr, body) = (self.execute, request.to_string());
        std::thread::spawn(move || post(addr, "/v1/execute", body.as_bytes()))
    }

    fn cancel(&self, id: &str) -> Reply {
        let body = json!({"version": "1", "type": "cancel", "activation_id": id,
            "attempt_id": format!("att-{id}"), "cancel": {"state": "requested"}});
        post(self.control, "/v1/cancel", body.to_string().as_bytes())
    }

    /// Starts a blocking guest and waits for the worker to report that it started.
    fn start_blocking(&self, spin: &str, id: &str) -> std::thread::JoinHandle<Reply> {
        let from = self.events.len();
        let handle = self.spawn_execute(&Req::new(spin, id).json());
        self.events.wait(from, event_is("guest_started", id));
        handle
    }

    /// A normal execute and a cancel both succeed: no slot or control capacity leaked.
    fn recovers(&self, spin: &str, echo: &str, id: &str) {
        let blocked = self.start_blocking(spin, &format!("{id}-block"));
        let cancelled = self.cancel(&format!("{id}-block"));
        assert_eq!(cancelled.status, 200, "cancel after the case");
        assert_eq!(cancelled.json()["cancel"]["state"], "acknowledged");
        let finished = blocked.join().unwrap();
        assert_eq!(finished.json()["result"]["detail"]["reason"], "cancelled");
        let normal = self.execute(&Req::new(echo, &format!("{id}-echo")).json());
        assert_eq!(normal.status, 200, "execute after the case");
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ---------------------------------------------------------------------------------------------
// HTTP client and requests
// ---------------------------------------------------------------------------------------------

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&self.body)))
    }
}

fn parse_reply(mut raw: Vec<u8>) -> Reply {
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a complete response head");
    let head = String::from_utf8_lossy(&raw[..end]).into_owned();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    Reply {
        status,
        body: raw.split_off(end + 4),
    }
}

fn exchange(addr: SocketAddr, request: &[u8]) -> Reply {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(FAILSAFE)).unwrap();
    // The server may answer and close before it has read everything; its answer still counts.
    let _ = stream.write_all(request);
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    parse_reply(raw)
}

fn post(addr: SocketAddr, path: &str, body: &[u8]) -> Reply {
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: worker\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    exchange(addr, &request)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for a Unix time in milliseconds (civil-from-days, the inverse of
/// the worker's days-from-civil).
fn iso(unix_ms: u64) -> String {
    let seconds = (unix_ms / 1000) as i64;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        unix_ms % 1000
    )
}

/// The execution credential's claim strings used by the cases. They are recognizable words,
/// never a real credential.
const CREDENTIAL_STRINGS: [&str; 6] = [
    "credential-canary-issuer",
    "credential-canary-audience",
    "credential-canary-tenant",
    "credential-canary-workspace",
    "credential-canary-pool",
    "credential-canary-secret",
];

struct Req {
    artifact: String,
    activation: String,
    mode: String,
    engine: String,
    imports: Vec<String>,
    budgets: Value,
    deadline_ms: u64,
    params: Value,
    issuer: String,
    extra_projection: Value,
}

impl Req {
    fn new(artifact: &str, activation: &str) -> Self {
        Self {
            artifact: artifact.to_owned(),
            activation: activation.to_owned(),
            mode: "wasm".into(),
            engine: PINNED_WASMTIME.into(),
            imports: Vec::new(),
            budgets: json!({"fuel": 9_007_199_254_740_991u64, "memory_bytes": 67_108_864u64,
                "timeout_ms": 30_000, "max_host_calls": 5_000}),
            deadline_ms: 120_000,
            params: json!({"__ow_path": "/hello", "__ow_method": "post", "name": "world"}),
            issuer: CREDENTIAL_STRINGS[0].into(),
            extra_projection: json!({}),
        }
    }

    fn imports(mut self, imports: &[&str]) -> Self {
        self.imports = imports.iter().map(|import| (*import).to_owned()).collect();
        self
    }

    fn budget(mut self, key: &str, value: u64) -> Self {
        self.budgets[key] = json!(value);
        self
    }

    /// The request's absolute `deadline`, this long from now.
    fn deadline_in(mut self, ms: u64) -> Self {
        self.deadline_ms = ms;
        self
    }

    fn json(&self) -> Value {
        let revision = format!("rev-{}", self.activation);
        let mut projection = json!({
            "revision": revision,
            "runtime_mode": self.mode,
            "artifact_digest": self.artifact,
            "component_digest": self.artifact,
            "language_profile": {"name": "profile-qualification", "version": "1"},
            "engine_version": self.engine,
            "wasi_version": "0.2.6",
            "interface_version": "0.2.0",
            "imports": self.imports,
            "budgets": self.budgets,
            "grants": []
        });
        for (key, value) in self.extra_projection.as_object().unwrap() {
            projection[key] = value.clone();
        }
        json!({
            "version": "1",
            "type": "execute",
            "activation_id": self.activation,
            "attempt_id": format!("att-{}", self.activation),
            "attempt_number": 1,
            "deadline": iso(now_ms() + self.deadline_ms),
            "invocation": {"invocation_key": format!("inv-{}", self.activation)},
            "credential": {
                "iss": self.issuer, "aud": CREDENTIAL_STRINGS[1],
                "tenant": CREDENTIAL_STRINGS[2], "workspace": CREDENTIAL_STRINGS[3],
                "pool_profile": CREDENTIAL_STRINGS[4],
                "revision": revision,
                "activation": self.activation,
                "attempt": format!("att-{}", self.activation),
                "iat": 1_700_000_000, "exp": 4_100_000_000u64,
                "allowed_capabilities": [{
                    "family": "storage",
                    "interface": "falcone:host/storage@0.1.0",
                    "scope": {"prefixes": ["credential-canary-prefix/"]},
                    "secretRef": {"name": CREDENTIAL_STRINGS[5]}
                }]
            },
            "projection": projection,
            "params": self.params
        })
    }
}

fn expect_failure(reply: &Reply, status: u16, code: &str, reason: &str) -> Value {
    let body = reply.json();
    assert_eq!(reply.status, status, "{body}");
    // A refusal is either a result envelope (activation known) or the bare failure envelope.
    let failure = if body["type"] == "result" {
        &body["result"]
    } else {
        &body
    };
    assert_eq!(failure["status"], status, "{body}");
    assert_eq!(failure["code"], code, "{body}");
    assert_eq!(failure["detail"]["reason"], reason, "{body}");
    body
}

fn expect_success(reply: &Reply) -> Value {
    let body = reply.json();
    assert_eq!(reply.status, 200, "{body}");
    assert_eq!(body["status"], "success", "{body}");
    body
}

/// No guest was instantiated since `from`: the worker reports `guest_started` just before it
/// enters guest code, and never before the instance exists.
fn no_instance(worker: &Worker, from: usize) {
    let started = worker
        .events
        .lock_slice(from)
        .iter()
        .any(|event| event["event"] == "guest_started");
    assert!(!started, "an instance was created");
}

impl Events {
    fn lock_slice(&self, from: usize) -> Vec<Value> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .skip(from)
            .map(|(_, e)| e.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------------------------

/// A wasm execute returns the legacy envelope; the identity matches the projection and the
/// pinned Wasmtime version.
pub fn identity() {
    let worker = Worker::start(Settings::default());
    let echo = worker.publish(ECHO);
    let request = Req::new(&echo, "act-identity").json();
    let body = expect_success(&worker.execute(&request));
    assert_eq!(body["version"], "1");
    assert_eq!(body["type"], "result");
    assert_eq!(body["activation_id"], "act-identity");
    assert_eq!(body["attempt_id"], "att-act-identity");
    assert_eq!(body["attempt_number"], 1);
    assert!(body.get("class").is_none());
    assert_eq!(body["result"]["statusCode"], 200);
    // The echo guest returns its parameters: the action's result is the body.
    assert_eq!(body["result"]["body"], request["params"]);
    assert_eq!(body["logs"], json!([]));
    assert_eq!(
        body["identity"],
        json!({
            "function_revision": "rev-act-identity",
            "runtime_mode": "wasm",
            "artifact_digest": echo,
            "wasmtime_version": "49.0.5",
            "language_profile": {"name": "profile-qualification", "version": "1"},
            "interface_version": "0.2.0"
        })
    );
    // A revision pinned to another engine is refused, never reported as run on this one.
    let mut other = Req::new(&echo, "act-identity-engine");
    other.engine = "48.0.0".into();
    expect_failure(
        &worker.execute(&other.json()),
        400,
        "invalid_request",
        "engine_version_mismatch",
    );
}

/// A mode other than wasm, or an image reference, gets the contract refusal first and creates
/// no instance. No artifact is published at all: nothing may need one.
pub fn runtime_mode() {
    let worker = Worker::start(Settings::default());
    let missing = digest_of(b"never published");
    let from = worker.events.len();
    // `oci`: the schema-valid refusal carries the attribution and the identity.
    let mut oci = Req::new(&missing, "act-mode-oci");
    oci.mode = "oci".into();
    oci.extra_projection = json!({"base": {"kind": "byo", "image_digest": missing,
        "signer_identity": "signer", "attestation_ref": "attestation"}});
    // The refusal comes first even when later checks would also refuse.
    oci.engine = "48.0.0".into();
    oci.budgets["memory_bytes"] = json!(1u64 << 40);
    let body = expect_failure(
        &worker.execute(&oci.json()),
        409,
        "runtime_mode_not_supported",
        "runtime_mode_not_wasm",
    );
    assert_eq!(body["class"], "non_retryable");
    assert_eq!(body["result"]["retryable"], false);
    assert_eq!(body["identity"]["runtime_mode"], "oci");
    assert_eq!(body["identity"]["base_digest"], missing);
    // A value the schema does not know is refused the same way, without attribution.
    let mut unknown = Req::new(&missing, "act-mode-unknown");
    unknown.mode = "container".into();
    expect_failure(
        &worker.execute(&unknown.json()),
        409,
        "runtime_mode_not_supported",
        "runtime_mode_not_wasm",
    );
    // An image reference instead of a Wasm artifact.
    let mut image = Req::new(&missing, "act-mode-image");
    image.mode = "oci".into();
    image.extra_projection = json!({"image": "registry.invalid/function@sha256:0000"});
    expect_failure(
        &worker.execute(&image.json()),
        409,
        "runtime_mode_not_supported",
        "runtime_mode_not_wasm",
    );
    // A wasm projection that smuggles an image is malformed: still nothing runs.
    let mut smuggled = Req::new(&missing, "act-mode-smuggled");
    smuggled.extra_projection = json!({"image": "registry.invalid/function@sha256:0000"});
    expect_failure(
        &worker.execute(&smuggled.json()),
        400,
        "invalid_request",
        "malformed_message",
    );
    no_instance(&worker, from);
    // Refused at parse: not even admitted to a slot.
    let admitted = worker
        .events
        .lock_slice(from)
        .iter()
        .any(|e| e["event"] == "execution_admitted");
    assert!(!admitted, "a refused revision was admitted");
}

/// The credential is absent from the guest's memory, from the environment and files (the guest
/// has no WASI imports, and one that asks for them is refused), and from the worker's logs.
pub fn credential() {
    let worker = Worker::start(Settings::default());
    let dump = worker.publish(MEMORY_DUMP);
    let log_echo = worker.publish(LOG_ECHO);
    let wasi = worker.publish(WASI_IMPORT);
    let secrets = CREDENTIAL_STRINGS;

    // Guest memory: everything the host wrote into it, returned as hex.
    let mut probe = Req::new(&dump, "act-credential-memory");
    probe.params = json!({"__ow_path": "/probe", "sentinel": "memory-sentinel-value"});
    let body = expect_success(&worker.execute(&probe.json()));
    let hex = body["result"]["body"].as_str().unwrap();
    let memory: Vec<u8> = (0..hex.len() / 2)
        .map(|at| u8::from_str_radix(&hex[2 * at..2 * at + 2], 16).unwrap())
        .collect();
    let contains = |needle: &str| memory.windows(needle.len()).any(|w| w == needle.as_bytes());
    // The control: the dump is real, and holds what the host legitimately provides.
    assert!(
        contains("memory-sentinel-value"),
        "the dump missed the parameters"
    );
    for secret in secrets {
        assert!(!contains(secret), "guest memory holds a credential string");
    }

    // Environment and files: a component that imports WASI is refused before it runs, even
    // when the revision declares the import.
    let from = worker.events.len();
    let reply = worker.execute(
        &Req::new(&wasi, "act-credential-wasi")
            .imports(&["wasi:cli/environment@0.2.0"])
            .json(),
    );
    expect_failure(&reply, 422, "undeclared_import", "import_not_allowed");
    no_instance(&worker, from);

    // Logs: a guest that echoes the parameters it was given into its log leaks nothing.
    let mut echoing = Req::new(&log_echo, "act-credential-log").imports(&[LOGGING]);
    echoing.params = json!({"__ow_path": "/leak", "text": secrets.join(" | ")});
    let body = expect_success(&worker.execute(&echoing.json()));
    let logs = body["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1);
    let line = logs[0].as_str().unwrap();
    assert!(line.contains("<redacted>"), "{line}");
    for secret in secrets {
        assert!(
            !line.contains(secret),
            "a response log holds a credential string"
        );
    }
    // The structured worker log: the same line is emitted redacted, and no event anywhere
    // carries a credential string.
    worker
        .events
        .wait(0, event_is("execution_finished", "act-credential-log"));
    let emitted = worker.events.count(|event| {
        event["event"] == "guest_log"
            && event["line"]
                .as_str()
                .is_some_and(|l| l.contains("<redacted>"))
    });
    assert_eq!(emitted, 1);
    let all = worker.events.lines();
    for secret in secrets {
        assert!(
            !all.contains(secret),
            "the worker log holds a credential string"
        );
    }
}

/// Declared-import enforcement, before compilation and without running guest code.
pub fn imports() {
    let worker = Worker::start(Settings::default());
    let context = worker.publish(CONTEXT_IMPORT);
    let wasi = worker.publish(WASI_IMPORT);
    let log_echo = worker.publish(LOG_ECHO);
    let from = worker.events.len();
    // An undeclared Falcone import with `imports: []`.
    let reply = worker.execute(&Req::new(&context, "act-import-empty").json());
    expect_failure(&reply, 422, "undeclared_import", "import_not_declared");
    // An undeclared import with a non-empty list.
    let reply = worker.execute(
        &Req::new(&context, "act-import-other")
            .imports(&[LOGGING])
            .json(),
    );
    expect_failure(&reply, 422, "undeclared_import", "import_not_declared");
    // A declared import that the worker does not allow.
    let reply = worker.execute(
        &Req::new(&wasi, "act-import-wasi")
            .imports(&["wasi:cli/environment@0.2.0"])
            .json(),
    );
    expect_failure(&reply, 422, "undeclared_import", "import_not_allowed");
    no_instance(&worker, from);
    // Every import declared and allowed: it runs.
    expect_success(
        &worker.execute(
            &Req::new(&context, "act-import-ok")
                .imports(&[CONTEXT])
                .json(),
        ),
    );
    expect_success(
        &worker.execute(
            &Req::new(&log_echo, "act-import-log")
                .imports(&[LOGGING])
                .json(),
        ),
    );
}

/// The three revision budgets are limits, not observations.
pub fn budgets() {
    let settings = Settings {
        max_memory_bytes: 134_217_728,
        ..Settings::default()
    };
    let worker = Worker::start(settings);
    let echo = worker.publish(ECHO);
    let spin = worker.publish(SPIN);
    let calls = worker.publish(HOST_CALLS_11);
    let log_then_spin = worker.publish(LOG_THEN_SPIN);

    // Memory: the 4 MiB fixture does not fit a 1 MiB budget, and fits a sufficient one.
    let small = Req::new(&echo, "act-budget-memory").budget("memory_bytes", 1_048_576);
    let body = expect_failure(
        &worker.execute(&small.json()),
        500,
        "execution_failed",
        "memory_budget_exceeded",
    );
    assert_eq!(body["result"]["retryable"], false);
    assert_eq!(body["class"], "non_retryable");
    assert!(body["result"].get("body").is_none(), "a partial result");
    let enough = Req::new(&echo, "act-budget-memory-ok").budget("memory_bytes", 16_777_216);
    expect_success(&worker.execute(&enough.json()));

    // A budget above the worker's maximum is refused before anything is compiled or
    // instantiated: this artifact was never published.
    let never = digest_of(b"never published");
    let from = worker.events.len();
    let above = Req::new(&never, "act-budget-ceiling").budget("memory_bytes", 134_217_729);
    let body = expect_failure(
        &worker.execute(&above.json()),
        400,
        "invalid_request",
        "budget_above_worker_ceiling",
    );
    assert_eq!(body["class"], "non_retryable");
    no_instance(&worker, from);
    // Exactly the maximum is honoured.
    expect_success(
        &worker.execute(
            &Req::new(&echo, "act-budget-max")
                .budget("memory_bytes", 134_217_728)
                .json(),
        ),
    );

    // Fuel: a loop ends by its fuel, well before the 30 s timeout.
    let started = Instant::now();
    let looping = Req::new(&spin, "act-budget-fuel").budget("fuel", 5_000);
    let body = expect_failure(
        &worker.execute(&looping.json()),
        500,
        "execution_failed",
        "fuel_exhausted",
    );
    assert_eq!(body["result"]["retryable"], false);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "fuel did not end the loop"
    );

    // Host calls: with N = 10, the 11th call is refused before it reaches the host. The
    // credential's issuer is "hello", the word the guest logs, so the logs show redaction.
    let from = worker.events.len();
    let mut limited = Req::new(&calls, "act-budget-calls")
        .imports(&[LOGGING])
        .budget("max_host_calls", 10);
    limited.issuer = "hello".into();
    let body = expect_failure(
        &worker.execute(&limited.json()),
        500,
        "execution_failed",
        "host_call_budget_exceeded",
    );
    assert_eq!(body["result"]["retryable"], false);
    assert_eq!(body["logs"], json!(vec!["[info] <redacted>"; 10]));
    let (_, _, finished) = worker
        .events
        .wait(from, event_is("execution_finished", "act-budget-calls"));
    assert_eq!(
        finished["host_calls"], 10,
        "the host saw more or fewer calls than the budget"
    );
    // Only guest calls count: the runtime's own epoch ticks, many of them during a 400 ms loop
    // that made a single host call, are not host calls, so a budget of 1 ends it by its
    // deadline, not by the host-call budget. (Compiled once first: the clock measures the guest.)
    let warm = Req::new(&log_then_spin, "act-budget-ticks-warm")
        .imports(&[LOGGING])
        .budget("fuel", 5_000);
    expect_failure(
        &worker.execute(&warm.json()),
        500,
        "execution_failed",
        "fuel_exhausted",
    );
    let idle = Req::new(&log_then_spin, "act-budget-ticks")
        .imports(&[LOGGING])
        .budget("max_host_calls", 1)
        .budget("timeout_ms", 400);
    expect_failure(
        &worker.execute(&idle.json()),
        504,
        "execution_timeout",
        "deadline_exceeded",
    );
    // With N = 11 the same guest completes and the host sees all 11.
    let from = worker.events.len();
    let all = Req::new(&calls, "act-budget-calls-ok")
        .imports(&[LOGGING])
        .budget("max_host_calls", 11);
    let body = expect_success(&worker.execute(&all.json()));
    assert_eq!(body["logs"].as_array().unwrap().len(), 11);
    let (_, _, finished) = worker
        .events
        .wait(from, event_is("execution_finished", "act-budget-calls-ok"));
    assert_eq!(finished["host_calls"], 11);
}

/// Timeout selection: the shortest of the request deadline, the projection limit and the
/// worker's wall clock wins, with the 504 envelope, and capacity recovers.
pub fn timeouts() {
    const LIMIT_MS: u64 = 1_500;
    // Cases: (request deadline in, projection timeout, worker wall clock).
    let cases = [
        ("request", LIMIT_MS, 30_000, 300_000),
        ("projection", 60_000, LIMIT_MS, 300_000),
        ("worker", 60_000, 30_000, LIMIT_MS),
    ];
    for (name, request_ms, projection_ms, worker_ms) in cases {
        let worker = Worker::start(Settings {
            concurrency: 1,
            max_wall_clock_ms: worker_ms,
            ..Settings::default()
        });
        let spin = worker.publish(SPIN);
        let echo = worker.publish(ECHO);
        // Compile once, so the clock below measures the guest.
        let warm = Req::new(&spin, &format!("act-timeout-{name}-warm")).budget("fuel", 5_000);
        expect_failure(
            &worker.execute(&warm.json()),
            500,
            "execution_failed",
            "fuel_exhausted",
        );
        let id = format!("act-timeout-{name}");
        let request = Req::new(&spin, &id)
            .deadline_in(request_ms)
            .budget("timeout_ms", projection_ms);
        let started = Instant::now();
        let reply = worker.execute(&request.json());
        let elapsed = started.elapsed();
        let body = expect_failure(&reply, 504, "execution_timeout", "deadline_exceeded");
        assert_eq!(body["activation_id"], id, "{name}");
        assert_eq!(body["attempt_id"], format!("att-{id}"), "{name}");
        assert_eq!(body["identity"]["wasmtime_version"], "49.0.5", "{name}");
        assert_eq!(body["identity"]["artifact_digest"], spin, "{name}");
        assert_eq!(body["result"]["retryable"], false, "{name}");
        assert!(
            elapsed >= Duration::from_millis(LIMIT_MS - 200),
            "{name}: ended early after {elapsed:?}"
        );
        assert!(
            elapsed <= Duration::from_millis(LIMIT_MS) + TOLERANCE,
            "{name}: the shortest limit did not win ({elapsed:?})"
        );
        // Capacity recovered: with concurrency 1 the next execute still runs.
        expect_success(
            &worker.execute(&Req::new(&echo, &format!("act-timeout-{name}-next")).json()),
        );
    }
}

/// With `concurrency` guests blocked and the execute listener full of stalled connections, a
/// cancel on the control listener takes effect; an extra execute gets 503 busy; each listener
/// refuses the other's operations.
pub fn saturation() {
    let worker = Worker::start(Settings {
        concurrency: 2,
        ..Settings::default()
    });
    let spin = worker.publish(SPIN);
    let echo = worker.publish(ECHO);
    // Compile once so that the stalled connections below are opened close to the checks.
    let warm = Req::new(&spin, "act-saturation-warm").budget("fuel", 5_000);
    expect_failure(
        &worker.execute(&warm.json()),
        500,
        "execution_failed",
        "fuel_exhausted",
    );

    // Connections stalled in the body: headers arrived, the body did not. They are kept alive
    // by a trickle so they last as long as the case needs (the absolute body deadline is 30 s).
    let stop = Arc::new(AtomicBool::new(false));
    let mut trickle = Vec::new();
    for _ in 0..4 {
        let mut stream = TcpStream::connect(worker.execute).unwrap();
        stream
            .write_all(b"POST /v1/execute HTTP/1.1\r\nHost: w\r\nContent-Length: 1000000\r\n\r\n{")
            .unwrap();
        let stop = stop.clone();
        trickle.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(500));
                if stream.write_all(b" ").is_err() {
                    break;
                }
            }
        }));
    }
    let first = worker.start_blocking(&spin, "act-saturation-1");
    let second = worker.start_blocking(&spin, "act-saturation-2");
    // The rest of the execute listener: stalled headers. Two running guests, four stalled
    // bodies and these four fill the 2 + 8 connections.
    let mut held = Vec::new();
    for _ in 0..ADMISSION_ALLOWANCE - 4 {
        let mut stream = TcpStream::connect(worker.execute).unwrap();
        stream
            .write_all(b"POST /v1/execute HTTP/1.1\r\nHost: w\r\n")
            .unwrap();
        held.push(stream);
    }
    // Every connection is held on the server (the guests report themselves; the stalled ones
    // are connected and accepted in order, so a request now meets a full listener).
    let reply = worker.execute(&Req::new(&echo, "act-saturation-extra").json());
    let body = expect_failure(&reply, 503, "busy", "at_capacity");
    assert_eq!(body["retryable"], true);

    // The cancel takes effect, on the control listener, while the execute listener is full.
    let cancelled = worker.cancel("act-saturation-1");
    assert_eq!(cancelled.status, 200);
    assert_eq!(cancelled.json()["cancel"]["state"], "acknowledged");
    let result = first.join().unwrap();
    expect_failure(&result, 500, "execution_failed", "cancelled");
    let cancelled = worker.cancel("act-saturation-2");
    assert_eq!(cancelled.json()["cancel"]["state"], "acknowledged");
    second.join().unwrap();

    // Each listener refuses the other's operations.
    let reply = post(worker.execute, "/v1/cancel", b"{}");
    expect_failure(&reply, 404, "not_found", "unknown_path");
    let reply = post(worker.control, "/v1/execute", b"{}");
    expect_failure(&reply, 404, "not_found", "unknown_path");
    let reply = exchange(
        worker.execute,
        b"GET /v1/execute HTTP/1.1\r\nHost: w\r\nContent-Length: 0\r\n\r\n",
    );
    expect_failure(&reply, 405, "method_not_allowed", "method_not_allowed");
    // An unknown execution is not found (control listener).
    expect_failure(
        &worker.cancel("act-saturation-gone"),
        404,
        "not_found",
        "unknown_execution",
    );

    stop.store(true, Ordering::Release);
    for handle in trickle {
        handle.join().unwrap();
    }
    drop(held);
}

/// The same attempt cannot run twice at once; a cancel names exactly one attempt.
pub fn duplicate_attempt() {
    let worker = Worker::start(Settings::default());
    let spin = worker.publish(SPIN);
    let running = worker.start_blocking(&spin, "act-duplicate");
    let again = worker.execute(&Req::new(&spin, "act-duplicate").json());
    expect_failure(&again, 400, "invalid_request", "duplicate_attempt");
    // A cancel for another attempt of the same activation touches nothing.
    let other = json!({"version": "1", "type": "cancel", "activation_id": "act-duplicate",
        "attempt_id": "att-other", "cancel": {"state": "requested"}});
    let reply = post(worker.control, "/v1/cancel", other.to_string().as_bytes());
    expect_failure(&reply, 404, "not_found", "unknown_execution");
    let cancelled = worker.cancel("act-duplicate");
    assert_eq!(cancelled.json()["cancel"]["state"], "acknowledged");
    expect_failure(
        &running.join().unwrap(),
        500,
        "execution_failed",
        "cancelled",
    );
}

/// Filling the control listener with stalled connections does not affect running executions,
/// and the control listener recovers after its deadlines.
pub fn control_stall() {
    let worker = Worker::start(Settings {
        concurrency: 2,
        ..Settings::default()
    });
    let spin = worker.publish(SPIN);
    let echo = worker.publish(ECHO);
    let warm = Req::new(&spin, "act-stall-warm").budget("fuel", 5_000);
    expect_failure(
        &worker.execute(&warm.json()),
        500,
        "execution_failed",
        "fuel_exhausted",
    );
    let running = worker.start_blocking(&spin, "act-stall-running");
    let from = worker.events.len();
    let mut held = Vec::new();
    for _ in 0..CONTROL_CONNECTIONS {
        let mut stream = TcpStream::connect(worker.control).unwrap();
        stream
            .write_all(b"POST /v1/cancel HTTP/1.1\r\nHost: w\r\n")
            .unwrap();
        held.push(stream);
    }
    // The control listener is full: an extra control request is answered busy at once.
    let body = expect_failure(
        &worker.cancel("act-stall-running"),
        503,
        "busy",
        "at_capacity",
    );
    assert_eq!(body["retryable"], true);
    // Executions are unaffected: a normal execute runs while the control listener is stalled.
    expect_success(&worker.execute(&Req::new(&echo, "act-stall-echo").json()));
    assert_eq!(
        worker.events.count(
            |e| e["event"] == "execution_finished" && e["activation_id"] == "act-stall-running"
        ),
        0,
        "the stall disturbed a running execution"
    );
    // The stalled connections end at their header deadline; the listener recovers.
    let mut next = from;
    for _ in 0..CONTROL_CONNECTIONS {
        let (at, _, _) = worker.events.wait(next, |e| {
            e["event"] == "connection_closed"
                && e["listener"] == "control"
                && e["reason"] == "header_deadline"
        });
        next = at + 1;
    }
    let cancelled = worker.cancel("act-stall-running");
    assert_eq!(cancelled.status, 200);
    assert_eq!(cancelled.json()["cancel"]["state"], "acknowledged");
    running.join().unwrap();
    drop(held);
}

/// Oversized headers, request and action body are refused without running guest code; slow
/// connections are closed within their bounds; afterwards a normal execute and a cancel work.
pub fn http_limits() {
    let worker = Worker::start(Settings {
        concurrency: 2,
        ..Settings::default()
    });
    let echo = worker.publish(ECHO);
    let spin = worker.publish(SPIN);

    // Oversized headers: refused with 413 before the body, or anything else, is read.
    let mut head =
        b"POST /v1/execute HTTP/1.1\r\nHost: w\r\nContent-Length: 2\r\nX-Padding: ".to_vec();
    head.extend(std::iter::repeat_n(b'a', 20 * 1024));
    head.extend_from_slice(b"\r\n\r\n{}");
    let mark = worker.events.len();
    let reply = exchange(worker.execute, &head);
    expect_failure(&reply, 413, "request_too_large", "headers_too_large");
    no_instance(&worker, mark);
    worker.recovers(&spin, &echo, "limits-headers");

    // A declared body above the 8 MiB request limit is refused without being read.
    let mark = worker.events.len();
    let reply = exchange(
        worker.execute,
        b"POST /v1/execute HTTP/1.1\r\nHost: w\r\nContent-Length: 8388609\r\n\r\n",
    );
    expect_failure(&reply, 413, "request_too_large", "request_body_too_large");
    // The control listener has a far smaller body limit.
    let reply = exchange(
        worker.control,
        b"POST /v1/cancel HTTP/1.1\r\nHost: w\r\nContent-Length: 16385\r\n\r\n",
    );
    expect_failure(&reply, 413, "request_too_large", "request_body_too_large");
    no_instance(&worker, mark);
    worker.recovers(&spin, &echo, "limits-request");

    // An oversized action body: a whole valid request whose `__ow_body` is one byte over 1 MiB.
    let from = worker.events.len();
    let mut big = Req::new(&echo, "act-limits-action");
    big.params = json!({"__ow_path": "/big", "__ow_body": "x".repeat(1_048_577)});
    let reply = worker.execute(&big.json());
    expect_failure(&reply, 413, "request_too_large", "body_too_large");
    no_instance(&worker, from);
    worker.recovers(&spin, &echo, "limits-action");

    // Slow connections, all at once: no bytes, dripped headers, a stalled body.
    let from = worker.events.len();
    let silent = TcpStream::connect(worker.execute).unwrap();
    let mut drip = TcpStream::connect(worker.execute).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let dripper = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut sent = 0;
            let request = b"POST /v1/execute HTTP/1.1\r\nHost: w\r\nX-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            while !stop.load(Ordering::Acquire) && sent < request.len() {
                if drip.write_all(&request[sent..=sent]).is_err() {
                    break;
                }
                sent += 1;
                std::thread::sleep(Duration::from_millis(700));
            }
            drip
        })
    };
    let mut stalled = TcpStream::connect(worker.execute).unwrap();
    stalled
        .write_all(b"POST /v1/execute HTTP/1.1\r\nHost: w\r\nContent-Length: 100000\r\n\r\n{\"")
        .unwrap();
    let opened = Instant::now();
    let closed = |reason: &'static str| {
        move |event: &Value| {
            event["event"] == "connection_closed"
                && event["listener"] == "execute"
                && event["reason"] == reason
        }
    };
    let (first, header_one, _) = worker.events.wait(from, closed("header_deadline"));
    let (_, header_two, _) = worker.events.wait(first + 1, closed("header_deadline"));
    let (_, body_inactive, _) = worker.events.wait(from, closed("body_inactivity"));
    stop.store(true, Ordering::Release);
    let _drip = dripper.join().unwrap();
    // Each ended within its documented bound (plus scheduling slack).
    assert!(header_one.duration_since(opened) <= HEADER_DEADLINE + TOLERANCE);
    assert!(header_two.duration_since(opened) <= HEADER_DEADLINE + TOLERANCE);
    assert!(body_inactive.duration_since(opened) <= BODY_INACTIVITY + TOLERANCE);
    // The server closed them: reading ends at once.
    for mut stream in [silent, stalled] {
        stream.set_read_timeout(Some(TOLERANCE)).unwrap();
        let mut sink = [0u8; 16];
        assert!(
            !matches!(stream.read(&mut sink), Ok(n) if n > 0),
            "a closed connection answered"
        );
    }
    worker.recovers(&spin, &echo, "limits-slow");
}

/// A reader that takes one byte at a time is cut off at the absolute response deadline, its
/// slot is released, and with `concurrency` such readers a normal execute and a cancel still work.
pub fn slow_reader() {
    let worker = Worker::start(Settings {
        concurrency: 2,
        ..Settings::default()
    });
    let echo = worker.publish(ECHO);
    let spin = worker.publish(SPIN);
    let logs = worker.publish(MAX_LOGS);
    let from = worker.events.len();
    let mut readers = Vec::new();
    for n in 0..2 {
        let id = format!("act-slow-{n}");
        let body = Req::new(&logs, &id)
            .imports(&[LOGGING])
            .budget("max_host_calls", 2_000)
            .json()
            .to_string();
        let addr = worker.execute;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        readers.push((
            stop,
            std::thread::spawn(move || {
                let mut stream = TcpStream::connect(addr).unwrap();
                let request = format!(
                    "POST /v1/execute HTTP/1.1\r\nHost: w\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(request.as_bytes()).unwrap();
                let mut taken = 0usize;
                let mut byte = [0u8; 1];
                // One byte at a time, far slower than the deadline allows for a ~8 MB response.
                while !flag.load(Ordering::Acquire) {
                    match stream.read(&mut byte) {
                        Ok(1) => taken += 1,
                        _ => break,
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                taken
            }),
        ));
    }
    // Both finish executing (server-side events); then each response is cut off by the
    // absolute response deadline, however slowly it is being read.
    let finished: Vec<Instant> = (0..2)
        .map(|n| {
            let id = format!("act-slow-{n}");
            worker
                .events
                .wait(from, event_is("execution_finished", &id))
                .1
        })
        .collect();
    let (first, last) = (
        *finished.iter().min().unwrap(),
        *finished.iter().max().unwrap(),
    );
    let mut next = from;
    for _ in 0..2 {
        let (at, cut, _) = worker
            .events
            .wait(next, |e| e["event"] == "response_deadline_exceeded");
        next = at + 1;
        // Neither cut early (the deadline was waited out) nor late.
        assert!(cut >= first + RESPONSE_DEADLINE - Duration::from_secs(1));
        assert!(
            cut <= last + RESPONSE_DEADLINE + TOLERANCE,
            "the response outlived its absolute deadline"
        );
    }
    for (stop, reader) in readers {
        stop.store(true, Ordering::Release);
        // The reader got a prefix, never the whole response.
        let taken = reader.join().unwrap();
        assert!(
            taken < 8_000_000,
            "a slow reader received the whole response"
        );
    }
    assert_eq!(
        worker
            .events
            .count(|e| e["event"] == "response_deadline_exceeded"),
        2
    );
    worker.recovers(&spin, &echo, "slow");
}

// ---------------------------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------------------------

type Case = (&'static str, fn());

pub const CASES: &[Case] = &[
    ("identity", identity),
    ("runtime-mode", runtime_mode),
    ("credential", credential),
    ("imports", imports),
    ("budgets", budgets),
    ("timeouts", timeouts),
    ("duplicate-attempt", duplicate_attempt),
    ("saturation", saturation),
    ("control-stall", control_stall),
    ("http-limits", http_limits),
    ("slow-reader", slow_reader),
];

/// Runs one case, or `all`, in this process (the restricted pod) and reports each as a JSON
/// line, then the success record the cluster harness looks for.
pub fn run(name: &str) -> ExitCode {
    let selected: Vec<&Case> = if name == "all" {
        CASES.iter().collect()
    } else {
        CASES
            .iter()
            .filter(|(candidate, _)| *candidate == name)
            .collect()
    };
    if selected.is_empty() {
        eprintln!("unknown_execution_api_case");
        return ExitCode::FAILURE;
    }
    for (case_name, case) in selected {
        let started = Instant::now();
        let outcome = std::panic::catch_unwind(case);
        let record = json!({"check": "execution_api", "case": case_name,
            "status": if outcome.is_ok() { "passed" } else { "failed" },
            "elapsed_ms": started.elapsed().as_millis() as u64});
        println!("{record}");
        if outcome.is_err() {
            return ExitCode::FAILURE;
        }
    }
    println!("{}", json!({"check": "execution_api", "status": "passed"}));
    ExitCode::SUCCESS
}
