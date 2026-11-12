//! RUN-01.t3 local tests of the worker execution API: request parsing, refusal ordering,
//! credential redaction and the HTTP service over loopback sockets.
//!
//! Guests run here only as the fast feedback loop (operator decision, 2026-11-08). The
//! restricted-pod cases of the same criteria are in `cluster_execute_api` and in
//! `qualification/execution_api.rs`.

use falcone_serverless_worker::api::{
    MARKER, MAX_LINE_CHARS, MAX_LINES, MAX_SECRET_BYTES, MAX_SECRETS, Redactor, parse_cancel,
    parse_request, parse_result,
};
use falcone_serverless_worker::server::{
    Artifacts, CONTROL_RESERVE, Config, HttpLimits, Running, Server,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The documented bound for redacting the adversarial log set. Debug builds run the
/// dependency matcher unoptimized, so they get a looser bound; the release bound is the one
/// the contract documents.
fn redaction_bound() -> Duration {
    if cfg!(debug_assertions) {
        Duration::from_secs(20)
    } else {
        Duration::from_secs(1)
    }
}

fn redact(secrets: &[&str], lines: &[&str]) -> Vec<String> {
    Redactor::new(secrets.iter().copied()).redact(lines.iter().map(|l| (*l).to_owned()).collect())
}

#[test]
fn every_secret_is_replaced_by_the_marker_on_every_line() {
    let out = redact(
        &["canary-credential-0001"],
        &[
            "token=canary-credential-0001 and again canary-credential-0001!",
            "nothing to hide",
        ],
    );
    assert_eq!(
        out,
        [
            format!("token={MARKER} and again {MARKER}!"),
            "nothing to hide".to_owned()
        ]
    );
}

#[test]
fn prefix_sharing_secrets_are_fully_redacted() {
    // The longer secret wins at the same start; no tail of it survives.
    let out = redact(&["abcd", "abcdef"], &["x abcdefg y abcd z"]);
    assert_eq!(out, [format!("x {MARKER}g y {MARKER} z")]);
}

#[test]
fn overlapping_secrets_are_fully_redacted() {
    // "abcdef" and "defghi" overlap in "def"; leftmost-longest alone would leave "ghi".
    let out = redact(&["abcdef", "defghi"], &["--abcdefghi--"]);
    assert_eq!(out, [format!("--{MARKER}--")]);
    // A chain of three, and an overlap that starts inside a longer secret.
    let out = redact(&["aaaa1", "1bbbb", "bbbb2"], &["aaaa1bbbb2"]);
    assert_eq!(out, [MARKER.to_owned()]);
    let out = redact(&["wxyz", "uvwxyz12"], &["uvwxyz12 wxyz"]);
    assert_eq!(out, [format!("{MARKER} {MARKER}")]);
}

#[test]
fn secrets_shorter_than_the_minimum_are_not_matched() {
    // A one-byte secret would redact every occurrence of the letter; it is not a credential.
    assert_eq!(redact(&["a", "ab"], &["abcabc"]), ["abcabc"]);
}

#[test]
fn each_line_is_cut_to_8192_chars_before_redaction() {
    let secret = "canary-credential-0002";
    let line = format!("{}{secret}", "x".repeat(MAX_LINE_CHARS + 10));
    let out = redact(&[secret], &[&line]);
    assert_eq!(out, ["x".repeat(MAX_LINE_CHARS)]);
    // Multi-byte characters are cut on a character boundary, at 8192 characters.
    let wide = "é".repeat(MAX_LINE_CHARS + 5);
    let out = redact(&[secret], &[&wide]);
    assert_eq!(out[0].chars().count(), MAX_LINE_CHARS);
}

#[test]
fn a_secret_that_straddles_the_cut_is_not_partly_shown() {
    let secret = "canary-credential-0003";
    // The secret starts 5 characters before the cut: only a prefix of it fits in 8192 chars.
    let line = format!("{}{secret}tail", "x".repeat(MAX_LINE_CHARS - 5));
    let out = redact(&[secret], &[&line]);
    assert_eq!(out.len(), 1);
    assert!(out[0].starts_with(&"x".repeat(MAX_LINE_CHARS - 5)));
    assert!(
        !out[0].contains("canary"),
        "a prefix of the secret survived"
    );
    assert!(out[0].chars().count() <= MAX_LINE_CHARS);
}

#[test]
fn shrinking_replacements_never_pull_text_past_the_cut_into_the_line() {
    // Two whole copies of A fill the 8192-char window; B starts right after the `x`, past the cut.
    // Replacing A twice shrinks the line, but nothing beyond the original cut may appear.
    // Neither secret overlaps itself, so each copy is one match.
    let a = format!("A{}", "a".repeat(MAX_SECRET_BYTES - 1));
    let b = format!("B{}", "b".repeat(MAX_SECRET_BYTES - 1));
    let line = format!("{a}{a}x{b}");
    let out = redact(&[&a, &b], &[&line]);
    assert_eq!(out, [format!("{MARKER}{MARKER}")]);
    // The same with a secret that straddles the cut: it is replaced whole, and what follows
    // the cut is still not shown.
    let line = format!("{}{a}yz{b}", "x".repeat(MAX_LINE_CHARS - 100));
    let out = redact(&[&a, &b], &[&line]);
    assert_eq!(
        out,
        [format!("{}{MARKER}", "x".repeat(MAX_LINE_CHARS - 100))]
    );
}

#[test]
fn logs_are_capped_at_1000_lines() {
    let lines: Vec<String> = (0..MAX_LINES + 50).map(|n| format!("line {n}")).collect();
    let out = Redactor::new(["canary-credential-0004"]).redact(lines);
    assert_eq!(out.len(), MAX_LINES);
    assert_eq!(out[MAX_LINES - 1], format!("line {}", MAX_LINES - 1));
}

#[test]
fn work_over_budget_yields_only_the_marker_never_unredacted_text() {
    // More distinct secrets than the set budget allows: the request could carry hundreds of
    // thousands. The logs are dropped and one marker line stands in for them.
    let secrets: Vec<String> = (0..MAX_SECRETS + 1)
        .map(|n| format!("secret-{n:08}"))
        .collect();
    let started = Instant::now();
    let out = Redactor::new(secrets.iter().map(String::as_str)).redact(vec![
        "secret-00000001 is here".into(),
        "and so is secret-00000002".into(),
    ]);
    assert_eq!(out, [MARKER]);
    assert!(started.elapsed() < redaction_bound());
    // A secret over the per-secret size is over budget too.
    let long = "k".repeat(MAX_SECRET_BYTES + 1);
    assert_eq!(redact(&[&long], &["plain line"]), [MARKER]);
    // Without any log line there is nothing to show.
    assert!(
        Redactor::new(secrets.iter().map(String::as_str))
            .redact(Vec::new())
            .is_empty()
    );
}

#[test]
fn many_matches_in_the_work_budget_fail_closed() {
    // One 4096-byte secret of repeated bytes against 1000 lines of the same byte: every probe
    // rescans the secret, so the total work is far over the fixed budget.
    let secret = "a".repeat(MAX_SECRET_BYTES);
    let line = "a".repeat(MAX_LINE_CHARS);
    let started = Instant::now();
    let out = Redactor::new([secret.as_str()]).redact(vec![line; MAX_LINES]);
    assert_eq!(out, [MARKER]);
    assert!(started.elapsed() < redaction_bound());
}

#[test]
fn the_maximum_secret_set_and_1000_maximum_lines_finish_within_the_bound() {
    let secrets: Vec<String> = (0..MAX_SECRETS)
        .map(|n| format!("distinct-secret-{n:010}"))
        .collect();
    // Lines of the maximum length, a few carrying secrets.
    let lines: Vec<String> = (0..MAX_LINES)
        .map(|n| {
            let mut line = "ordinary log text ".repeat(MAX_LINE_CHARS / 18);
            if n % 100 == 0 {
                line.replace_range(100..100 + secrets[n].len(), &secrets[n]);
            }
            line
        })
        .collect();
    let started = Instant::now();
    let out = Redactor::new(secrets.iter().map(String::as_str)).redact(lines);
    let elapsed = started.elapsed();
    assert!(elapsed < redaction_bound(), "took {elapsed:?}");
    assert_eq!(out.len(), MAX_LINES);
    for (n, line) in out.iter().enumerate() {
        assert!(!line.contains("distinct-secret-"), "line {n}");
        if n % 100 == 0 {
            assert!(line.contains(MARKER), "line {n}");
        }
    }
}

// --- the service over loopback sockets ------------------------------------------------------

const ECHO: &[u8] = include_bytes!("../src/qualification/execute_fixtures/handler.wasm");
const IMPORTS: &[u8] = include_bytes!("../src/qualification/execute_fixtures/api-imports.wasm");
const SPIN: &[u8] = include_bytes!("../src/qualification/execute_fixtures/api-spin.wasm");
const WASI: &[u8] = include_bytes!("../src/qualification/execute_fixtures/wasi-import.wasm");
const LOGS: &[u8] = include_bytes!("../src/qualification/execute_fixtures/api-logs.wasm");
const PROGRESS: &str = "falcone:host/progress@0.2.0";
const LOGGING: &str = "falcone:host/logging@0.2.0";
const CONTEXT: &str = "falcone:host/invocation-context@0.2.0";
const CANARY: &str = "canary-credential-0001";

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// The artifact store boundary: components by digest, counting every fetch.
struct Store {
    components: Vec<(String, Vec<u8>)>,
    fetches: AtomicUsize,
}

impl Artifacts for Store {
    fn component(&self, digest: &str) -> Option<Vec<u8>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.components
            .iter()
            .find(|(known, _)| known == digest)
            .map(|(_, bytes)| bytes.clone())
    }
}

