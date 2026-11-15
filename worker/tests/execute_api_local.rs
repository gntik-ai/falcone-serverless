//! Local tests of the worker's HTTP execution API: a real server on loopback sockets, guests
//! compiled from WAT, and a test platform at the system boundary. Tests synchronize on
//! server-side events (guest start, setup, cache stages, slot release) and never on fixed
//! sleeps. Each test starts its own server, so capacity and state are never shared.
#![cfg(target_os = "linux")]

use falcone_serverless_worker::api::Ceilings;
use falcone_serverless_worker::host::{Bindings, Context, HostError, Operation, Reply};
use falcone_serverless_worker::server::{
    Deadlines, Platform, Secret, Server, ServerConfig, Unavailable,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WORKER_ENGINE: &str = "49.0.5";
/// The documented termination budget.
const BUDGET: Duration = Duration::from_millis(500);
/// Generous waits for things that must happen; they bound a hang, never a measurement.
const LONG: Duration = Duration::from_secs(60);

// --- guests -------------------------------------------------------------------------------

fn wat_bytes(text: &str) -> String {
    text.bytes().map(|b| format!("\\{b:02x}")).collect()
}

/// A handler component. `body` is the WAT body of `handle(ptr, len) -> i32`; the result area
/// is at 0. `data` are (offset, text) segments; `pages` is the initial memory in 64 KiB pages.
fn guest(imports: &[&str], pages: u32, data: &[(u32, &str)], body: &str) -> Vec<u8> {
    let error = r#"(type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
      (export "host-error" (type $error (eq $error-type)))"#;
    let mut wat = String::from("(component\n");
    let (mut aliases, mut core_imports, mut core_args) =
        (String::new(), String::new(), String::new());
    for import in imports {
        match *import {
            "logging" => {
                wat += &format!(
                    r#"(import "falcone:host/logging@0.2.0" (instance $logging
                      {error}
                      (type $level-type (enum "debug" "info" "warn" "error"))
                      (export "level" (type $level (eq $level-type)))
                      (type $field-type (record (field "name" string) (field "value" string)))
                      (export "field" (type $field (eq $field-type)))
                      (export "log" (func (param "severity" $level) (param "message" string) (param "fields" (list $field)) (result (result (error $error)))))))"#
                );
                aliases += r#"(alias export $logging "log" (func $log))
                  (core func $log (canon lower (func $log) (memory $memory)))"#;
                core_imports +=
                    r#"(import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))"#;
                core_args += r#"(export "log" (func $log))"#;
            }
            "progress" => {
                wat += &format!(
                    r#"(import "falcone:host/progress@0.2.0" (instance $progress
                      {error}
                      (export "report" (func (param "completed" u64) (param "total" u64) (param "message" string) (result (result (error $error)))))))"#
                );
                aliases += r#"(alias export $progress "report" (func $report))
                  (core func $report (canon lower (func $report) (memory $memory)))"#;
                core_imports +=
                    r#"(import "host" "report" (func $report (param i64 i64 i32 i32 i32)))"#;
                core_args += r#"(export "report" (func $report))"#;
            }
            "context" => {
                wat += r#"(import "falcone:host/invocation-context@0.2.0" (instance $context
                      (export "invocation-key" (func (result string)))))"#;
                aliases += r#"(alias export $context "invocation-key" (func $key))
                  (core func $key (canon lower (func $key) (memory $memory) (realloc (func $realloc))))"#;
                core_imports += r#"(import "host" "key" (func $key (param i32)))"#;
                core_args += r#"(export "key" (func $key))"#;
            }
            "other" => wat += r#"(import "falcone:host/other@0.2.0" (instance))"#,
            other => panic!("unknown import {other}"),
        }
    }
    let segments: String = data
        .iter()
        .map(|(at, text)| format!("(data (i32.const {at}) \"{}\")", wat_bytes(text)))
        .collect();
    wat += &format!(
        r#"
      (type $bytes (list u8))
      (type $herror (variant (case "invalid-input" string) (case "failed" string)))
      (type $result (result $bytes (error $herror)))
      (type $handle (func (param "params" $bytes) (result $result)))
      (core module $libc
        (memory (export "memory") {pages})
        (global $heap (mut i32) (i32.const 65536))
        (func (export "realloc") (param $old i32) (param $oldsize i32) (param $align i32) (param $size i32) (result i32)
          (local $ptr i32)
          (local.set $ptr (i32.and (i32.add (global.get $heap) (i32.sub (local.get $align) (i32.const 1)))
                                   (i32.sub (i32.const 0) (local.get $align))))
          (global.set $heap (i32.add (local.get $ptr) (local.get $size)))
          (local.get $ptr)))
      (core instance $libc (instantiate $libc))
      (alias core export $libc "memory" (core memory $memory))
      (alias core export $libc "realloc" (core func $realloc))
      {aliases}
      (core module $guest
        (import "host" "memory" (memory {pages}))
        {core_imports}
        {segments}
        (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
          {body}))
      (core instance $host (export "memory" (memory $memory)) {core_args})
      (core instance $guest (instantiate $guest (with "host" (instance $host))))
      (func $handle (type $handle) (canon lift (core func $guest "handle")
        (memory $memory) (realloc (func $realloc)) string-encoding=utf8))
      (instance $handler (export "handler-error" (type $herror)) (export "handle" (func $handle)))
      (export "falcone:host/handler@0.2.0" (instance $handler)))"#
    );
    wat::parse_str(&wat).unwrap_or_else(|error| panic!("{error}\n{wat}"))
}

const OK_JSON: &str = r#"{"statusCode":200,"body":{"ok":true}}"#;

/// Returns the static bytes at 1024 (the result area is at 0).
fn ret(len: usize) -> String {
    format!(
        "(i32.store (i32.const 4) (i32.const 1024)) (i32.store (i32.const 8) (i32.const {len})) (i32.const 0)"
    )
}

/// A guest that returns the legacy result and uses 4 MiB of memory.
fn success() -> Vec<u8> {
    guest(&[], 64, &[(1024, OK_JSON)], &ret(OK_JSON.len()))
}

/// A guest that never returns.
fn spin() -> Vec<u8> {
    guest(&[], 64, &[], &format!("(loop $l (br $l)) {}", ret(0)))
}

/// A guest that logs `message` `times` times, then returns the legacy result.
fn logging(message: &str, times: u32) -> Vec<u8> {
    guest(
        &["logging"],
        64,
        &[(1024, OK_JSON), (2048, message)],
        &format!(
            "(local $i i32)
             (loop $again
               (call $log (i32.const 1) (i32.const 2048) (i32.const {}) (i32.const 0) (i32.const 0) (i32.const 3000))
               (local.set $i (i32.add (local.get $i) (i32.const 1)))
               (br_if $again (i32.lt_u (local.get $i) (i32.const {times}))))
             {}",
            message.len(),
            ret(OK_JSON.len())
        ),
    )
}

/// A guest that makes `calls` progress reports, which reach the platform's bindings.
fn reporting(calls: u32) -> Vec<u8> {
    guest(
        &["progress"],
        64,
        &[(1024, OK_JSON)],
        &format!(
            "(local $i i32)
             (loop $again
               (call $report (i64.const 1) (i64.const 2) (i32.const 2048) (i32.const 0) (i32.const 3000))
               (local.set $i (i32.add (local.get $i) (i32.const 1)))
               (br_if $again (i32.lt_u (local.get $i) (i32.const {calls}))))
             {}",
            ret(OK_JSON.len())
        ),
    )
}

/// A guest that calls the host's invocation context for ever.
fn calling() -> Vec<u8> {
    guest(
        &["context"],
        64,
        &[],
        &format!("(loop $l (call $key (i32.const 3000)) (br $l)) {}", ret(0)),
    )
}

// --- platform -----------------------------------------------------------------------------

