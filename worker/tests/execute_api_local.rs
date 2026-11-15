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
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

#[path = "support/execute_api_fixtures.rs"]
mod support;
use support::*;

/// A guest that announces itself, then calls the host's invocation context for ever.
fn calling() -> Vec<u8> {
    guest(
        &["context", "progress"],
        64,
        &[],
        &format!(
            "{ANNOUNCE} (loop $l (call $key (i32.const 3000)) (br $l)) {}",
            ret(0)
        ),
    )
}

// --- platform -----------------------------------------------------------------------------

/// The system boundary: artifacts by digest, and bindings that count the calls they see.
#[derive(Default)]
struct World {
    artifacts: Mutex<HashMap<String, Vec<u8>>>,
    /// Operations that reached the platform's bindings.
    host_calls: Arc<AtomicUsize>,
    actions: Arc<Actions>,
    binds: AtomicUsize,
    contexts: Mutex<Vec<Context>>,
    /// The last credential bound, as the platform's binding layer saw it.
    credential: Mutex<Option<String>>,
}

/// What the platform's bindings saw of the guest's first actions (progress reports). A test
/// that needs "the guest ran" waits here, not on a server event that precedes instantiation.
#[derive(Default)]
struct Actions {
    count: AtomicUsize,
    sender: Mutex<Option<mpsc::Sender<()>>>,
    /// When armed, a progress report blocks in the binding until the holder lets go.
    held: Mutex<Option<Arc<Mutex<mpsc::Receiver<()>>>>>,
}

impl Actions {
    /// Blocks every following progress report until the returned sender is dropped.
    fn block(&self) -> mpsc::Sender<()> {
        let (release, held) = mpsc::channel();
        *self.held.lock().unwrap() = Some(Arc::new(Mutex::new(held)));
        release
    }

    fn unblock(&self) {
        *self.held.lock().unwrap() = None;
    }
}

struct Counting(Arc<AtomicUsize>, Arc<Actions>);