struct Fixture {
    running: Option<Running>,
    store: Arc<Store>,
    directory: PathBuf,
    /// Executions `recovered` has run; the only guests a refusal-only test may have started.
    recoveries: AtomicUsize,
}

impl Fixture {
    fn start(concurrency: u32, http: HttpLimits) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "execute-api-local-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&directory).unwrap();
        let store = Arc::new(Store {
            components: [ECHO, IMPORTS, SPIN, WASI, LOGS]
                .iter()
                .map(|bytes| (digest(bytes), bytes.to_vec()))
                .collect(),
            fetches: AtomicUsize::new(0),
        });
        let mut config = Config::new(
            PathBuf::from(env!("CARGO_BIN_EXE_artifact-compile-child")),
            directory.join("cache"),
            store.clone(),
        );
        config.concurrency = concurrency;
        config.http = http;
        config.cancel_deadline = Duration::from_secs(5);
        let server = Server::bind("127.0.0.1:0".parse().unwrap(), config).unwrap();
        Self {
            running: Some(server.spawn()),
            store,
            directory,
            recoveries: AtomicUsize::new(0),
        }
    }

    /// No guest ran for anything but the recovery checks.
    fn no_guest_ran(&self) {
        let ran = self.running().instances();
        assert_eq!(ran, self.recoveries.load(Ordering::SeqCst), "a guest ran");
    }

    fn running(&self) -> &Running {
        self.running.as_ref().unwrap()
    }

    fn addr(&self) -> SocketAddr {
        self.running().addr()
    }

    fn post(&self, path: &str, body: &[u8]) -> Reply {
        post(self.addr(), path, body)
    }

    fn execute(&self, request: &Value) -> Reply {
        self.post("/v1/execute", &serde_json::to_vec(request).unwrap())
    }

    /// After any abuse: a normal execute and a cancel both succeed, and no slot is still held.
    fn recovered(&self) {
        let request = request(1000, ECHO, &[], None);
        self.recoveries.fetch_add(1, Ordering::SeqCst);
        let reply = self.execute(&request);
        assert_eq!(
            reply.status,
            200,
            "execute after the case: {}",
            reply.text()
        );
        let cancel = self.post("/v1/cancel", &cancel_body(&request, "requested"));
        assert_eq!(
            cancel.status,
            200,
            "cancel after the case: {}",
            cancel.text()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.running().slots() != (0, 0, 0) {
            assert!(Instant::now() < deadline, "a slot leaked");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.running.take());
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn request(n: u32, component: &[u8], imports: &[&str], base: Option<Value>) -> Value {
    let (activation, attempt, revision) = (
        format!("act-local-{n}"),
        format!("att-local-{n}"),
        "rev-local-0001",
    );
    let artifact = digest(component);
    let mut projection = json!({
        "revision": revision,
        "runtime_mode": "wasm",
        "artifact_digest": artifact,
        "component_digest": artifact,
        "language_profile": {"name": "profile-local", "version": "1"},
        "engine_version": "49.0.5",
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
        "version": "1",
        "type": "execute",
        "activation_id": activation,
        "attempt_id": attempt,
        "attempt_number": 1,
        "deadline": "2099-01-01T00:00:00Z",
        "invocation": {"invocation_key": format!("inv-local-{n}")},
        "credential": {
            "iss": "canary-issuer-0001", "aud": "canary-audience-0001",
            "tenant": "canary-tenant-0001", "workspace": "canary-workspace-0001",
            "pool_profile": "canary-pool-0001",
            "revision": revision, "activation": activation, "attempt": attempt,
            "iat": 1, "exp": 2,
            "allowed_capabilities": [{
                "family": "http",
                "interface": "falcone:host/http@0.1.0",
                "scope": {"hosts": [CANARY]}
            }]
        },
        "projection": projection,
        "params": {"__ow_path": "/v1/local", "name": "world"}
    })
}

fn cancel_body(request: &Value, state: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "version": "1", "type": "cancel",
        "activation_id": request["activation_id"], "attempt_id": request["attempt_id"],
        "cancel": {"state": state}
    }))
    .unwrap()
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.text()))
    }
}

