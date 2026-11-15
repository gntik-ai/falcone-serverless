//! Guests, request specs and a minimal HTTP client shared by the execution API suites
//! (`execute_api_local.rs` and `execute_api_production.rs`). Guests are compiled from WAT.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const WORKER_ENGINE: &str = "49.0.5";
/// The documented termination budget.
pub const BUDGET: Duration = Duration::from_millis(500);
/// Generous waits for things that must happen; they bound a hang, never a measurement.
pub const LONG: Duration = Duration::from_secs(60);

// --- guests -------------------------------------------------------------------------------

pub fn wat_bytes(text: &str) -> String {
    text.bytes().map(|b| format!("\\{b:02x}")).collect()
}

/// A handler component. `body` is the WAT body of `handle(ptr, len) -> i32`; the result area
/// is at 0. `data` are (offset, text) segments; `pages` is the initial memory in 64 KiB pages.
pub fn guest(imports: &[&str], pages: u32, data: &[(u32, &str)], body: &str) -> Vec<u8> {
    guest_with(imports, pages, data, "", "", body)
}

/// `guest` with `functions` (core functions beside `handle`) and `exports`, component-level
/// exports added to the handler export. Besides the Falcone interfaces, `imports` names the
/// prohibited WASI surfaces a guest could reach the environment or a file through
/// (`wasi-environment`, `wasi-preopens`, `wasi-filesystem`): they are imported but never used,
/// because the worker must refuse them by name before any guest code exists.
pub fn guest_with(
    imports: &[&str],
    pages: u32,
    data: &[(u32, &str)],
    functions: &str,
    exports: &str,
    body: &str,
) -> Vec<u8> {
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
            "wasi-environment" => {
                wat += r#"(import "wasi:cli/environment@0.2.6" (instance
                      (export "get-environment" (func (result (list (tuple string string)))))))"#;
            }
            "wasi-preopens" => {
                wat += r#"(import "wasi:filesystem/preopens@0.2.6" (instance
                      (export "get-directories" (func (result (list (tuple u32 string)))))))"#;
            }
            "wasi-filesystem" => wat += r#"(import "wasi:filesystem/types@0.2.6" (instance))"#,
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
        {functions}
        (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
          {body}))
      (core instance $host (export "memory" (memory $memory)) {core_args})
      (core instance $guest (instantiate $guest (with "host" (instance $host))))
      (func $handle (type $handle) (canon lift (core func $guest "handle")
        (memory $memory) (realloc (func $realloc)) string-encoding=utf8))
      (instance $handler (export "handler-error" (type $herror)) (export "handle" (func $handle)))
      (export "falcone:host/handler@0.2.0" (instance $handler))
      {exports})"#
    );
    wat::parse_str(&wat).unwrap_or_else(|error| panic!("{error}\n{wat}"))
}

pub const OK_JSON: &str = r#"{"statusCode":200,"body":{"ok":true}}"#;

/// Returns the static bytes at 1024 (the result area is at 0).
pub fn ret(len: usize) -> String {
    format!(
        "(i32.store (i32.const 4) (i32.const 1024)) (i32.store (i32.const 8) (i32.const {len})) (i32.const 0)"
    )
}

/// A guest that returns the legacy result and uses 4 MiB of memory.
pub fn success() -> Vec<u8> {
    guest(&[], 64, &[(1024, OK_JSON)], &ret(OK_JSON.len()))
}

/// The guest's first action: one progress report, which reaches the platform's bindings.
pub const ANNOUNCE: &str =
    "(call $report (i64.const 1) (i64.const 2) (i32.const 2048) (i32.const 0) (i32.const 3000))";

/// What a running spin guest logs, so that its result envelope proves that it ran.
pub const STARTED: &str = "started";

/// A guest that announces itself with a progress report, logs `STARTED` and never returns.
/// Its revision declares `logging` and `progress` (see `Spec::blocking`).
pub fn spin() -> Vec<u8> {
    guest(
        &["logging", "progress"],
        64,
        &[(4096, STARTED)],
        &format!(
            "{ANNOUNCE} (call $log (i32.const 1) (i32.const 4096) (i32.const {}) (i32.const 0) (i32.const 0) (i32.const 3000)) (loop $l (br $l)) {}",
            STARTED.len(),
            ret(0)
        ),
    )
}

/// A guest that logs `message` `times` times, then returns the legacy result.
pub fn logging(message: &str, times: u32) -> Vec<u8> {
    logging_then_returning(message, times, OK_JSON)
}

/// A guest that logs `message` `times` times, then returns `json` as its handler result.
pub fn logging_then_returning(message: &str, times: u32, json: &str) -> Vec<u8> {
    guest(
        &["logging"],
        64,
        &[(1024, json), (2048, message)],
        &format!(
            "(local $i i32)
             (loop $again
               (call $log (i32.const 1) (i32.const 2048) (i32.const {}) (i32.const 0) (i32.const 0) (i32.const 3000))
               (local.set $i (i32.add (local.get $i) (i32.const 1)))
               (br_if $again (i32.lt_u (local.get $i) (i32.const {times}))))
             {}",
            message.len(),
            ret(json.len())
        ),
    )
}

/// What `memory_probe` reports: one digit counts the secrets it found outside their own copy,
/// another the positive controls.
const PROBE_RESULT: &str = r#"{"statusCode":200,"body":{"leaked":"0","seen":"0"}}"#;

/// Where the digit of `name` sits in `PROBE_RESULT`.
fn digit_at(name: &str) -> usize {
    PROBE_RESULT.find(&format!("\"{name}\":\"")).unwrap() + name.len() + 4
}