impl Bindings for Counting {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        self.0.fetch_add(1, Ordering::AcqRel);
        match operation {
            Operation::Progress { .. } => {
                self.1.count.fetch_add(1, Ordering::AcqRel);
                if let Some(sender) = &*self.1.sender.lock().unwrap() {
                    let _ = sender.send(());
                }
                let held = self.1.held.lock().unwrap().clone();
                if let Some(held) = held {
                    let _ = held.lock().unwrap().recv_timeout(LONG);
                }
                Ok(Reply::Empty)
            }
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
        Ok(Arc::new(Counting(
            self.host_calls.clone(),
            self.actions.clone(),
        )))
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

// --- harness ------------------------------------------------------------------------------

struct Harness {
    server: Server,
    world: Arc<World>,
    /// The guests' first actions, in order.
    acted: Mutex<mpsc::Receiver<()>>,
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
        *world.actions.sender.lock().unwrap() = Some(send);
        Self {
            server,
            world,
            acted: Mutex::new(receive),
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

    /// Waits for `count` guests to take their first action: a call that reached the
    /// platform's bindings from inside the running guest.
    fn wait_acted(&self, count: usize) {
        let receive = self.acted.lock().unwrap();
        for _ in 0..count {
            receive.recv_timeout(LONG).expect("a guest acted");
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
            let receive = self.acted.lock().unwrap();
            // The request ends first only when it failed before the guest acted.
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

#[test]
fn an_invalid_handler_result_is_a_500_invalid_handler_result_that_keeps_its_logs() {
    let harness = Harness::new(2);
    // Each is JSON, so it is a handler result, but none is a valid legacy result.
    for text in [
        r#"{"statusCode":600}"#,
        r#"{"statusCode":99,"body":"x"}"#,
        r#"{"statusCode":"200"}"#,
        r#"{"statusCode":200,"headers":{"a":1}}"#,
        r#"{"statusCode":200,"extra":true}"#,
    ] {
        let digest = harness
            .world
            .add(&logging_then_returning("kept log line", 1, text));
        let spec = Spec::new(&digest).with(|s| s.imports = vec!["logging"]);
        let reply = harness.run(&spec);
        assert_eq!(
            code(&reply),
            (
                500,
                "execution_failed".into(),
                "invalid_handler_result".into()
            ),
            "{text}"
        );
        let result = reply.json();
        assert_eq!(result["status"], "failure", "{text}");
        assert_eq!(result["logs"], json!(["info kept log line"]), "{text}");
        assert_eq!(result["class"], "non_retryable", "{text}");
        assert_eq!(
            result["identity"]["wasmtime_version"], WORKER_ENGINE,
            "{text}"
        );
    }
    // A valid result keeps HTTP 200, whatever its own statusCode.
    let digest = harness
        .world
        .add(&logging_then_returning("ok", 1, r#"{"statusCode":201}"#));
    let spec = Spec::new(&digest).with(|s| s.imports = vec!["logging"]);
    let reply = harness.run(&spec);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["result"]["statusCode"], 201);
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
    let running = Spec::blocking(&digest);
    let handle = harness.spawn(&running);
    harness.wait_acted(1);
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

/// Every credential claim, and a secret that the revision's grants carry as a host scope.
const CLAIM_CANARIES: [&str; 5] = [
    "issuer-credential-canary",
    "audience-credential-canary",
    "tenant-credential-canary",
    "workspace-credential-canary",
    "pool-credential-canary",
];
const SCOPE_SECRET: &str = "cr0ss1ng-s3cret-v4lue";

#[test]
fn a_guest_scanning_its_own_memory_finds_no_credential_material_but_does_find_what_the_host_wrote()
{
    let harness = Harness::new(2);
    let mut secrets = CLAIM_CANARIES.to_vec();
    secrets.push(SCOPE_SECRET);
    // The positive controls: the params and the invocation key are what the host writes into
    // a guest's memory, so a scan that cannot see them proves nothing about absence.
    let digest = harness
        .world
        .add(&memory_probe(&secrets, &["/v1/local", "inv-local-0001"]));
    let spec = Spec::new(&digest).with(|s| {
        s.imports = vec!["context"];
        s.secrets = vec![SCOPE_SECRET.to_owned()];
    });
    let reply = harness.run(&spec);
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(probe_report(&reply.json()), (0, 2));
    // The platform's binding layer did receive the credential: the guest saw none of it.
    let held = harness.world.credential.lock().unwrap().clone().unwrap();
    assert!(CLAIM_CANARIES.iter().all(|canary| held.contains(canary)));
    // The probe itself detects material that is in its memory: it finds a needle that the
    // host put there, in a second guest that searches for the invocation key as a "secret".
    let detecting = harness
        .world
        .add(&memory_probe(&["inv-local-0001"], &["/v1/local"]));
    let spec = Spec::new(&detecting).with(|s| s.imports = vec!["context"]);
    assert_eq!(probe_report(&harness.run(&spec).json()), (1, 1));
}

#[test]
fn guest_access_to_the_environment_or_a_file_is_refused_by_name_before_any_guest_runs() {
    let harness = Harness::new(2);
    for prohibited in ["wasi-environment", "wasi-preopens", "wasi-filesystem"] {
        let digest = harness.world.add(&guest(
            &[prohibited],
            64,
            &[(1024, OK_JSON)],
            &ret(OK_JSON.len()),
        ));
        let refused = (
            422,
            "undeclared_import".to_owned(),
            "import_not_declared".to_owned(),
        );
        // Not declared by the revision, and declared by it: the worker's own list decides.
        assert_eq!(
            code(&harness.run(&Spec::new(&digest))),
            refused,
            "{prohibited}"
        );
        let declared = Spec::new(&digest).with(|s| s.imports = vec![prohibited]);
        assert_eq!(code(&harness.run(&declared)), refused, "{prohibited}");
    }
    // A component that exports anything beside the handler is refused as well.
    let extra = harness.world.add(&guest_with(
        &[],
        64,
        &[(1024, OK_JSON)],
        "",
        r#"(export "wasi:cli/run@0.2.6" (instance $handler))"#,
        &ret(OK_JSON.len()),
    ));
    let (status, failure, reason) = code(&harness.run(&Spec::new(&extra)));
    assert_eq!(
        (status, failure.as_str(), reason.as_str()),
        (500, "execution_failed", "artifact_rejected")
    );
    assert_eq!(
        harness
            .server
            .hooks()
            .guests_started
            .load(Ordering::Acquire),
        0
    );
    assert_eq!(harness.world.binds.load(Ordering::Acquire), 0);
    assert_eq!(harness.world.host_calls.load(Ordering::Acquire), 0);
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

/// `length` ASCII characters in which every eight-character window is distinct in practice: a
/// 64-bit generator over the 62 letters and digits.
fn pseudo_random_claim(length: usize, seed: u64) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut state = seed;
    (0..length)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ALPHABET[((state >> 33) % 62) as usize] as char
        })
        .collect()
}

#[test]
fn oversized_credential_claims_fail_closed_to_exactly_the_marker_within_a_bound_and_release_the_slot()
 {
    let harness = Harness::new(1);
    let message = "q".repeat(8192);
    let digest = harness.world.add(&logging(&message, 1000));
    // Two 1,000,000-character claims are 2 M distinct windows: past the 2^20 cap, and about
    // 4 MB of request once the projection repeats them.
    let secrets = vec![
        pseudo_random_claim(1_000_000, 1),
        pseudo_random_claim(1_000_000, 2),
    ];
    let spec = Spec::new(&digest).with(|s| {
        s.imports = vec!["logging"];
        s.max_host_calls = 2000;
        s.secrets = secrets;
    });
    let started = Instant::now();
    let reply = harness.run(&spec);
    let elapsed = started.elapsed();
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    // Fail closed: no line of the thousand reaches the result, only the marker.
    assert_eq!(reply.json()["logs"], json!(["<redacted>"]));
    // The whole request, parse and compile included, is bounded; a debug build gets more room.
    let bound = if cfg!(debug_assertions) { 60 } else { 5 };
    assert!(elapsed < Duration::from_secs(bound), "{elapsed:?}");
    // The slot is released, and the next request is admitted.
    assert!(harness.server.wait_running(0, BUDGET).is_some());
    let ok = harness.world.add(&success());
    assert_eq!(harness.run(&Spec::new(&ok)).status, 200);
}

#[test]
fn a_cancel_during_matcher_construction_releases_the_slot_within_the_budget() {
    let harness = Arc::new(Harness::new(2));
    let digest = harness.world.add(&success());
    // A valid credential with a large claim: building its matcher is real work.
    let spec = Spec::new(&digest).with(|s| s.secrets = vec![pseudo_random_claim(1_000_000, 3)]);
    let (reached_tx, reached) = mpsc::channel();
    let reached_tx = Mutex::new(reached_tx);
    let first = std::sync::atomic::AtomicBool::new(true);
    // The server's own event: the build asked for its first claim. It is held there until the
    // execution's cancel flag is set, so the cancel arrives while the matcher is being built.
    harness.server.hooks().on_build(move |cancelled| {
        if first.swap(false, Ordering::AcqRel) {
            let _ = reached_tx.lock().unwrap().send(());
            let limit = Instant::now() + LONG;
            while !cancelled.load(Ordering::Acquire) && Instant::now() < limit {
                std::thread::yield_now();
            }
        }
    });
    let guests = harness
        .server
        .hooks()
        .guests_started
        .load(Ordering::Acquire);
    let pending = harness.spawn(&spec);
    reached
        .recv_timeout(LONG)
        .expect("the matcher build was reached");
    let sent = Instant::now();
    let answer = harness.cancel(&spec.activation);
    assert_eq!(answer.status, 200);
    assert_eq!(answer.json()["cancel"]["state"], "acknowledged");
    assert!(
        harness
            .server
            .wait_running(0, BUDGET.saturating_sub(sent.elapsed()))
            .is_some(),
        "the slot was held while the matcher was being built"
    );
    assert!(sent.elapsed() < BUDGET, "{:?}", sent.elapsed());
    let reply = pending.join().unwrap();
    assert_eq!(
        code(&reply),
        (499, "cancelled".into(), "cancel_requested".into()),
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    // An interrupted build fails closed: only the marker, never text.
    assert_eq!(reply.json()["logs"], json!(["<redacted>"]));
    assert_eq!(
        harness
            .server
            .hooks()
            .guests_started
            .load(Ordering::Acquire),
        guests
    );
    // The next execute is admitted.
    let ok = harness
        .world
        .add(&guest(&[], 64, &[(1024, OK_JSON)], &ret(OK_JSON.len())));
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
    let (first, second) = (Spec::blocking(&digest), Spec::blocking(&digest));
    let (a, b) = (harness.spawn(&first), harness.spawn(&second));
    harness.wait_acted(2);
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
    assert_eq!(
        code(&a.join().unwrap()),
        (499, "cancelled".into(), "cancel_requested".into())
    );
    // The slot is free again within the budget, counted from before the cancel was sent.
    assert!(harness.server.wait_running(1, BUDGET).is_some());
    assert!(started.elapsed() < BUDGET, "{:?}", started.elapsed());
    // The other execution is untouched until it is cancelled in its turn.
    assert_eq!(harness.cancel(&second.activation).status, 200);
    assert_eq!(b.join().unwrap().status, 499);
    drop(held);
}

#[test]
fn a_duplicate_of_a_running_attempt_past_its_deadline_is_a_timeout_not_busy() {
    let harness = Arc::new(Harness::new(2));
    let digest = harness.world.add(&spin());
    let running = Spec::blocking(&digest);
    let first = harness.spawn(&running);
    harness.wait_acted(1);
    // The same activation and attempt, with a request deadline that has already passed.
    let expired = running.clone().with(|s| s.deadline_secs = -5);
    let reply = harness.run(&expired);
    assert_eq!(
        code(&reply),
        (504, "timeout".into(), "deadline_exceeded".into())
    );
    // Before its deadline the same duplicate is refused as busy, and the first is untouched.
    let live = running.clone();
    assert_eq!(
        code(&harness.run(&live)),
        (503, "busy".into(), "at_capacity".into())
    );
    assert_eq!(harness.cancel(&running.activation).status, 200);
    assert_eq!(first.join().unwrap().status, 499);
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
    let spec = Spec::blocking(&digest);
    let handle = harness.spawn(&spec);
    harness.wait_acted(1);
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
    let spec = Spec::blocking(&digest);
    let handle = harness.spawn(&spec);
    harness.wait_acted(1);
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
    let before = harness.world.actions.count.load(Ordering::Acquire);
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
    // The guest ran (its own first action reached the bindings), and the execution ended at
    // the shortest limit, within the budget.
    assert_eq!(
        harness.world.actions.count.load(Ordering::Acquire),
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
    harness.warm(&Spec::blocking(&digest));
    // The request's own deadline is the shortest (whole seconds: between 1 and 2 s away).
    let request = Spec::blocking(&digest).with(|s| s.deadline_secs = 2);
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
        &Spec::blocking(&digest).with(|s| s.timeout_ms = 700),
        Duration::from_millis(700),
    );
    // The worker's wall-clock limit is the shortest of all.
    timed(
        &harness,
        &Spec::blocking(&digest).with(|s| s.timeout_ms = 20_000),
        Duration::from_millis(2500),
    );
    let ok = harness.world.add(&success());
    assert_eq!(harness.run(&Spec::new(&ok)).status, 200);
}

#[test]
fn a_host_call_after_the_deadline_ends_the_execution_as_a_timeout() {
    let harness = Arc::new(Harness::new(1));
    let digest = harness.world.add(&calling());
    harness.warm(&Spec::new(&digest).with(|s| s.imports = vec!["context", "progress"]));
    timed(
        &harness,
        &Spec::new(&digest).with(|s| {
            s.imports = vec!["context", "progress"];
            s.timeout_ms = 600;
            s.max_host_calls = 1 << 50;
        }),
        Duration::from_millis(600),
    );
}

/// A guest that makes two progress reports one after the other. A report that the host cuts
/// off hands control back to the guest, which then makes the second.
fn two_reports() -> Vec<u8> {
    guest(
        &["progress"],
        64,
        &[(1024, OK_JSON)],
        &format!("{ANNOUNCE} {ANNOUNCE} {}", ret(OK_JSON.len())),
    )
}

#[test]
fn a_host_call_entered_after_until_through_the_linked_interface_never_reaches_the_host() {
    let harness = Arc::new(Harness::new(1));
    let digest = harness.world.add(&two_reports());
    // One call fits the budget, so the second would also exceed it.
    let spec = Spec::new(&digest).with(|s| {
        s.imports = vec!["progress"];
        s.timeout_ms = 1000;
        s.max_host_calls = 1;
    });
    // Control: with the binding answering at once, the guest does make its second call, which
    // the budget refuses before it reaches the host. The host saw exactly the first.
    let before = harness.world.host_calls.load(Ordering::Acquire);
    let reply = harness.run(&spec);
    assert_eq!(
        code(&reply),
        (
            500,
            "execution_failed".into(),
            "host_call_budget_exceeded".into()
        )
    );
    assert_eq!(harness.world.host_calls.load(Ordering::Acquire) - before, 1);
    // Late: the first call blocks in the binding until `until` cuts it off and returns control
    // to the guest, so the guest's second call is entered after `until` by construction.
    let release = harness.world.actions.block();
    let before = harness.world.host_calls.load(Ordering::Acquire);
    let started = Instant::now();
    let reply = harness.run(&Spec::new(&digest).with(|s| {
        s.imports = vec!["progress"];
        s.timeout_ms = 1000;
        s.max_host_calls = 1;
    }));
    let elapsed = started.elapsed();
    harness.world.actions.unblock();
    drop(release);
    // The deadline outranks the exhausted budget, and the host saw only the first call.
    assert_eq!(
        code(&reply),
        (504, "timeout".into(), "deadline_exceeded".into()),
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(harness.world.host_calls.load(Ordering::Acquire) - before, 1);
    assert!(
        elapsed < Duration::from_millis(1000) + BUDGET,
        "{elapsed:?}"
    );
    assert!(harness.server.wait_running(0, BUDGET).is_some());
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
    let reply = harness.run(&Spec::blocking(&looping).with(|s| s.fuel = 100_000));
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

// --- tests: a host call blocked across the deadline or a cancel --------------------------

#[test]
fn a_progress_call_blocked_across_until_is_a_504_timeout_that_stays_uncertain() {
    let harness = Arc::new(Harness::new(1));
    let digest = harness.world.add(&spin());
    harness.warm(&Spec::blocking(&digest));
    // From now on the binding blocks on its first report until the test lets go.
    let release = harness.world.actions.block();
    let spec = Spec::blocking(&digest).with(|s| s.timeout_ms = 1000);
    let started = Instant::now();
    let pending = harness.spawn(&spec);
    // The guest's first action reached the binding, and the binding does not return.
    harness.wait_acted(1);
    let reply = pending.join().unwrap();
    assert_eq!(
        code(&reply),
        (504, "timeout".into(), "deadline_exceeded".into()),
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(reply.json()["class"], "uncertain");
    assert!(
        started.elapsed() < Duration::from_millis(1000) + BUDGET,
        "{:?}",
        started.elapsed()
    );
    assert!(harness.server.wait_running(0, BUDGET).is_some());
    harness.world.actions.unblock();
    drop(release);
    // The slot is free: the next execute is admitted.
    let ok = harness.world.add(&success());
    assert_eq!(harness.run(&Spec::new(&ok)).status, 200);
}

#[test]
fn a_cancel_during_a_blocked_progress_call_is_a_499_cancelled_that_stays_uncertain() {
    let harness = Arc::new(Harness::new(1));
    let digest = harness.world.add(&spin());
    harness.warm(&Spec::blocking(&digest));
    let release = harness.world.actions.block();
    let spec = Spec::blocking(&digest);
    let pending = harness.spawn(&spec);
    harness.wait_acted(1);
    let sent = Instant::now();
    let answer = harness.cancel(&spec.activation);
    assert_eq!(answer.status, 200);
    assert!(
        harness
            .server
            .wait_running(0, BUDGET.saturating_sub(sent.elapsed()))
            .is_some()
    );
    assert!(sent.elapsed() < BUDGET, "{:?}", sent.elapsed());
    let reply = pending.join().unwrap();
    assert_eq!(
        code(&reply),
        (499, "cancelled".into(), "cancel_requested".into()),
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(reply.json()["class"], "uncertain");
    harness.world.actions.unblock();
    drop(release);
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
            // The clock starts before the cancel is sent and stops when the slot is free.
            let sent = Instant::now();
            let answer = harness.cancel(&spec.activation);
            assert_eq!(answer.status, 200, "{stall:?}");
            assert_eq!(
                answer.json()["cancel"]["state"],
                "acknowledged",
                "{stall:?}"
            );
            assert!(
                harness
                    .server
                    .wait_running(0, BUDGET.saturating_sub(sent.elapsed()))
                    .is_some(),
                "{stall:?}: slot held"
            );
            assert!(sent.elapsed() < BUDGET, "{stall:?}: {:?}", sent.elapsed());
            (499, "cancelled")
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
    let sent = Instant::now();
    assert_eq!(harness.cancel(&spec.activation).status, 200);
    assert!(
        harness
            .server
            .wait_running(0, BUDGET.saturating_sub(sent.elapsed()))
            .is_some()
    );
    assert!(sent.elapsed() < BUDGET, "{:?}", sent.elapsed());
    assert_eq!(pending.join().unwrap().status, 499);
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
    // Each client keeps reading, a few hundred bytes every ten milliseconds, so every write
    // the server makes progresses within a small fraction of the response deadline. A
    // deadline renewed per write would never expire for such a reader.
    let stop = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = mpsc::channel();
    let handles: Vec<_> = readers
        .into_iter()
        .map(|mut stream| {
            let stop = Arc::clone(&stop);
            let started = started_tx.clone();
            std::thread::spawn(move || {
                let mut chunk = [0u8; 256];
                let (mut total, mut reads) = (0usize, 0usize);
                stream.set_read_timeout(Some(LONG)).unwrap();
                while !stop.load(Ordering::Acquire) {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if reads == 0 {
                                let _ = started.send(());
                            }
                            total += count;
                            reads += 1;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                // Past the cut-off, what the server had queued is drained at full speed.
                let mut rest = Vec::new();
                let _ = stream.read_to_end(&mut rest);
                (reads, total + rest.len())
            })
        })
        .collect();
    drop(started_tx);
    // The response deadline runs from the first byte of the response written.
    for _ in 0..2 {
        started_rx.recv_timeout(LONG).expect("a response started");
    }
    // The permits are released at the one absolute response deadline, within the termination
    // budget, although both clients are still reading with continuing progress.
    assert!(
        harness
            .server
            .wait_connections((0, 0), Duration::from_millis(900) + BUDGET),
        "a slow reader held its permit past the absolute response deadline"
    );
    stop.store(true, Ordering::Release);
    for handle in handles {
        let (reads, total) = handle.join().unwrap();
        assert!(reads >= 3, "the client was not reading: {reads} reads");
        // The response was cut off: a maximum-size result carries over 8,000,000 bytes.
        assert!(
            total < 8_000_000,
            "the whole response was delivered: {total}"
        );
    }
    recovered(&harness, "slow readers");
}