fn read_reply(stream: &mut TcpStream) -> Option<Reply> {
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
    let status = head.split(' ').nth(1)?.parse().ok()?;
    Some(Reply {
        status,
        body: bytes[split + 4..].to_vec(),
    })
}

fn connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream
}

fn post(addr: SocketAddr, path: &str, body: &[u8]) -> Reply {
    let mut stream = connect(addr);
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: worker\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    read_reply(&mut stream).expect("a reply")
}

fn fast() -> HttpLimits {
    HttpLimits {
        header_deadline: Duration::from_millis(600),
        io_timeout: Duration::from_millis(300),
        body_deadline: Duration::from_millis(1500),
        ..HttpLimits::default()
    }
}

fn identity(result: &Value) -> &Value {
    &result["identity"]
}

#[test]
fn execute_returns_the_legacy_envelope_with_the_identity_of_its_projection() {
    let fixture = Fixture::start(2, HttpLimits::default());
    for (n, base) in [
        (1, None),
        (
            2,
            Some(
                json!({"kind": "catalog", "id": "base-local", "version": "1.0.0",
                "digest": format!("sha256:{}", "2".repeat(64))}),
            ),
        ),
    ] {
        let request = request(n, ECHO, &[], base.clone());
        let reply = fixture.execute(&request);
        assert_eq!(reply.status, 200, "{}", reply.text());
        let result = reply.json();
        // The result parses against the request it answers: identity, activation and attempt.
        let parsed = parse_request(&serde_json::to_vec(&request).unwrap()).unwrap();
        parse_result(&reply.body, &parsed).unwrap();
        assert_eq!(result["status"], "success");
        assert_eq!(result["activation_id"], request["activation_id"]);
        assert_eq!(result["attempt_id"], request["attempt_id"]);
        assert_eq!(result["attempt_number"], 1);
        assert_eq!(result["result"]["statusCode"], 200);
        // The echo handler returns its params: the legacy `main(params)` object.
        assert_eq!(result["result"]["body"], request["params"]);
        assert!(result["logs"].as_array().unwrap().is_empty());
        let identity = identity(&result);
        assert_eq!(identity["function_revision"], "rev-local-0001");
        assert_eq!(identity["runtime_mode"], "wasm");
        assert_eq!(
            identity["artifact_digest"],
            request["projection"]["artifact_digest"]
        );
        assert_eq!(identity["wasmtime_version"], "49.0.5");
        assert_eq!(
            identity["language_profile"],
            request["projection"]["language_profile"]
        );
        assert_eq!(identity["interface_version"], "0.2.0");
        match base {
            Some(base) => assert_eq!(identity["base_digest"], base["digest"]),
            None => assert!(identity.get("base_digest").is_none()),
        }
    }
}

#[test]
fn handler_output_maps_to_the_legacy_result() {
    use falcone_serverless_worker::api::HandlerResult;
    let body = |output: &Value| {
        let handler = HandlerResult::from_output(output.to_string().as_bytes())?;
        serde_json::to_value(&handler).ok()
    };
    // The legacy `{statusCode, headers?, body?}` result is taken as it is.
    assert_eq!(
        body(&json!({"statusCode": 201, "headers": {"x-a": "b"}, "body": {"ok": true}})),
        Some(json!({"statusCode": 201, "headers": {"x-a": "b"}, "body": {"ok": true}}))
    );
    // Any other JSON value is the body of a 200; a null leaves the body absent.
    assert_eq!(
        body(&json!({"a": 1})),
        Some(json!({"statusCode": 200, "body": {"a": 1}}))
    );
    assert_eq!(
        body(&json!([1, 2])),
        Some(json!({"statusCode": 200, "body": [1, 2]}))
    );
    assert_eq!(body(&json!(null)), Some(json!({"statusCode": 200})));
    // A result with a `statusCode` must be exactly the legacy result and a valid one.
    assert_eq!(body(&json!({"statusCode": 99})), None);
    assert_eq!(body(&json!({"statusCode": 600})), None);
    assert_eq!(body(&json!({"statusCode": 304, "body": 1})), None);
    assert_eq!(body(&json!({"statusCode": 200, "extra": 1})), None);
    assert_eq!(body(&json!({"statusCode": "200"})), None);
    assert!(HandlerResult::from_output(b"not json").is_none());
}