/// The count of `needle` in the guest's whole linear memory, outside the needle's own copy.
const FIND: &str = "(func $find (param $needle i32) (param $len i32) (result i32)
    (local $p i32) (local $end i32) (local $k i32) (local $count i32)
    (local.set $end (i32.sub (i32.mul (memory.size) (i32.const 65536)) (local.get $len)))
    (block $done
      (loop $next
        (br_if $done (i32.gt_u (local.get $p) (local.get $end)))
        (local.set $k (i32.const 0))
        (block $miss
          (loop $compare
            (br_if $miss (i32.ne (i32.load8_u (i32.add (local.get $p) (local.get $k)))
                                 (i32.load8_u (i32.add (local.get $needle) (local.get $k)))))
            (local.set $k (i32.add (local.get $k) (i32.const 1)))
            (br_if $compare (i32.lt_u (local.get $k) (local.get $len))))
          (if (i32.ne (local.get $p) (local.get $needle))
            (then (local.set $count (i32.add (local.get $count) (i32.const 1))))))
        (local.set $p (i32.add (local.get $p) (i32.const 1)))
        (br $next)))
    (local.get $count))";

/// A guest that looks for credential material in everything it can see: its whole linear
/// memory, after it took what the host hands a guest (its params and the invocation key).
/// `secrets` are searched for and must be absent; `controls` are searched for and must be
/// present, which proves that the scan reads the memory that the host wrote. The result
/// reports both counts as `{"leaked":"<n>","seen":"<n>"}`, each count capped at 9.
pub fn memory_probe(secrets: &[&str], controls: &[&str]) -> Vec<u8> {
    let mut data: Vec<(u32, String)> = vec![(1024, PROBE_RESULT.to_owned())];
    let mut at = 8192;
    let mut calls = String::new();
    for (needles, counter) in [(secrets, "$leaked"), (controls, "$seen")] {
        for needle in needles {
            data.push((at, (*needle).to_owned()));
            calls += &format!(
                "(local.set {counter} (i32.add (local.get {counter}) (i32.gt_u (call $find (i32.const {at}) (i32.const {})) (i32.const 0))))",
                needle.len()
            );
            at += 256;
        }
    }
    let segments: Vec<(u32, &str)> = data.iter().map(|(at, text)| (*at, text.as_str())).collect();
    guest_with(
        &["context"],
        4,
        &segments,
        FIND,
        "",
        &format!(
            "(local $leaked i32) (local $seen i32)
             (call $key (i32.const 3000))
             {calls}
             (i32.store8 (i32.const {}) (i32.add (i32.const 48) (select (local.get $leaked) (i32.const 9) (i32.lt_u (local.get $leaked) (i32.const 9)))))
             (i32.store8 (i32.const {}) (i32.add (i32.const 48) (select (local.get $seen) (i32.const 9) (i32.lt_u (local.get $seen) (i32.const 9)))))
             {}",
            1024 + digit_at("leaked"),
            1024 + digit_at("seen"),
            ret(PROBE_RESULT.len())
        ),
    )
}

/// What `memory_probe` reported: (secrets found, controls found).
pub fn probe_report(result: &Value) -> (u8, u8) {
    let digit = |name: &str| {
        result["result"]["body"][name]
            .as_str()
            .and_then(|text| text.parse().ok())
            .unwrap_or_else(|| panic!("no {name} in {result}"))
    };
    (digit("leaked"), digit("seen"))
}

/// A guest that makes `calls` progress reports, which reach the platform's bindings.
pub fn reporting(calls: u32) -> Vec<u8> {
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
// --- requests -----------------------------------------------------------------------------

pub fn civil(seconds: u64) -> String {
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

/// The civil time `seconds` from now; a negative count is a deadline that has passed.
pub fn deadline_in(seconds: i64) -> String {
    civil(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_add_signed(seconds),
    )
}

pub static NEXT: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
pub struct Spec {
    pub digest: String,
    pub activation: String,
    pub imports: Vec<&'static str>,
    pub runtime_mode: &'static str,
    pub timeout_ms: u64,
    /// Seconds until the request's own absolute deadline (negative: already passed).
    pub deadline_secs: i64,
    pub memory_bytes: u64,
    pub fuel: u64,
    pub max_host_calls: u64,
    pub engine: &'static str,
    pub secrets: Vec<String>,
    pub base: bool,
}

impl Spec {
    /// A request for a guest that announces itself and blocks; its revision declares what it imports.
    pub fn blocking(digest: &str) -> Self {
        Self::new(digest).with(|s| s.imports = vec!["logging", "progress"])
    }

    pub fn new(digest: &str) -> Self {
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

    pub fn with(mut self, change: impl FnOnce(&mut Self)) -> Self {
        change(&mut self);
        self
    }

    pub fn request(&self) -> Value {
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
            "imports": self.imports.iter().map(|i| match *i {
                "context" => "falcone:host/invocation-context@0.2.0".to_owned(),
                "wasi-environment" => "wasi:cli/environment@0.2.6".to_owned(),
                "wasi-preopens" => "wasi:filesystem/preopens@0.2.6".to_owned(),
                "wasi-filesystem" => "wasi:filesystem/types@0.2.6".to_owned(),
                other => format!("falcone:host/{other}@0.2.0"),
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

pub struct Reply2 {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply2 {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&self.body)))
    }
}

pub fn read_reply(mut stream: TcpStream) -> Option<Reply2> {
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

pub fn connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).unwrap();
    stream.set_nodelay(true).unwrap();
    stream
}

/// One request in one write, then the whole response.
pub fn post(addr: SocketAddr, path: &str, body: &[u8]) -> Reply2 {
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

/// (status, code, reason) of a refusal or of a result envelope's failure.
pub fn code(reply: &Reply2) -> (u16, String, String) {
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