/// The system boundary: artifacts by digest, and bindings that count the calls they see.
#[derive(Default)]
struct World {
    artifacts: Mutex<HashMap<String, Vec<u8>>>,
    /// Operations that reached the platform's bindings.
    host_calls: Arc<AtomicUsize>,
    binds: AtomicUsize,
    contexts: Mutex<Vec<Context>>,
    /// The last credential bound, as the platform's binding layer saw it.
    credential: Mutex<Option<String>>,
}

struct Counting(Arc<AtomicUsize>);

impl Bindings for Counting {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        self.0.fetch_add(1, Ordering::AcqRel);
        match operation {
            Operation::Progress { .. } => Ok(Reply::Empty),
            _ => Err(HostError::Denied),
        }
    }
}

impl Platform for World {
    fn artifact(&self, digest: &str) -> Result<Vec<u8>, Unavailable> {
        self.artifacts
            .lock()
            .unwrap()
            .get(digest)
            .cloned()
            .ok_or(Unavailable::NotFound)
    }

    fn bind(
        &self,
        context: &Context,
        credential: Secret,
    ) -> Result<Arc<dyn Bindings>, Unavailable> {
        self.binds.fetch_add(1, Ordering::AcqRel);
        self.contexts.lock().unwrap().push(context.clone());
        *self.credential.lock().unwrap() = Some(credential.expose().to_owned());
        Ok(Arc::new(Counting(self.host_calls.clone())))
    }
}

impl World {
    fn add(&self, bytes: &[u8]) -> String {
        let digest = format!("sha256:{:x}", Sha256::digest(bytes));
        self.artifacts
            .lock()
            .unwrap()
            .insert(digest.clone(), bytes.to_vec());
        digest
    }
}

// --- requests -----------------------------------------------------------------------------

fn civil(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn deadline_in(seconds: u64) -> String {
    civil(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + seconds,
    )
}

static NEXT: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
struct Spec {
    digest: String,
    activation: String,
    imports: Vec<&'static str>,
    runtime_mode: &'static str,
    timeout_ms: u64,
    /// Seconds until the request's own absolute deadline.
    deadline_secs: u64,
    memory_bytes: u64,
    fuel: u64,
    max_host_calls: u64,
    engine: &'static str,
    secrets: Vec<String>,
    base: bool,
}

impl Spec {
    fn new(digest: &str) -> Self {
        let id = NEXT.fetch_add(1, Ordering::AcqRel);
        Self {
            digest: digest.to_owned(),
            activation: format!("act-local-{id:06}"),
            imports: Vec::new(),
            runtime_mode: "wasm",
            timeout_ms: 30_000,
            deadline_secs: 120,
            memory_bytes: 64 * 1024 * 1024,
            fuel: 1 << 50,
            max_host_calls: 1000,
            engine: WORKER_ENGINE,
            secrets: Vec::new(),
            base: false,
        }
    }

    fn with(mut self, change: impl FnOnce(&mut Self)) -> Self {
        change(&mut self);
        self
    }

    fn request(&self) -> Value {
        let mut hosts = vec![json!("api.example.test")];
        hosts.extend(self.secrets.iter().map(|secret| json!(secret)));
        let mut projection = json!({
            "revision": "rev-local-0001",
            "runtime_mode": self.runtime_mode,
            "artifact_digest": self.digest,
            "component_digest": self.digest,
            "language_profile": {"name": "profile-local", "version": "1"},
            "engine_version": self.engine,
            "wasi_version": "0.2.6",
            "interface_version": "0.2.0",
            "imports": self.imports.iter().map(|i| {
                format!("falcone:host/{}@0.2.0", if *i == "context" { "invocation-context" } else { i })
            }).collect::<Vec<_>>(),
            "budgets": {
                "fuel": self.fuel,
                "memory_bytes": self.memory_bytes,
                "timeout_ms": self.timeout_ms,
                "max_host_calls": self.max_host_calls
            },
            "grants": [{"family": "http", "interface": "falcone:host/http@0.2.0", "scope": {"hosts": hosts}}]
        });
        if self.base {
            projection["base"] = json!({
                "kind": "catalog", "id": "base-local", "version": "1.0.0",
                "digest": format!("sha256:{}", "2".repeat(64))
            });
        }
        json!({
            "version": "1",
            "type": "execute",
            "activation_id": self.activation,
            "attempt_id": "att-local-0001",
            "attempt_number": 1,
            "deadline": deadline_in(self.deadline_secs),
            "invocation": {"invocation_key": "inv-local-0001"},
            "credential": {
                "iss": "issuer-credential-canary", "aud": "audience-credential-canary",
                "tenant": "tenant-credential-canary", "workspace": "workspace-credential-canary",
                "pool_profile": "pool-credential-canary", "revision": "rev-local-0001",
                "activation": self.activation, "attempt": "att-local-0001", "iat": 1, "exp": 2,
                "allowed_capabilities": [{
                    "family": "http", "interface": "falcone:host/http@0.1.0",
                    "scope": {"hosts": hosts}
                }]
            },
            "projection": projection,
            "params": {"__ow_path": "/v1/local"}
        })
    }
}

// --- http client --------------------------------------------------------------------------

struct Reply2 {
    status: u16,
    body: Vec<u8>,
}

impl Reply2 {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&self.body)))
    }
}

fn read_reply(mut stream: TcpStream) -> Option<Reply2> {
    stream.set_read_timeout(Some(LONG)).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head.split(' ').nth(1)?.parse().ok()?;
    Some(Reply2 {
        status,
        body: raw[split + 4..].to_vec(),
    })
}

fn connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).unwrap();
    stream.set_nodelay(true).unwrap();
    stream
}