#[test]
fn a_handler_output_that_is_not_a_valid_result_fails_the_execution() {
    // The echo handler returns its params, and every params object carries `__ow_path`, so a
    // params object with a `statusCode` is a legacy result with an unknown field.
    let fixture = Fixture::start(1, HttpLimits::default());
    let mut request = request(3, ECHO, &[], None);
    request["params"] = json!({"__ow_path": "/x", "statusCode": 201});
    let reply = fixture.execute(&request);
    let result = reply.json();
    assert_eq!(reply.status, 500);
    assert_eq!(result["status"], "failure");
    assert_eq!(result["result"]["code"], "execution_failed");
    assert_eq!(
        result["result"]["detail"]["reason"],
        "invalid_handler_result"
    );
    assert_eq!(result["class"], "non_retryable");
    let parsed = parse_request(&serde_json::to_vec(&request).unwrap()).unwrap();
    parse_result(&reply.body, &parsed).unwrap();
}

#[test]
fn a_runtime_mode_other_than_wasm_is_refused_before_anything_else_runs() {
    let fixture = Fixture::start(1, HttpLimits::default());
    let byo = json!({"kind": "byo", "image_digest": format!("sha256:{}", "3".repeat(64)),
        "signer_identity": "signer-local", "attestation_ref": "attestation-local"});
    let mut cases = Vec::new();
    // An image-based revision and a value the schema does not know, each with a request that
    // would also fail every later check (no artifact, undeclared import, no path).
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
        let reply = fixture.execute(&case);
        assert_eq!(reply.status, 409, "{}", reply.text());
        let refusal = reply.json();
        let envelope = refusal.get("result").unwrap_or(&refusal);
        assert_eq!(envelope["code"], "runtime_mode_not_supported");
        assert_eq!(envelope["detail"]["reason"], "runtime_mode_not_wasm");
        assert_eq!(envelope["retryable"], false);
        let text = reply.text();
        // A result repeats the activation it answers; nothing else of the request.
        for value in [CANARY, "canary-", "signer-local", "wasm2", "OCI", "native"] {
            assert!(!text.contains(value), "the refusal carries {value}");
        }
    }
    fixture.no_guest_ran();
    assert_eq!(fixture.running().compiles(), 0, "a compile was started");
    assert_eq!(
        fixture.store.fetches.load(Ordering::SeqCst),
        0,
        "an artifact was fetched"
    );
    fixture.recovered();
}

#[test]
fn legacy_inputs_are_refused_in_the_contract_order_and_the_handler_is_not_invoked() {
    let fixture = Fixture::start(1, HttpLimits::default());
    let mut request = request(20, ECHO, &[], None);
    request["params"] =
        json!({"__ow_headers": {"content-type": "application/json"}, "__ow_body": "not json"});
    let expect = |request: &Value, status: u16, code: &str, reason: &str| {
        let reply = fixture.execute(request);
        assert_eq!(reply.status, status, "{}", reply.text());
        let result = reply.json();
        assert_eq!(result["result"]["code"], code);
        assert_eq!(result["result"]["detail"]["reason"], reason);
        assert_eq!(result["identity"]["runtime_mode"], "wasm");
    };
    // Without a path the first check fails, whatever else is wrong.
    expect(&request, 400, "invalid_request", "missing_path");
    request["params"]["__ow_path"] = json!("/x");
    expect(&request, 400, "invalid_request", "non_json_body");
    request["params"]["__ow_body"] = json!(format!("{{\"a\":\"{}\"", "x".repeat(1_048_576)));
    expect(&request, 413, "request_too_large", "body_too_large");
    fixture.no_guest_ran();
    assert_eq!(fixture.store.fetches.load(Ordering::SeqCst), 0);
    // A malformed message is a bodyless 400 and never echoes a value.
    let reply = fixture.post("/v1/execute", b"{\"canary_marker_text\": nope");
    assert_eq!((reply.status, reply.body.len()), (400, 0));
    fixture.recovered();
}

#[test]
fn only_declared_and_allow_listed_imports_run() {
    let fixture = Fixture::start(1, HttpLimits::default());
    let undeclared = |reply: Reply| {
        let result = reply.json();
        assert_eq!(reply.status, 422, "{}", result);
        assert_eq!(result["result"]["code"], "undeclared_import");
        assert_eq!(result["result"]["detail"]["reason"], "undeclared_import");
        assert_eq!(result["result"]["retryable"], false);
        assert_eq!(result["class"], "non_retryable");
        // The message carries no import name.
        assert!(!reply.text().contains("falcone:host/"), "{}", reply.text());
    };
    // (1) An ungated interface with `imports: []` ("empty means none").
    undeclared(fixture.execute(&request(30, IMPORTS, &[], None)));
    // (2) A non-empty list that misses one of the component's imports.
    undeclared(fixture.execute(&request(31, IMPORTS, &[LOGGING], None)));
    // (3) A declared import that is not allow-listed (ambient WASI).
    undeclared(fixture.execute(&request(32, WASI, &["wasi:cli/environment@0.2.0"], None)));
    assert_eq!(fixture.running().compiles(), 0, "a compile was started");
    fixture.no_guest_ran();
    // (4) Every import declared and allow-listed: it runs.
    let request = request(33, IMPORTS, &[LOGGING, CONTEXT], None);
    let reply = fixture.execute(&request);
    assert_eq!(reply.status, 200, "{}", reply.text());
    assert_eq!(reply.json()["status"], "success");
    assert_eq!(fixture.running().compiles(), 1);
    assert_eq!(fixture.running().instances(), 1);
    fixture.recovered();
}

#[test]
fn the_credential_is_redacted_from_logs_and_never_reaches_guest_memory() {
    let fixture = Fixture::start(1, HttpLimits::default());
    let mut request = request(40, IMPORTS, &[LOGGING, CONTEXT], None);
    // The guest logs its params, as a guest that was handed a credential could.
    request["params"] = json!({"__ow_path": "/x", "leak": CANARY, "again": "canary-tenant-0001"});
    let reply = fixture.execute(&request);
    assert_eq!(reply.status, 200, "{}", reply.text());
    let logs = reply.json()["logs"].as_array().unwrap().clone();
    assert_eq!(logs.len(), 2);
    let logs = serde_json::to_string(&logs).unwrap();
    for secret in [
        CANARY,
        "canary-tenant-0001",
        "canary-issuer-0001",
        "canary-pool-0001",
    ] {
        assert!(!logs.contains(secret), "{secret} is in the logs");
    }
    assert!(logs.contains(MARKER));
    // The memory dump holds only what the host wrote: the params. With params that do not
    // carry the credential, the dump holds no credential string either.
    let clean = self::request(41, IMPORTS, &[LOGGING, CONTEXT], None);
    let logs = fixture.execute(&clean).json()["logs"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(logs.len(), 2);
    for line in &logs {
        let line = line.as_str().unwrap();
        assert!(
            !line.contains(MARKER),
            "a credential string was in guest memory"
        );
        assert!(line.contains("__ow_path"), "the dump shows the params");
    }
    fixture.recovered();
}

/// An execute request whose guest logs its params 1000 times, with `hosts` distinct credential
/// strings and params of 9000 characters that carry the first of them.
fn adversarial_logging_request(n: u32, hosts: usize) -> (Value, Vec<String>) {
    let secrets: Vec<String> = (0..hosts)
        .map(|at| format!("adv-credential-{at:08}.example.invalid"))
        .collect();
    let mut request = request(n, LOGS, &[LOGGING], None);
    request["credential"]["allowed_capabilities"][0]["scope"]["hosts"] = json!(secrets);
    request["params"] = json!({"__ow_path": "/x", "leak": secrets[0], "pad": "p".repeat(9000)});
    (request, secrets)
}

#[test]
fn adversarial_logging_through_execute_is_redacted_within_the_bound_and_releases_the_slot() {
    let fixture = Fixture::start(1, HttpLimits::default());
    // The most distinct credential strings the redactor accepts, and 1000 lines of the maximum
    // length: nothing of any of them survives, and the work finishes within the bound.
    let (request, secrets) = adversarial_logging_request(120, 4000);
    let started = Instant::now();
    let reply = fixture.execute(&request);
    let elapsed = started.elapsed();
    assert_eq!(
        reply.status,
        200,
        "{}",
        &reply.text()[..200.min(reply.body.len())]
    );
    assert!(
        elapsed < redaction_bound() + Duration::from_secs(30),
        "{elapsed:?}"
    );
    let result = reply.json();
    let logs = result["logs"].as_array().unwrap();
    assert!(!logs.is_empty() && logs.len() <= MAX_LINES);
    for line in logs {
        let line = line.as_str().unwrap();
        assert!(line.chars().count() <= MAX_LINE_CHARS);
        assert!(
            !line.contains("adv-credential-"),
            "a credential string survived"
        );
        assert!(!line.contains(CANARY));
    }
    assert!(
        logs.iter()
            .any(|line| line.as_str() == Some(MARKER) || line.as_str().unwrap().contains(MARKER))
    );
    assert!(secrets.len() < MAX_SECRETS);
    // The slot was released: a normal execute and a cancel both succeed.
    fixture.recovered();
    // Past the secret count the redactor fails closed: the logs are one marker line.
    let (request, _) = adversarial_logging_request(121, MAX_SECRETS + 100);
    let reply = fixture.execute(&request);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["logs"], json!([MARKER]));
    fixture.recovered();
}

#[test]
fn no_failure_text_carries_a_credential_or_request_value() {
    let fixture = Fixture::start(1, HttpLimits::default());
    let mut bodies = Vec::new();
    let mut with = |mutate: &dyn Fn(&mut Value)| {
        let mut request = request(50, ECHO, &[], None);
        mutate(&mut request);
        bodies.push(fixture.execute(&request).text());
    };
    with(&|r| r["projection"]["engine_version"] = json!("48.0.0"));
    with(&|r| r["projection"]["artifact_digest"] = json!(format!("sha256:{}", "9".repeat(64))));
    with(&|r| r["deadline"] = json!("2001-01-01T00:00:00Z"));
    with(&|r| r["projection"]["runtime_mode"] = json!("oci"));
    with(&|r| r["params"] = json!({}));
    with(&|r| r["credential"]["attempt"] = json!("canary-mismatch-0001"));
    bodies.push(fixture.post("/v1/execute", b"{").text());
    bodies.push(fixture.post("/v1/cancel", b"{").text());
    bodies.push(fixture.post("/v1/nothing", b"{}").text());
    for body in bodies {
        for value in [CANARY, "canary-"] {
            assert!(!body.contains(value), "{value} is in: {body}");
        }
    }
    fixture.recovered();
}

#[test]
fn an_expired_deadline_or_a_pinned_engine_that_differs_fails_without_running() {
    let fixture = Fixture::start(1, HttpLimits::default());
    let mut expired = request(60, ECHO, &[], None);
    expired["deadline"] = json!("2001-01-01T00:00:00Z");
    let result = fixture.execute(&expired).json();
    assert_eq!(result["result"]["code"], "execution_failed");
    assert_eq!(result["result"]["detail"]["reason"], "deadline_exceeded");
    let mut other_engine = request(61, ECHO, &[], None);
    other_engine["projection"]["engine_version"] = json!("48.0.0");
    let result = fixture.execute(&other_engine).json();
    assert_eq!(result["result"]["detail"]["reason"], "engine_mismatch");
    let mut unknown = request(62, ECHO, &[], None);
    unknown["projection"]["artifact_digest"] = json!(format!("sha256:{}", "9".repeat(64)));
    let result = fixture.execute(&unknown).json();
    assert_eq!(result["result"]["detail"]["reason"], "artifact_unavailable");
    fixture.no_guest_ran();
    fixture.recovered();
}