/// One request in one write, then the whole response.
fn post(addr: SocketAddr, path: &str, body: &[u8]) -> Reply2 {
    let mut stream = connect(addr);
    let mut message = format!(
        "POST {path} HTTP/1.1\r\nHost: worker\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    message.extend_from_slice(body);
    stream.write_all(&message).unwrap();
    read_reply(stream).expect("a response")
}

// --- harness ------------------------------------------------------------------------------

struct Harness {
    server: Server,
    world: Arc<World>,
    started: Mutex<mpsc::Receiver<()>>,
    _dir: TempDir,
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let id = NEXT.fetch_add(1, Ordering::AcqRel);
        let path =
            std::env::temp_dir().join(format!("execute-api-{label}-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(dir: &TempDir, concurrency: usize) -> ServerConfig {
    let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
    ServerConfig::new(
        loopback,
        loopback,
        concurrency,
        Ceilings {
            max_memory_bytes: 512 * 1024 * 1024,
            max_wall_clock_ms: 60_000,
        },
        dir.0.join("cache"),
        compiler(),
    )
}

/// The trusted compile child. In the restricted pod the suite runs beside the image's own.
fn compiler() -> std::path::PathBuf {
    std::env::var_os("EXECUTE_API_COMPILER")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_artifact-compile-child").into())
}

impl Harness {
    fn new(concurrency: usize) -> Self {
        Self::with(concurrency, |_| {})
    }

    fn with(concurrency: usize, change: impl FnOnce(&mut ServerConfig)) -> Self {
        let dir = TempDir::new("local");
        let mut config = config(&dir, concurrency);
        change(&mut config);
        let world = Arc::new(World::default());
        let server = Server::start(config, world.clone()).unwrap();
        let (send, receive) = mpsc::channel();
        let send = Mutex::new(send);
        server.hooks().on_guest_start(move || {
            let _ = send.lock().unwrap().send(());
        });
        Self {
            server,
            world,
            started: Mutex::new(receive),
            _dir: dir,
        }
    }

    fn cache_dir(&self) -> std::path::PathBuf {
        self._dir.0.join("cache")
    }

    fn execute_addr(&self) -> SocketAddr {
        self.server.execute_addr()
    }

    fn control_addr(&self) -> SocketAddr {
        self.server.control_addr()
    }

    /// Waits for `count` guests to reach their start; the server's own event.
    fn wait_started(&self, count: usize) {
        let receive = self.started.lock().unwrap();
        for _ in 0..count {
            receive.recv_timeout(LONG).expect("a guest started");
        }
    }

    fn run(&self, spec: &Spec) -> Reply2 {
        post(
            self.execute_addr(),
            "/v1/execute",
            spec.request().to_string().as_bytes(),
        )
    }

    fn spawn(self: &Arc<Self>, spec: &Spec) -> std::thread::JoinHandle<Reply2> {
        let harness = self.clone();
        let body = spec.request().to_string();
        std::thread::spawn(move || post(harness.execute_addr(), "/v1/execute", body.as_bytes()))
    }

    fn cancel(&self, activation: &str) -> Reply2 {
        let body = json!({
            "version": "1", "type": "cancel", "activation_id": activation,
            "attempt_id": "att-local-0001", "cancel": {"state": "requested"}
        });
        post(
            self.control_addr(),
            "/v1/cancel",
            body.to_string().as_bytes(),
        )
    }

    /// Runs the artifact once to completion of its compile so later timings measure the guest.
    /// The guest is cancelled as soon as it starts.
    fn warm(self: &Arc<Self>, spec: &Spec) {
        let handle = self.spawn(spec);
        {
            let receive = self.started.lock().unwrap();
            // The request ends first only when it failed before the guest started.
            while receive.recv_timeout(Duration::from_millis(20)).is_err() {
                if handle.is_finished() {
                    let reply = handle.join().unwrap();
                    panic!(
                        "no guest started: {} {}",
                        reply.status,
                        String::from_utf8_lossy(&reply.body)
                    );
                }
            }
        }
        assert_eq!(self.cancel(&spec.activation).status, 200);
        assert_eq!(handle.join().unwrap().status, 499);
    }
}

fn code(reply: &Reply2) -> (u16, String, String) {
    let value = reply.json();
    let failure = if value["type"] == "result" {
        &value["result"]
    } else {
        &value
    };
    (
        reply.status,
        failure["code"].as_str().unwrap_or("").to_owned(),
        failure["detail"]["reason"]
            .as_str()
            .unwrap_or("")
            .to_owned(),
    )
}

// --- tests: the contract ------------------------------------------------------------------

#[test]
fn a_wasm_execute_returns_the_legacy_envelope_with_the_runtime_identity() {
    let harness = Harness::new(2);
    let digest = harness.world.add(&success());
    for with_base in [false, true] {
        let spec = Spec::new(&digest).with(|s| s.base = with_base);
        let reply = harness.run(&spec);
        assert_eq!(
            reply.status,
            200,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        let result = reply.json();
        assert_eq!(result["type"], "result");
        assert_eq!(result["status"], "success");
        assert_eq!(
            result["result"],
            json!({"statusCode": 200, "body": {"ok": true}})
        );
        assert_eq!(result["activation_id"], spec.activation);
        let identity = &result["identity"];
        assert_eq!(identity["function_revision"], "rev-local-0001");
        assert_eq!(identity["runtime_mode"], "wasm");
        assert_eq!(identity["artifact_digest"], digest);
        assert_eq!(identity["wasmtime_version"], WORKER_ENGINE);
        assert_eq!(identity["interface_version"], "0.2.0");
        assert_eq!(identity["base_digest"].is_string(), with_base);
    }
    // The first run compiled; the second is served from the cache, and both ran the guest.
    assert_eq!(harness.world.binds.load(Ordering::Acquire), 2);
    // Each artifact is validated once per execution, never twice.
    assert_eq!(
        harness
            .server
            .cache_hooks()
            .validations
            .load(Ordering::Acquire),
        2
    );
}

// --- tests: refusal and admission order ---------------------------------------------------

/// Sends only a request line and headers promising `length` body bytes, then reads the answer.
fn post_headers_only(addr: SocketAddr, path: &str, length: usize) -> Reply2 {
    let mut stream = connect(addr);
    let head = format!("POST {path} HTTP/1.1\r\nHost: worker\r\nContent-Length: {length}\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    read_reply(stream).expect("a response")
}

#[test]
fn a_non_wasm_request_is_refused_409_and_creates_no_instance_even_when_every_slot_is_busy() {
    let harness = Arc::new(Harness::new(1));
    let digest = harness.world.add(&spin());
    let oci = Spec::new(&digest).with(|s| s.runtime_mode = "oci");
    // With a free slot: refused first, with the result envelope of the pinned projection.
    let reply = harness.run(&oci);
    assert_eq!(
        code(&reply),
        (
            409,
            "runtime_mode_not_supported".into(),
            "runtime_mode_not_wasm".into()
        )
    );
    assert_eq!(reply.json()["identity"]["runtime_mode"], "oci");
    // An image reference (an OCI revision pinning a base) is refused the same way.
    let image = oci.clone().with(|s| s.base = true);
    assert_eq!(harness.run(&image).status, 409);
    assert_eq!(harness.world.binds.load(Ordering::Acquire), 0);
    // Occupy the only slot, then repeat with the body fully buffered.
    let running = Spec::new(&digest);
    let handle = harness.spawn(&running);
    harness.wait_started(1);
    assert_eq!(harness.server.running(), 1);
    let reply = harness.run(&oci);
    assert_eq!(
        code(&reply),
        (
            409,
            "runtime_mode_not_supported".into(),
            "runtime_mode_not_wasm".into()
        )
    );
    // A wasm request over capacity is busy, and so is one whose body never arrives: its body
    // is not waited for.
    let busy = harness.run(&Spec::new(&digest));
    assert_eq!(code(&busy), (503, "busy".into(), "at_capacity".into()));
    assert_eq!(busy.json()["retryable"], true);
    let started = Instant::now();
    let reply = post_headers_only(harness.execute_addr(), "/v1/execute", 4096);
    assert_eq!(code(&reply), (503, "busy".into(), "at_capacity".into()));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // No instance was created for any of them.
    assert_eq!(
        harness
            .server
            .hooks()
            .guests_started
            .load(Ordering::Acquire),
        1
    );
    assert_eq!(harness.world.binds.load(Ordering::Acquire), 1);
    assert_eq!(harness.cancel(&running.activation).status, 200);
    assert_eq!(handle.join().unwrap().status, 499);
}

#[test]
fn a_malformed_message_and_a_wrong_engine_are_refused_before_anything_runs() {
    let harness = Harness::new(1);
    let digest = harness.world.add(&success());
    let reply = post(
        harness.execute_addr(),
        "/v1/execute",
        b"{\"version\":\"1\"}",
    );
    assert_eq!(
        code(&reply),
        (400, "invalid_request".into(), "malformed_message".into())
    );
    let engine = Spec::new(&digest).with(|s| s.engine = "1.0.0");
    assert_eq!(
        code(&harness.run(&engine)),
        (
            400,
            "invalid_request".into(),
            "engine_version_mismatch".into()
        )
    );
    let mut request = Spec::new(&digest).request();
    request["params"]["__ow_body"] = json!("x".repeat(1_048_577));
    let reply = post(
        harness.execute_addr(),
        "/v1/execute",
        request.to_string().as_bytes(),
    );
    assert_eq!(
        code(&reply),
        (413, "request_too_large".into(), "body_too_large".into())
    );
    assert_eq!(harness.world.binds.load(Ordering::Acquire), 0);
    assert_eq!(
        harness
            .server
            .hooks()
            .guests_started
            .load(Ordering::Acquire),
        0
    );
}

// --- tests: declared imports --------------------------------------------------------------

#[test]
fn an_import_the_projection_does_not_declare_or_the_worker_does_not_allow_is_422_before_any_guest_runs()
 {
    let harness = Harness::new(1);
    let both = harness.world.add(&guest(
        &["logging", "progress"],
        64,
        &[(1024, OK_JSON)],
        &ret(OK_JSON.len()),
    ));
    let none = harness.world.add(&guest(
        &["logging"],
        64,
        &[(1024, OK_JSON)],
        &ret(OK_JSON.len()),
    ));
    let other = harness.world.add(&guest(
        &["other"],
        64,
        &[(1024, OK_JSON)],
        &ret(OK_JSON.len()),
    ));
    let undeclared = (
        422,
        "undeclared_import".to_owned(),
        "import_not_declared".to_owned(),
    );
    // An undeclared Falcone interface with `imports: []`.
    assert_eq!(code(&harness.run(&Spec::new(&none))), undeclared);
    // An undeclared interface beside a non-empty list.
    let partial = Spec::new(&both).with(|s| s.imports = vec!["logging"]);
    assert_eq!(code(&harness.run(&partial)), undeclared);
    // A declared interface that is not in ALLOWED_IMPORTS.
    let declared_other = Spec::new(&other).with(|s| s.imports = vec!["other"]);
    assert_eq!(code(&harness.run(&declared_other)), undeclared);
    assert_eq!(
        harness
            .server
            .hooks()
            .guests_started
            .load(Ordering::Acquire),
        0
    );
    assert_eq!(harness.world.binds.load(Ordering::Acquire), 0);
    // Every import declared.
    let all = Spec::new(&both).with(|s| s.imports = vec!["logging", "progress"]);
    assert_eq!(harness.run(&all).status, 200);
    let declared = Spec::new(&none).with(|s| s.imports = vec!["logging"]);
    assert_eq!(harness.run(&declared).status, 200);
}

// --- tests: the credential and logs -------------------------------------------------------

/// Whether `line` contains eight consecutive characters of `secret`, or all of it when shorter.
fn leaks(line: &str, secret: &str) -> bool {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 8 {
        return line.contains(secret);
    }
    chars
        .windows(8)
        .any(|window| line.contains(&window.iter().collect::<String>()))
}

#[test]
fn the_credential_never_reaches_guest_visible_output_or_logs_including_across_the_cut() {
    let harness = Harness::new(2);
    let long = "L0ngS3cr3t".repeat(900);
    let crossing = "cr0ss1ng-s3cret-v4lue";
    let at_cut = "3nds-at-th3-cut!!";
    let short = "sh0rt1";
    let secrets = [long.as_str(), crossing, at_cut, short];
    let canaries = [
        "issuer-credential-canary",
        "audience-credential-canary",
        "tenant-credential-canary",
        "workspace-credential-canary",
        "pool-credential-canary",
    ];
    let cut = 8192;
    let messages = [
        // A secret longer than the capture limit, logged as one message.
        format!("start {long} end"),
        // A secret that crosses the cut point.
        format!("{}{crossing} after", "a".repeat(cut - 7)),
        // A secret that ends exactly at the cut.
        format!("{}{at_cut}", "b".repeat(cut - at_cut.len())),
        // Several secrets on one line, only some of which fit before the cut.
        format!("{}{short} {at_cut} {crossing} {long}", "c".repeat(cut - 20)),
        // The credential claims themselves.
        format!("{} {} {}", canaries[0], canaries[2], short),
    ];
    for (index, message) in messages.iter().enumerate() {
        let digest = harness.world.add(&logging(message, 1));
        let spec = Spec::new(&digest).with(|s| {
            s.imports = vec!["logging"];
            s.secrets = secrets.iter().map(|secret| (*secret).to_owned()).collect();
        });
        let reply = harness.run(&spec);
        assert_eq!(
            reply.status,
            200,
            "{index}: {}",
            String::from_utf8_lossy(&reply.body)
        );
        let result = reply.json();
        let logs: Vec<String> = serde_json::from_value(result["logs"].clone()).unwrap();
        assert!(!logs.is_empty(), "{index}");
        for line in &logs {
            assert!(line.chars().count() <= cut, "{index}");
            for secret in secrets.iter().chain(canaries.iter()) {
                assert!(
                    !leaks(line, secret),
                    "{index}: output holds part of a secret"
                );
            }
        }
        // Nothing else of the response carries it either.
        let text = String::from_utf8_lossy(&reply.body);
        for secret in canaries.iter().chain([crossing, at_cut].iter()) {
            assert!(!text.contains(secret), "{index}");
        }
    }
    // The credential lives in the host binding layer: the platform's bindings saw it, and no
    // guest-visible host context carries it.
    let held = harness.world.credential.lock().unwrap().clone().unwrap();
    assert!(held.contains("tenant-credential-canary"));
    // What the guest can read of the host context is the invocation identity only.
    for context in harness.world.contexts.lock().unwrap().iter() {
        let visible = [
            &context.invocation_key,
            &context.attempt_id,
            &context.idempotency_key,
        ];
        assert!(visible.iter().all(|value| !value.contains("canary")));
    }
}

#[test]
fn over_budget_redaction_fails_closed_to_a_single_marker_line() {
    let harness = Harness::new(1);
    // 600 maximum-length lines are 4.9 M characters: more than one execution may scan.
    let message = "z".repeat(8192);
    let digest = harness.world.add(&logging(&message, 600));
    let spec = Spec::new(&digest).with(|s| {
        s.imports = vec!["logging"];
        s.max_host_calls = 2000;
    });
    let reply = harness.run(&spec);
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(reply.json()["logs"], json!(["<redacted>"]));
}

#[test]
fn many_distinct_credential_strings_and_a_thousand_long_lines_redact_within_a_bound_and_release_the_slot()
 {
    let harness = Harness::new(1);
    let message = "q".repeat(8192);
    let digest = harness.world.add(&logging(&message, 1000));
    let secrets: Vec<String> = (0..60_000)
        .map(|i| format!("distinct-secret-{i:08}"))
        .collect();
    let spec = Spec::new(&digest).with(|s| {
        s.imports = vec!["logging"];
        s.max_host_calls = 2000;
        s.secrets = secrets.clone();
    });
    let started = Instant::now();
    let reply = harness.run(&spec);
    assert_eq!(reply.status, 200);
    // Over the scan budget the answer is the marker; within it, redacted lines. Either way it
    // finishes within the documented bound (a debug build here; far less in release).
    let logs = reply.json()["logs"].clone();
    assert!(logs == json!(["<redacted>"]) || logs.as_array().is_some_and(|l| l.len() <= 1000));
    assert!(
        started.elapsed() < Duration::from_secs(120),
        "{:?}",
        started.elapsed()
    );
    assert!(harness.server.wait_running(0, BUDGET).is_some());
    // The next request is admitted.
    let ok = harness.world.add(&success());
    assert_eq!(harness.run(&Spec::new(&ok)).status, 200);
}

// --- tests: saturation, control and cancel ------------------------------------------------

fn stalled(addr: SocketAddr, bytes: &[u8]) -> TcpStream {
    let mut stream = connect(addr);
    stream.write_all(bytes).unwrap();
    stream
}

#[test]
fn with_every_execution_running_and_the_execute_listener_saturated_a_cancel_still_takes_effect() {
    let harness = Arc::new(Harness::new(2));
    let digest = harness.world.add(&spin());
    let (first, second) = (Spec::new(&digest), Spec::new(&digest));
    let (a, b) = (harness.spawn(&first), harness.spawn(&second));
    harness.wait_started(2);
    // 2 running + 4 in admission is the execute listener's whole limit; fill the rest with
    // stalled connections: some mid-header, some with headers promising a body.
    let mut held = Vec::new();
    for index in 0..4 {
        held.push(if index % 2 == 0 {
            stalled(harness.execute_addr(), b"POST /v1/exec")
        } else {
            stalled(
                harness.execute_addr(),
                b"POST /v1/execute HTTP/1.1\r\nContent-Length: 4096\r\n",
            )
        });
    }
    assert!(harness.server.wait_connections((6, 0), LONG));
    // A further execute is busy, at once.
    let reply = harness.run(&Spec::new(&digest));
    assert_eq!(code(&reply), (503, "busy".into(), "at_capacity".into()));
    // The cancel on the control listener is accepted and takes effect within the budget.
    let started = Instant::now();
    let answer = harness.cancel(&first.activation);
    assert_eq!(answer.status, 200);
    assert_eq!(answer.json()["cancel"]["state"], "acknowledged");
    assert!(started.elapsed() < BUDGET, "{:?}", started.elapsed());
    assert_eq!(
        code(&a.join().unwrap()),
        (499, "cancelled".into(), "cancel_requested".into())
    );
    assert!(harness.server.wait_running(1, BUDGET).is_some());
    // The other execution is untouched until it is cancelled in its turn.
    assert_eq!(harness.cancel(&second.activation).status, 200);
    assert_eq!(b.join().unwrap().status, 499);
    drop(held);
}

#[test]
fn each_listener_refuses_the_others_operations_and_unknown_paths() {
    let harness = Harness::new(1);
    let digest = harness.world.add(&success());
    let not_found = |reply: Reply2| {
        (
            reply.status,
            reply.json()["code"].as_str().unwrap().to_owned(),
        )
    };
    let execute = Spec::new(&digest).request().to_string();
    let cancel = json!({
        "version": "1", "type": "cancel", "activation_id": "act-x-0001",
        "attempt_id": "att-x-0001", "cancel": {"state": "requested"}
    })
    .to_string();
    assert_eq!(
        not_found(post(
            harness.control_addr(),
            "/v1/execute",
            execute.as_bytes()
        )),
        (404, "not_found".into())
    );
    assert_eq!(
        not_found(post(
            harness.execute_addr(),
            "/v1/cancel",
            cancel.as_bytes()
        )),
        (404, "not_found".into())
    );
    assert_eq!(
        not_found(post(
            harness.execute_addr(),
            "/v1/update",
            cancel.as_bytes()
        )),
        (404, "not_found".into())
    );
    assert_eq!(
        not_found(post(harness.execute_addr(), "/nothing", b"{}")),
        (404, "not_found".into())
    );
    assert_eq!(
        not_found(post(harness.control_addr(), "/nothing", b"{}")),
        (404, "not_found".into())
    );
    // A cancel for an attempt that is not running is a 404 of its own reason.
    let unknown = post(harness.control_addr(), "/v1/cancel", cancel.as_bytes());
    assert_eq!(
        code(&unknown),
        (404, "not_found".into(), "unknown_execution".into())
    );
    // Execute still works on its own listener.
    assert_eq!(
        post(harness.execute_addr(), "/v1/execute", execute.as_bytes()).status,
        200
    );
}

#[test]
fn stalled_control_connections_leave_executions_alone_and_the_listener_recovers() {
    let harness = Arc::new(Harness::with(1, |config| {
        config.control_deadlines = Deadlines {
            header: Duration::from_millis(600),
            body: Duration::from_millis(600),
            idle: Duration::from_millis(300),
            response: Duration::from_millis(600),
        };
    }));
    let digest = harness.world.add(&spin());
    let spec = Spec::new(&digest);
    let handle = harness.spawn(&spec);
    harness.wait_started(1);
    let held: Vec<_> = (0..16)
        .map(|_| stalled(harness.control_addr(), b"POST /v1/can"))
        .collect();
    assert!(harness.server.wait_connections((1, 16), LONG));
    // The control listener is full; the running execution is not affected by it.
    assert_eq!(harness.server.running(), 1);
    let refused = harness.cancel(&spec.activation);
    assert_eq!(code(&refused), (503, "busy".into(), "at_capacity".into()));
    assert_eq!(harness.server.running(), 1);
    // After the control deadlines the stalled connections are closed and cancel works again.
    assert!(harness.server.wait_connections((1, 0), LONG));
    assert_eq!(harness.cancel(&spec.activation).status, 200);
    assert_eq!(handle.join().unwrap().status, 499);
    drop(held);
}

#[test]
fn an_update_moves_forward_only() {
    let harness = Arc::new(Harness::new(1));
    let digest = harness.world.add(&spin());
    let spec = Spec::new(&digest);
    let handle = harness.spawn(&spec);
    harness.wait_started(1);
    let update = |sequence: u64| {
        let body = json!({
            "version": "1", "type": "update", "activation_id": spec.activation,
            "attempt_id": "att-local-0001", "sequence": sequence, "update": {"kind": "heartbeat"}
        });
        post(
            harness.control_addr(),
            "/v1/update",
            body.to_string().as_bytes(),
        )
    };
    assert_eq!(update(2).status, 202);
    assert_eq!(
        code(&update(2)),
        (409, "stale_update".into(), "sequence_not_newer".into())
    );
    assert_eq!(
        code(&update(1)),
        (409, "stale_update".into(), "sequence_not_newer".into())
    );
    assert_eq!(update(3).status, 202);
    assert_eq!(harness.cancel(&spec.activation).status, 200);
    assert_eq!(handle.join().unwrap().status, 499);
}

// --- tests: timeout selection and budgets -------------------------------------------------

/// A blocking guest runs without a cancel and ends at the shortest limit.
fn timed(harness: &Arc<Harness>, spec: &Spec, shortest: Duration) {
    let before = harness
        .server
        .hooks()
        .guests_started
        .load(Ordering::Acquire);
    let started = Instant::now();
    let reply = harness.run(spec);
    let elapsed = started.elapsed();
    assert_eq!(
        code(&reply),
        (504, "timeout".into(), "deadline_exceeded".into())
    );
    let result = reply.json();
    assert_eq!(result["type"], "result");
    assert_eq!(result["activation_id"], spec.activation);
    assert_eq!(result["identity"]["wasmtime_version"], WORKER_ENGINE);
    assert_eq!(result["class"], "uncertain");
    // The guest started, and the execution ended at the shortest limit, within the budget.
    assert_eq!(
        harness
            .server
            .hooks()
            .guests_started
            .load(Ordering::Acquire),
        before + 1
    );
    assert!(
        elapsed >= shortest.saturating_sub(Duration::from_millis(50)),
        "{elapsed:?} < {shortest:?}"
    );
    assert!(
        elapsed < shortest + BUDGET,
        "{elapsed:?} >= {shortest:?} + budget"
    );
    // Capacity is recovered: the next execute is admitted.
    assert!(harness.server.wait_running(0, BUDGET).is_some());
}

#[test]
fn the_shortest_of_the_request_the_projection_and_the_worker_limit_wins() {
    let harness = Arc::new(Harness::with(1, |config| {
        config.ceilings.max_wall_clock_ms = 2500
    }));
    let digest = harness.world.add(&spin());
    harness.warm(&Spec::new(&digest));
    // The request's own deadline is the shortest (whole seconds: between 1 and 2 s away).
    let request = Spec::new(&digest).with(|s| s.deadline_secs = 2);
    let started = Instant::now();
    let reply = harness.run(&request);
    let elapsed = started.elapsed();
    assert_eq!(
        code(&reply),
        (504, "timeout".into(), "deadline_exceeded".into())
    );
    assert!(
        elapsed > Duration::from_millis(900) && elapsed < Duration::from_secs(2) + BUDGET,
        "{elapsed:?}"
    );
    // The projection's limit is the shortest.
    timed(
        &harness,
        &Spec::new(&digest).with(|s| s.timeout_ms = 700),
        Duration::from_millis(700),
    );
    // The worker's wall-clock limit is the shortest of all.
    timed(
        &harness,
        &Spec::new(&digest).with(|s| s.timeout_ms = 20_000),
        Duration::from_millis(2500),
    );
    let ok = harness.world.add(&success());
    assert_eq!(harness.run(&Spec::new(&ok)).status, 200);
}

#[test]
fn a_host_call_after_the_deadline_ends_the_execution_as_a_timeout() {
    let harness = Arc::new(Harness::new(1));
    let digest = harness.world.add(&calling());
    harness.warm(&Spec::new(&digest).with(|s| s.imports = vec!["context"]));
    timed(
        &harness,
        &Spec::new(&digest).with(|s| {
            s.imports = vec!["context"];
            s.timeout_ms = 600;
            s.max_host_calls = 1 << 50;
        }),
        Duration::from_millis(600),
    );
}

#[test]
fn memory_fuel_and_host_call_budgets_end_the_execution_with_their_own_reason() {
    let harness = Arc::new(Harness::with(1, |config| {
        config.ceilings.max_memory_bytes = 64 * 1024 * 1024
    }));
    let ok = harness.world.add(&success());
    let failed = |reason: &str| (500, "execution_failed".to_owned(), reason.to_owned());
    // Memory: the 4 MiB fixture fails at 1 MiB and passes with enough.
    let small = Spec::new(&ok).with(|s| s.memory_bytes = 1024 * 1024);
    let reply = harness.run(&small);
    assert_eq!(code(&reply), failed("memory_budget_exceeded"));
    assert_eq!(reply.json()["result"]["retryable"], false);
    assert_eq!(
        harness
            .run(&Spec::new(&ok).with(|s| s.memory_bytes = 8 * 1024 * 1024))
            .status,
        200
    );
    // Fuel: a small budget stops a looping guest well before `timeout_ms`.
    let looping = harness.world.add(&spin());
    let started = Instant::now();
    let reply = harness.run(&Spec::new(&looping).with(|s| s.fuel = 100_000));
    assert_eq!(code(&reply), failed("fuel_exhausted"));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    // Host calls: N+1 calls make `host_call_budget_exceeded`, and the host saw exactly N.
    let reports = harness.world.add(&reporting(4));
    let before = harness.world.host_calls.load(Ordering::Acquire);
    let reply = harness.run(&Spec::new(&reports).with(|s| {
        s.imports = vec!["progress"];
        s.max_host_calls = 3;
    }));
    assert_eq!(code(&reply), failed("host_call_budget_exceeded"));
    assert_eq!(harness.world.host_calls.load(Ordering::Acquire) - before, 3);
    // Sufficient budgets pass.
    let reply = harness.run(&Spec::new(&reports).with(|s| {
        s.imports = vec!["progress"];
        s.max_host_calls = 4;
    }));
    assert_eq!(reply.status, 200);
    // A memory budget above the worker's ceiling is refused before compile or instantiate.
    let fresh = harness
        .world
        .add(&guest(&[], 1, &[(1024, OK_JSON)], &ret(OK_JSON.len())));
    let binds = harness.world.binds.load(Ordering::Acquire);
    let validations = harness
        .server
        .cache_hooks()
        .validations
        .load(Ordering::Acquire);
    let above = Spec::new(&fresh).with(|s| s.memory_bytes = 128 * 1024 * 1024);
    assert_eq!(
        code(&harness.run(&above)),
        (
            400,
            "invalid_request".into(),
            "budget_above_worker_ceiling".into()
        )
    );
    assert_eq!(harness.world.binds.load(Ordering::Acquire), binds);
    assert_eq!(
        harness
            .server
            .cache_hooks()
            .validations
            .load(Ordering::Acquire),
        validations
    );
}

// --- tests: stalled steps under the deadline and cancellation -----------------------------

/// Stalls the first arrival at a stage, tells the test it got there, and lets go when the
/// gate is dropped.
struct Gate {
    reached: mpsc::Receiver<()>,
    _release: mpsc::Sender<()>,
}

#[derive(Clone, Copy, Debug)]
enum Stall {
    Setup,
    Validate,
    Read,
    Compile,
    Publish,
}

fn install(harness: &Harness, stall: Stall) -> Gate {
    use falcone_serverless_worker::cache::hooks::Stage;
    let (reached_tx, reached) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let reached_tx = Mutex::new(reached_tx);
    let release_rx = Mutex::new(release_rx);
    let first = std::sync::atomic::AtomicBool::new(true);
    let stop = move || {
        if first.swap(false, Ordering::AcqRel) {
            let _ = reached_tx.lock().unwrap().send(());
            let _ = release_rx.lock().unwrap().recv_timeout(LONG);
        }
    };
    match stall {
        Stall::Setup => harness.server.hooks().on_setup(stop),
        other => {
            let at = match other {
                Stall::Validate => Stage::Validate,
                Stall::Read => Stage::Read,
                Stall::Compile => Stage::Compile,
                _ => Stage::Publish,
            };
            harness.server.cache_hooks().set(move |stage| {
                if stage == at {
                    stop();
                }
            });
        }
    }
    Gate {
        reached,
        _release: release,
    }
}

#[derive(Clone, Copy, Debug)]
enum Leave {
    /// `until` passes: the request's `timeout_ms`.
    Timeout(u64),
    Cancel,
}

fn wait_for_abandoned_steps(harness: &Harness) {
    let limit = Instant::now() + LONG;
    while harness.server.abandoned_running() > 0 {
        assert!(Instant::now() < limit, "an abandoned step never ended");
        std::thread::yield_now();
    }
}

fn cache_entries(harness: &Harness) -> Vec<String> {
    std::fs::read_dir(harness.cache_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn stalled_step(stall: Stall, leave: Leave) {
    let harness = Arc::new(Harness::new(2));
    let digest = harness.world.add(&success());
    let mut spec = Spec::new(&digest);
    if matches!(stall, Stall::Setup) {
        // Setup follows the cache: warm it so the stall is the only slow step.
        assert_eq!(harness.run(&spec).status, 200);
        spec = Spec::new(&digest);
    }
    if let Leave::Timeout(ms) = leave {
        spec.timeout_ms = ms;
    }
    let guests = harness
        .server
        .hooks()
        .guests_started
        .load(Ordering::Acquire);
    let binds = harness.world.binds.load(Ordering::Acquire);
    let gate = install(&harness, stall);
    let started = Instant::now();
    let pending = harness.spawn(&spec);
    // The stage was reached: the test proceeds on the server's own event.
    gate.reached
        .recv_timeout(LONG)
        .expect("the stage was reached");
    let (status, name) = match leave {
        Leave::Timeout(ms) => {
            let released = harness
                .server
                .wait_running(0, Duration::from_millis(ms) + BUDGET)
                .unwrap_or_else(|| panic!("{stall:?}: the slot was held past until + budget"));
            assert!(
                started.elapsed() <= Duration::from_millis(ms) + BUDGET,
                "{stall:?}: {released:?}"
            );
            (504, "timeout")
        }
        Leave::Cancel => {
            let answer = harness.cancel(&spec.activation);
            let released = Instant::now();
            assert_eq!(answer.status, 200, "{stall:?}");
            assert_eq!(
                answer.json()["cancel"]["state"],
                "acknowledged",
                "{stall:?}"
            );
            assert!(
                harness.server.wait_running(0, BUDGET).is_some(),
                "{stall:?}: slot held"
            );
            assert!(released.elapsed() < BUDGET, "{stall:?}");
            (499, if true { "cancelled" } else { "" })
        }
    };
    let reply = pending.join().unwrap();
    assert_eq!(
        reply.status,
        status,
        "{stall:?} {leave:?}: {}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(code(&reply).1, name, "{stall:?} {leave:?}");
    // No guest started, and nothing was bound after the stall.
    assert_eq!(
        harness
            .server
            .hooks()
            .guests_started
            .load(Ordering::Acquire),
        guests,
        "{stall:?}"
    );
    if !matches!(stall, Stall::Setup) {
        assert_eq!(
            harness.world.binds.load(Ordering::Acquire),
            binds,
            "{stall:?}"
        );
    }
    // Let the abandoned step go: it must publish nothing and leave nothing behind.
    drop(gate);
    wait_for_abandoned_steps(&harness);
    let leftover = cache_entries(&harness);
    if matches!(stall, Stall::Setup) {
        assert_eq!(
            leftover.len(),
            1,
            "{stall:?}: only the warmed entry: {leftover:?}"
        );
    } else {
        assert!(leftover.is_empty(), "{stall:?}: partial entry {leftover:?}");
    }
    // A following execute is admitted.
    let next = harness
        .world
        .add(&guest(&[], 64, &[(1024, OK_JSON)], &ret(OK_JSON.len())));
    assert_eq!(harness.run(&Spec::new(&next)).status, 200, "{stall:?}");
}

macro_rules! stalled_steps {
    ($($name:ident: $stall:expr, $leave:expr;)*) => {$(
        #[test]
        fn $name() {
            stalled_step($stall, $leave);
        }
    )*};
}

stalled_steps! {
    setup_stalled_past_until_is_a_timeout: Stall::Setup, Leave::Timeout(1500);
    setup_stalled_then_cancelled_is_cancelled: Stall::Setup, Leave::Cancel;
    validation_stalled_past_until_is_a_timeout: Stall::Validate, Leave::Timeout(1500);
    validation_stalled_then_cancelled_is_cancelled: Stall::Validate, Leave::Cancel;
    cache_read_stalled_past_until_is_a_timeout: Stall::Read, Leave::Timeout(1500);
    cache_read_stalled_then_cancelled_is_cancelled: Stall::Read, Leave::Cancel;
    compilation_stalled_past_until_is_a_timeout: Stall::Compile, Leave::Timeout(1500);
    compilation_stalled_then_cancelled_is_cancelled: Stall::Compile, Leave::Cancel;
    cache_publication_stalled_past_until_is_a_timeout: Stall::Publish, Leave::Timeout(2500);
    cache_publication_stalled_then_cancelled_is_cancelled: Stall::Publish, Leave::Cancel;
}

#[test]
fn a_large_cold_artifact_is_validated_under_the_deadline_and_cancellation() {
    // About 48 MiB of component: a data segment that validation must scan.
    let big = guest(
        &[],
        64,
        &[
            (4096, &"x".repeat(48 * 1024 * 1024 - 8192)),
            (1024, OK_JSON),
        ],
        &ret(OK_JSON.len()),
    );
    assert!(big.len() > 40 * 1024 * 1024);
    let harness = Arc::new(Harness::new(2));
    let digest = harness.world.add(&big);
    // Cancellation during validation.
    let spec = Spec::new(&digest);
    let gate = install(&harness, Stall::Validate);
    let pending = harness.spawn(&spec);
    gate.reached.recv_timeout(LONG).unwrap();
    assert_eq!(harness.cancel(&spec.activation).status, 200);
    assert_eq!(pending.join().unwrap().status, 499);
    assert!(harness.server.wait_running(0, BUDGET).is_some());
    drop(gate);
    wait_for_abandoned_steps(&harness);
    // A short timeout during validation.
    let spec = Spec::new(&digest).with(|s| s.timeout_ms = 300);
    let started = Instant::now();
    let reply = harness.run(&spec);
    assert_eq!(code(&reply).1, "timeout");
    assert!(
        started.elapsed() < Duration::from_millis(300) + BUDGET,
        "{:?}",
        started.elapsed()
    );
    wait_for_abandoned_steps(&harness);
    let ok = harness.world.add(&success());
    assert_eq!(harness.run(&Spec::new(&ok)).status, 200);
}

// --- tests: HTTP ceilings, transfer deadlines and slot recovery ---------------------------

fn tight(config: &mut ServerConfig) {
    let deadlines = Deadlines {
        header: Duration::from_millis(700),
        body: Duration::from_millis(900),
        idle: Duration::from_millis(400),
        response: Duration::from_millis(900),
    };
    config.execute_deadlines = deadlines;
    config.control_deadlines = deadlines;
}

/// After a case: no connection is held, and a normal execute and a cancel both succeed.
fn recovered(harness: &Arc<Harness>, case: &str) {
    assert!(
        harness.server.wait_connections((0, 0), LONG),
        "{case}: a connection leaked"
    );
    let digest = harness.world.add(&success());
    assert_eq!(harness.run(&Spec::new(&digest)).status, 200, "{case}");
    assert_eq!(
        code(&harness.cancel("act-none-0001")).1,
        "not_found",
        "{case}"
    );
}

fn closed_within(mut stream: TcpStream, bound: Duration, case: &str) {
    let started = Instant::now();
    stream.set_read_timeout(Some(LONG)).unwrap();
    let mut sink = Vec::new();
    let _ = stream.read_to_end(&mut sink);
    assert!(
        started.elapsed() < bound,
        "{case}: closed after {:?}",
        started.elapsed()
    );
}

#[test]
fn http_ceilings_and_transfer_deadlines_hold_over_a_real_socket_and_slots_recover() {
    let harness = Arc::new(Harness::with(2, tight));
    let guests = || {
        harness
            .server
            .hooks()
            .guests_started
            .load(Ordering::Acquire)
    };
    // Oversized headers are refused without running anything.
    let mut head = b"POST /v1/execute HTTP/1.1\r\nX-Padding: ".to_vec();
    head.extend(std::iter::repeat_n(b'a', 20 * 1024));
    head.extend_from_slice(b"\r\nContent-Length: 0\r\n\r\n");
    let mut stream = connect(harness.execute_addr());
    stream.write_all(&head).unwrap();
    let reply = read_reply(stream).expect("a response");
    assert_eq!(
        code(&reply),
        (413, "request_too_large".into(), "headers_too_large".into())
    );
    recovered(&harness, "oversized headers");
    // An oversized request is refused from its headers, before its body is read.
    let reply = post_headers_only(harness.execute_addr(), "/v1/execute", 8 * 1024 * 1024 + 1);
    assert_eq!(
        code(&reply),
        (413, "request_too_large".into(), "body_too_large".into())
    );
    recovered(&harness, "oversized request");
    // A connection that sends nothing is closed within the idle bound.
    closed_within(
        connect(harness.execute_addr()),
        Duration::from_millis(400) + Duration::from_secs(2),
        "silent",
    );
    recovered(&harness, "silent");
    // One that drips header bytes is closed at the absolute header deadline.
    let mut stream = connect(harness.execute_addr());
    let started = Instant::now();
    stream.write_all(b"POST /v1/execute HTTP/1.1\r\n").unwrap();
    loop {
        // Each byte arrives well inside the idle timeout; only the absolute deadline can end it.
        std::thread::sleep(Duration::from_millis(100));
        if stream.write_all(b"X").is_err() || started.elapsed() > Duration::from_secs(5) {
            break;
        }
        let mut probe = [0u8; 1];
        stream.set_nonblocking(true).unwrap();
        if matches!(stream.read(&mut probe), Ok(0)) {
            break;
        }
        stream.set_nonblocking(false).unwrap();
    }
    assert!(
        started.elapsed() < Duration::from_millis(700) + Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    recovered(&harness, "dripped headers");
    // A body that stalls is closed within the idle bound, and nothing ran.
    let before = guests();
    let mut stream = connect(harness.execute_addr());
    stream
        .write_all(b"POST /v1/execute HTTP/1.1\r\nContent-Length: 1000\r\n\r\n0123456789")
        .unwrap();
    closed_within(
        stream,
        Duration::from_millis(400) + Duration::from_secs(2),
        "stalled body",
    );
    assert_eq!(guests(), before);
    recovered(&harness, "stalled body");
    // The control listener has its own limits: an oversized control body is refused.
    let reply = post_headers_only(harness.control_addr(), "/v1/cancel", 1024 * 1024);
    assert_eq!(
        code(&reply),
        (413, "request_too_large".into(), "body_too_large".into())
    );
    recovered(&harness, "oversized control body");
}

#[test]
fn a_slow_reader_of_a_large_response_is_cut_off_and_releases_its_permit() {
    let harness = Arc::new(Harness::with(2, tight));
    // About 8 MiB of log text per response: 500 lines of 8192 two-byte characters.
    let message = "é".repeat(8192);
    let digest = harness.world.add(&logging(&message, 500));
    let spec = |_| {
        Spec::new(&digest).with(|s| {
            s.imports = vec!["logging"];
            s.max_host_calls = 2000;
        })
    };
    let mut readers = Vec::new();
    for index in 0..2 {
        let mut stream = connect(harness.execute_addr());
        let body = spec(index).request().to_string();
        let head = format!(
            "POST /v1/execute HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        readers.push(stream);
    }
    // Both executions finish (their slots are released before the response is written).
    assert!(harness.server.wait_running(0, LONG).is_some());
    // Read one byte at a time, far slower than the response deadline allows.
    let started = Instant::now();
    for stream in &mut readers {
        let mut byte = [0u8; 1];
        stream.set_read_timeout(Some(LONG)).unwrap();
        assert_eq!(stream.read(&mut byte).unwrap(), 1);
    }
    // The permits are released within the response deadline plus the budget, however slowly
    // the clients read.
    assert!(harness.server.wait_connections(
        (0, 0),
        Duration::from_millis(900) + BUDGET + Duration::from_secs(2)
    ));
    assert!(started.elapsed() < Duration::from_millis(900) + BUDGET + Duration::from_secs(2));
    drop(readers);
    recovered(&harness, "slow readers");
}

// --- tests: the failure code contract -----------------------------------------------------

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read_json(path: &str) -> Value {
    serde_json::from_slice(&std::fs::read(root().join(path)).unwrap()).unwrap()
}

#[test]
fn the_code_table_the_schema_the_spec_and_the_code_enum_agree_on_every_code() {
    use falcone_serverless_worker::api::{Class, Code, Reason, Refusal};
    let table = read_json("docs/contracts/worker-execution-api/failure-codes.v1.json");
    let table = table.as_array().unwrap();
    let common = read_json("docs/contracts/worker-execution-api/common.v1.schema.json");
    let failure = &common["$defs"]["failure"];
    let spec = std::fs::read_to_string(
        root().join("docs/contracts/worker-execution-api/execution-api-v1.md"),
    )
    .unwrap();
    // Every code of the enum is a row, and every row a code.
    let names: Vec<String> = Code::ALL
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    let rows: Vec<String> = table
        .iter()
        .map(|r| r["code"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, rows);
    let schema_codes: Vec<String> = failure["properties"]["code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(schema_codes, names);
    let mut every_reason = std::collections::BTreeSet::new();
    for (row, code) in table.iter().zip(Code::ALL) {
        let name = row["code"].as_str().unwrap();
        // The enum's reasons are the row's reasons.
        let reasons: Vec<String> = code
            .reasons()
            .iter()
            .map(|r| {
                serde_json::to_value(r)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let listed: Vec<String> = row["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(reasons, listed, "{name}");
        every_reason.extend(listed.iter().cloned());
        assert_eq!(
            u64::from(code.status()),
            row["status"].as_u64().unwrap(),
            "{name}"
        );
        for reason in &listed {
            let reason_value: Reason = serde_json::from_value(json!(reason)).unwrap();
            let refusal = Refusal::new(code, reason_value);
            let wire = serde_json::to_value(&refusal).unwrap();
            assert_eq!(wire["status"], row["status"], "{name}");
            assert_eq!(wire["code"], name);
            assert_eq!(wire["detail"]["reason"], json!(reason));
            // The class is the row's, or the reason's own override.
            let expected = row["class_by_reason"][reason.as_str()]
                .as_str()
                .unwrap_or(row["class"].as_str().unwrap());
            assert_eq!(
                serde_json::to_value(refusal.class()).unwrap(),
                json!(expected),
                "{name}/{reason}"
            );
            let retryable = refusal.class() == Class::Retryable;
            assert_eq!(wire["retryable"], json!(retryable), "{name}/{reason}");
            assert_eq!(wire["retryable"], row["retryable"], "{name}/{reason}");
        }
        // The schema: a status, a retryable value and the reasons, for the code.
        let block = failure["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["if"]["properties"]["code"]["const"] == json!(name))
            .unwrap_or_else(|| panic!("{name} has no schema block"));
        assert_eq!(
            block["then"]["properties"]["status"]["const"], row["status"],
            "{name}"
        );
        assert_eq!(
            block["then"]["properties"]["retryable"]["const"], row["retryable"],
            "{name}"
        );
        assert_eq!(
            block["then"]["properties"]["detail"]["properties"]["reason"]["enum"], row["reasons"],
            "{name}"
        );
        // The spec: one table row with the status, every reason and the class.
        let line = spec
            .lines()
            .find(|l| l.starts_with(&format!("| `{name}` |")))
            .unwrap_or_else(|| panic!("{name} is not in the spec table"));
        assert!(line.contains(&format!("| {} |", row["status"])), "{name}");
        for reason in &listed {
            assert!(line.contains(reason.as_str()), "{name}/{reason}");
        }
        assert!(line.contains(row["class"].as_str().unwrap()), "{name}");
    }
    let schema_reasons: std::collections::BTreeSet<String> =
        failure["properties"]["detail"]["properties"]["reason"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap().to_owned())
            .collect();
    assert_eq!(schema_reasons, every_reason);
}

// --- tests: the production binary and image -----------------------------------------------

struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn the_production_binary_serves_both_listeners_with_its_sibling_compile_child() {
    let dir = TempDir::new("serve");
    let artifacts = dir.0.join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let bytes = success();
    let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    std::fs::write(artifacts.join(format!("{}.wasm", &digest[7..])), &bytes).unwrap();
    let (execute, control) = (free_port(), free_port());
    let config = dir.0.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "pool_id = \"pool-local\"\nfalcone_api_endpoint = \"https://api.invalid\"\n\
             falcone_dispatcher_endpoint = \"https://dispatcher.invalid\"\n\
             listen_port = {execute}\ncontrol_port = {control}\n\
             cache_path = \"{}\"\nartifact_path = \"{}\"\n",
            dir.0.join("cache").display(),
            artifacts.display()
        ),
    )
    .unwrap();
    let _worker = Killed(
        std::process::Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
            .arg("serve")
            .arg(&config)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let execute_addr: SocketAddr = format!("127.0.0.1:{execute}").parse().unwrap();
    let control_addr: SocketAddr = format!("127.0.0.1:{control}").parse().unwrap();
    // Readiness: poll until the listener accepts.
    let limit = Instant::now() + LONG;
    while TcpStream::connect(execute_addr).is_err() || TcpStream::connect(control_addr).is_err() {
        assert!(Instant::now() < limit, "the worker never listened");
        std::thread::yield_now();
    }
    let spec = Spec::new(&digest);
    let reply = post(
        execute_addr,
        "/v1/execute",
        spec.request().to_string().as_bytes(),
    );
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(reply.json()["identity"]["wasmtime_version"], WORKER_ENGINE);
    assert_eq!(post(control_addr, "/v1/execute", b"{}").status, 404);
    assert_eq!(post(execute_addr, "/v1/cancel", b"{}").status, 404);
    // An artifact the platform does not have is a failure of its own reason, not a crash.
    let missing = Spec::new(&format!("sha256:{}", "9".repeat(64)));
    assert_eq!(
        code(&post(
            execute_addr,
            "/v1/execute",
            missing.request().to_string().as_bytes()
        )),
        (
            500,
            "execution_failed".into(),
            "artifact_unavailable".into()
        )
    );
}

#[test]
fn the_production_image_ships_the_compile_child_beside_the_worker() {
    let production = std::fs::read_to_string(root().join("worker/Containerfile")).unwrap();
    let copies: Vec<&str> = production
        .lines()
        .filter(|l| l.starts_with("COPY --from=build"))
        .collect();
    let target = |binary: &str| {
        copies
            .iter()
            .find(|line| line.contains(&format!("/release/{binary} ")))
            .and_then(|line| line.split_whitespace().last())
            .unwrap_or_else(|| panic!("{binary} is not copied into the production image"))
            .to_owned()
    };
    let (worker, child) = (
        target("falcone-serverless-worker"),
        target("artifact-compile-child"),
    );
    assert_eq!(
        std::path::Path::new(&worker).parent(),
        std::path::Path::new(&child).parent()
    );
    assert!(child.ends_with("/artifact-compile-child"));
    // The qualification image ships the same suite, which runs in the restricted pod.
    let qualification =
        std::fs::read_to_string(root().join("worker/qualification/Containerfile")).unwrap();
    assert!(qualification.contains("/usr/local/bin/execute_api_local"));
    assert!(qualification.contains("--test execute_api_local"));
}