#[test]
fn control_slots_survive_full_execute_load_and_an_extra_execute_gets_busy_at_once() {
    let concurrency = 2;
    let fixture = Fixture::start(concurrency, HttpLimits::default());
    // The reserve is what keeps a cancel possible: the connection limit is concurrency + it.
    assert_eq!(
        fixture.running().connection_limit(),
        concurrency as usize + CONTROL_RESERVE as usize
    );
    let spinners: Vec<_> = (0..concurrency)
        .map(|n| {
            let addr = fixture.addr();
            let request = request(70 + n, SPIN, &[PROGRESS], None);
            let handle = std::thread::spawn({
                let request = request.clone();
                move || post(addr, "/v1/execute", &serde_json::to_vec(&request).unwrap())
            });
            (request, handle)
        })
        .collect();
    // Each guest has reported that it runs: the slots below are held by running guests, not by
    // requests still being parsed, registered or compiled.
    for (request, _) in &spinners {
        wait_until_running(&fixture, request);
    }
    assert_eq!(fixture.running().slots().1, concurrency as usize);
    // Every execute slot is taken by a blocked guest. An extra execute is answered at once.
    let started = Instant::now();
    let extra = fixture.execute(&request(79, ECHO, &[], None));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "busy was not immediate"
    );
    assert_eq!(extra.status, 503);
    let busy = extra.json();
    assert_eq!(busy["code"], "busy");
    assert_eq!(busy["retryable"], true);
    // The connection limit is concurrency + reserve, and the busy reply held no slot.
    assert_eq!(fixture.running().slots().1, concurrency as usize);
    // Cancel is accepted while every execute slot is held, and takes effect within the deadline.
    let started = Instant::now();
    let cancel = fixture.post("/v1/cancel", &cancel_body(&spinners[0].0, "requested"));
    assert_eq!(cancel.status, 200, "{}", cancel.text());
    assert!(started.elapsed() < Duration::from_secs(5));
    let answer = parse_cancel(&cancel.body).unwrap();
    assert!(!answer.is_requested());
    assert_eq!(cancel.json()["cancel"]["state"], "acknowledged");
    let mut spinners = spinners.into_iter();
    let (_, first) = spinners.next().unwrap();
    let first = first.join().unwrap();
    assert_eq!(first.json()["result"]["detail"]["reason"], "cancelled");
    let (second_request, second) = spinners.next().unwrap();
    let cancel = fixture.post("/v1/cancel", &cancel_body(&second_request, "requested"));
    assert_eq!(cancel.json()["cancel"]["state"], "acknowledged");
    assert_eq!(
        second.join().unwrap().json()["result"]["detail"]["reason"],
        "cancelled"
    );
    fixture.recovered();
}

#[test]
fn a_cancel_for_an_attempt_that_is_not_running_is_acknowledged() {
    let fixture = Fixture::start(1, HttpLimits::default());
    let request = request(80, ECHO, &[], None);
    let reply = fixture.post("/v1/cancel", &cancel_body(&request, "requested"));
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["cancel"]["state"], "acknowledged");
    // Only the dispatcher's request is accepted; the worker's own states are not.
    for state in ["acknowledged", "uncertain"] {
        let reply = fixture.post("/v1/cancel", &cancel_body(&request, state));
        assert_eq!((reply.status, reply.body.len()), (400, 0), "{state}");
    }
}

#[test]
fn the_documented_http_ceilings_and_deadlines_are_the_defaults() {
    let limits = HttpLimits::default();
    assert_eq!(limits.max_headers, 16 * 1024);
    assert_eq!(limits.max_request, 8 * 1024 * 1024);
    assert_eq!(limits.header_deadline, Duration::from_secs(5));
    assert_eq!(limits.io_timeout, Duration::from_secs(5));
}

#[test]
fn oversized_headers_requests_and_action_bodies_are_refused_without_running_a_guest() {
    let fixture = Fixture::start(1, fast());
    let limits = fast();
    // Headers over 16 KiB: 431, the connection is closed.
    let mut stream = connect(fixture.addr());
    let head = format!(
        "POST /v1/execute HTTP/1.1\r\nContent-Length: 2\r\nX-Pad: {}\r\n\r\n{{}}",
        "a".repeat(limits.max_headers)
    );
    let _ = stream.write_all(head.as_bytes());
    let reply = read_reply(&mut stream);
    assert!(reply.as_ref().is_none_or(|reply| reply.status == 431));
    fixture.recovered();
    // A request over 8 MiB: 413 before its body is read.
    let mut stream = connect(fixture.addr());
    let head = format!(
        "POST /v1/execute HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        limits.max_request + 1
    );
    stream.write_all(head.as_bytes()).unwrap();
    let reply = read_reply(&mut stream).expect("a 413");
    assert_eq!(reply.status, 413);
    assert_eq!(reply.json()["code"], "request_too_large");
    fixture.recovered();
    // An action body over 1 MiB inside a request under 8 MiB: the contract refusal.
    let mut request = request(90, ECHO, &[], None);
    request["params"]["__ow_body"] = json!("x".repeat(1_048_577));
    let reply = fixture.execute(&request);
    assert_eq!(reply.status, 413);
    assert_eq!(reply.json()["result"]["detail"]["reason"], "body_too_large");
    // A control message over its own ceiling.
    let reply = fixture.post("/v1/cancel", &vec![b' '; 64 * 1024]);
    assert_eq!(reply.status, 413);
    fixture.no_guest_ran();
    fixture.recovered();
}

#[test]
fn silent_dripping_and_stalled_connections_are_closed_within_their_bounds() {
    let limits = fast();
    let fixture = Fixture::start(1, limits);
    // A connection that sends nothing is closed within the header deadline.
    let mut stream = connect(fixture.addr());
    let started = Instant::now();
    assert!(read_reply(&mut stream).is_none());
    assert!(started.elapsed() < limits.header_deadline + Duration::from_secs(2));
    fixture.recovered();
    // Header bytes dripped faster than the inactivity timeout still hit the absolute deadline.
    let mut stream = connect(fixture.addr());
    let mut dripper = stream.try_clone().unwrap();
    let started = Instant::now();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let drip = std::thread::spawn({
        let stop = stop.clone();
        move || {
            let _ = dripper.write_all(b"POST /v1/execute HTTP/1.1\r\n");
            let mut sent = 0;
            while !stop.load(Ordering::SeqCst) && started.elapsed() < limits.header_deadline * 3 {
                if dripper.write_all(b"X-A: b\r\n").is_err() {
                    break;
                }
                sent += 1;
                std::thread::sleep(limits.io_timeout / 3);
            }
            sent
        }
    });
    assert!(read_reply(&mut stream).is_none());
    let closed_after = started.elapsed();
    stop.store(true, Ordering::SeqCst);
    assert!(drip.join().unwrap() > 0);
    assert!(
        closed_after < limits.header_deadline + Duration::from_secs(2),
        "{closed_after:?}"
    );
    // Closed by the absolute header deadline, not by the shorter inactivity timeout.
    assert!(
        closed_after
            >= limits
                .header_deadline
                .saturating_sub(Duration::from_millis(100)),
        "{closed_after:?}"
    );
    fixture.recovered();
    // A body that stalls past the inactivity timeout is closed.
    let mut stream = connect(fixture.addr());
    let started = Instant::now();
    stream
        .write_all(b"POST /v1/execute HTTP/1.1\r\nContent-Length: 100\r\n\r\n{")
        .unwrap();
    assert!(read_reply(&mut stream).is_none());
    assert!(started.elapsed() < limits.io_timeout + Duration::from_secs(2));
    fixture.no_guest_ran();
    fixture.recovered();
}

#[test]
fn unsupported_requests_are_refused_by_status_only() {
    let fixture = Fixture::start(1, fast());
    let status = |bytes: &[u8]| {
        let mut stream = connect(fixture.addr());
        stream.write_all(bytes).unwrap();
        read_reply(&mut stream).map(|reply| (reply.status, reply.body.len()))
    };
    assert_eq!(status(b"GET /v1/execute HTTP/1.1\r\n\r\n"), Some((405, 0)));
    assert_eq!(
        status(b"POST /v1/other HTTP/1.1\r\nContent-Length: 0\r\n\r\n"),
        Some((404, 0))
    );
    assert_eq!(status(b"POST /v1/execute HTTP/1.1\r\n\r\n"), Some((411, 0)));
    assert_eq!(
        status(b"POST /v1/execute HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
        Some((501, 0))
    );
    assert_eq!(
        status(b"POST /v1/execute HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\n"),
        Some((400, 0))
    );
    assert_eq!(status(b"not http\r\n\r\n"), Some((400, 0)));
    fixture.recovered();
}

fn update_body(request: &Value, sequence: u64, kind: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "version": "1", "type": "update",
        "activation_id": request["activation_id"], "attempt_id": request["attempt_id"],
        "sequence": sequence, "update": {"kind": kind}
    }))
    .unwrap()
}

/// Blocks until the guest of `request` has run: the spinning guest reports progress 1 of 1 as its
/// first act, and the worker serves it through the update operation. Execute-slot counts say
/// only that a request was admitted, not that its guest runs; this is the guest's own signal.
fn wait_until_running(fixture: &Fixture, request: &Value) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let reply = fixture.post("/v1/update", &update_body(request, 1, "heartbeat"));
        if reply.status == 200 && reply.json()["update"]["kind"] == "progress" {
            assert_eq!(reply.json()["update"]["completed"], 1);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the guest did not report that it runs"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_running_attempt_answers_a_heartbeat_and_an_unknown_one_is_not_found() {
    use falcone_serverless_worker::api::parse_update;
    let fixture = Fixture::start(1, HttpLimits::default());
    let spinning = request(95, SPIN, &[PROGRESS], None);
    let run = std::thread::spawn({
        let addr = fixture.addr();
        let body = serde_json::to_vec(&spinning).unwrap();
        move || post(addr, "/v1/execute", &body)
    });
    wait_until_running(&fixture, &spinning);
    let reply = fixture.post("/v1/update", &update_body(&spinning, 7, "heartbeat"));
    assert_eq!(reply.status, 200, "{}", reply.text());
    parse_update(&reply.body).unwrap();
    let update = reply.json();
    // The guest's own report, with the sequence the poll asked for.
    assert_eq!(update["update"]["kind"], "progress");
    assert_eq!(update["update"]["completed"], 1);
    assert_eq!(update["update"]["total"], 1);
    assert_eq!(update["sequence"], 7);
    assert_eq!(update["activation_id"], spinning["activation_id"]);
    // Only a heartbeat is a poll; the worker's own kinds are not accepted.
    let reply = fixture.post("/v1/update", &update_body(&spinning, 1, "progress"));
    assert_eq!(reply.status, 400);
    let other = request(96, SPIN, &[PROGRESS], None);
    let reply = fixture.post("/v1/update", &update_body(&other, 1, "heartbeat"));
    assert_eq!((reply.status, reply.body.len()), (404, 0));
    let cancel = fixture.post("/v1/cancel", &cancel_body(&spinning, "requested"));
    assert_eq!(cancel.json()["cancel"]["state"], "acknowledged");
    let ended = run.join().unwrap();
    assert_eq!(ended.status, 500);
    assert_eq!(ended.json()["result"]["detail"]["reason"], "cancelled");
    fixture.recovered();
}

/// A small deterministic generator for the mutation test: no external randomness, so a failure
/// reproduces.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % bound
    }
}

/// Every path of a JSON document, for choosing a field to break.
fn paths(value: &Value, prefix: String, out: &mut Vec<String>) {
    out.push(prefix.clone());
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                paths(child, format!("{prefix}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (at, child) in items.iter().enumerate() {
                paths(child, format!("{prefix}/{at}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn mutated_requests_never_print_or_echo_a_credential_string() {
    // Break one field at a time of a valid request, in several ways, and check every error
    // path: the parser's rejection text, and the service's reply to a sample of them.
    let base = request(100, ECHO, &[], None);
    let mut all = Vec::new();
    paths(&base, String::new(), &mut all);
    let mut random = Lcg(0x5eed);
    let fixture = Fixture::start(1, HttpLimits::default());
    let mut served = 0;
    for round in 0..400 {
        let mut request = base.clone();
        let path = all[random.next(all.len())].clone();
        if path.is_empty() {
            continue;
        }
        let new = match random.next(5) {
            0 => json!(CANARY),
            1 => json!(null),
            2 => json!(random.next(1 << 20)),
            3 => json!([CANARY]),
            _ => json!({"canary_unknown": CANARY}),
        };
        if let Some(slot) = request.pointer_mut(&path) {
            if random.next(4) == 0 {
                // Remove it instead of replacing it.
                let (parent, key) = path.rsplit_once('/').unwrap();
                match request.pointer_mut(parent) {
                    Some(Value::Object(map)) => {
                        map.remove(key);
                    }
                    _ => *request.pointer_mut(&path).unwrap() = new,
                }
            } else {
                *slot = new;
            }
        }
        let bytes = serde_json::to_vec(&request).unwrap();
        match parse_request(&bytes) {
            Ok(_) => {}
            Err(error) => {
                let printed = format!("{error:?}|{error}");
                assert!(
                    !printed.contains(CANARY) && !printed.contains("canary_"),
                    "{path}: {printed}"
                );
            }
        }
        // The credential strings of the original request never show in any reply either.
        if round % 8 == 0 {
            served += 1;
            let reply = fixture.post("/v1/execute", &bytes);
            let text = reply.text();
            let credential = &request["credential"];
            for secret in [
                "canary-issuer-0001",
                "canary-audience-0001",
                "canary-tenant-0001",
                CANARY,
            ] {
                // A mutation may have put the canary into a field a result repeats on purpose
                // (the identity); it never reaches the message or the detail.
                if credential.to_string().contains(secret) && !text.contains("\"identity\"") {
                    assert!(!text.contains(secret), "{path}: {secret} in {text}");
                }
            }
            if let Ok(result) = serde_json::from_str::<Value>(&text) {
                let failure = result.get("result").unwrap_or(&result);
                for field in ["message", "detail"] {
                    let shown = failure.get(field).map(Value::to_string).unwrap_or_default();
                    assert!(!shown.contains("canary"), "{path}: {field} {shown}");
                }
            }
        }
    }
    assert!(served >= 40);
    fixture.recovered();
}

#[test]
fn the_serve_subcommand_serves_the_api_from_a_config_and_an_artifact_directory() {
    let root = std::env::temp_dir().join(format!("execute-api-serve-{}", std::process::id()));
    let artifacts = root.join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    std::fs::write(
        artifacts.join(format!(
            "{}.wasm",
            digest(ECHO).strip_prefix("sha256:").unwrap()
        )),
        ECHO,
    )
    .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = root.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "pool_id = \"pool-local\"\nlisten_port = {port}\nconcurrency = 2\n\
             cache_path = \"{}\"\nfalcone_api_endpoint = \"https://api.invalid\"\n\
             falcone_dispatcher_endpoint = \"https://dispatcher.invalid\"\n",
            root.join("cache").display()
        ),
    )
    .unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("serve")
        .arg(&config)
        .arg(&artifacts)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(addr).is_err() {
        assert!(Instant::now() < deadline, "the worker did not listen");
        std::thread::sleep(Duration::from_millis(50));
    }
    let request = request(110, ECHO, &[], None);
    let reply = post(addr, "/v1/execute", &serde_json::to_vec(&request).unwrap());
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(reply.status, 200, "{}", reply.text());
    assert_eq!(reply.json()["result"]["body"], request["params"]);
}

#[test]
fn serve_with_a_bad_config_or_a_missing_argument_fails_without_listening() {
    let worker = env!("CARGO_BIN_EXE_falcone-serverless-worker");
    for args in [
        vec!["serve"],
        vec!["serve", "/nonexistent/worker.toml", "/nonexistent"],
    ] {
        let output = std::process::Command::new(worker)
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
    }
}

/// The cases of the restricted-pod qualification, run here as the fast feedback loop: the same
/// binary, the same service and the same production limits, on loopback outside a pod.
const POD_CASES: [&str; 14] = [
    "identity",
    "non-wasm",
    "legacy",
    "credential",
    "imports",
    "saturation",
    "http-headers",
    "http-request",
    "http-action-body",
    "http-silent",
    "http-drip",
    "http-stall",
    "http-control-body",
    "http-body-drip",
];

#[test]
fn the_pod_qualification_cases_pass_locally_and_record_their_evidence() {
    let handles: Vec<_> = POD_CASES
        .iter()
        .map(|case| {
            let case = *case;
            std::thread::spawn(move || {
                let output =
                    std::process::Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
                        .arg(format!("--execute-api={case}"))
                        .output()
                        .unwrap();
                (
                    case,
                    output.status.success(),
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                )
            })
        })
        .collect();
    for handle in handles {
        let (case, passed, stdout) = handle.join().unwrap();
        assert!(passed, "{case}: {stdout}");
        let records: Vec<Value> = stdout
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let group = if case == "saturation" {
            "saturation"
        } else if case.starts_with("http-") {
            "http"
        } else {
            "api"
        };
        assert_eq!(records[0]["case"], case);
        assert_eq!(records[0]["group"], group);
        assert_eq!(records[0]["status"], "passed");
        assert_eq!(
            records[1],
            json!({"check": "execute_api", "status": "passed"})
        );
    }
}

#[test]
fn an_unknown_pod_case_fails() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
        .arg("--execute-api=nope")
        .output()
        .unwrap();
    assert!(!output.status.success());
}
