//! Standalone OCI v1 conformance boundary; not connected to the execution worker.
//!
//! # What this tester is
//!
//! A conformance check, not a security boundary. It must catch buggy or misconfigured
//! candidates reliably and it fails closed on everything it observes: a listener other
//! than `127.0.0.1:$PORT` that is not proven to belong to a process outside the candidate
//! tree, an unreadable or malformed `/proc` value, a tree or socket table that changes
//! between the sample and its final consistency re-read, and a candidate it cannot kill
//! and reap. One absolute deadline covers every phase of a process-mode check; the only
//! wait that may outlive it is the final reap after SIGKILL, bounded by [`KILL_SETTLE`].
//! Signals go only through pidfds, never to a numeric PID or process group, and a pidfd is
//! kept only after the process is seen to still be a child of its tree parent (or of the
//! tester) once the pidfd is open, so a reused PID is released unsignalled and rescanned. Scratch removal runs once, inside that deadline; when the
//! deadline passes first, the scratch directory is left behind rather than removed on a new
//! budget by a destructor.
//!
//! # Residual risk (not detected here)
//!
//! * A candidate that deliberately evades sampling: processes in its tree that signal,
//!   resume or hand sockets to each other so that the tester never sees the forbidden
//!   listener open, or that behave while tested and misbehave later.
//! * A process outside the candidate tree that cooperates with it: it can SIGSTOP/SIGCONT
//!   or trace the candidate or pass it file descriptors, or open the forbidden listener
//!   itself. Processes outside the tree are platform components and are trusted.
//!
//! Runtime enforcement is the boundary against both: the restricted pod, the seccomp
//! RuntimeDefault profile and the NetworkPolicy.
//!
//! # Process ownership
//!
//! The tester makes itself the child subreaper and treats every child of the process as
//! part of the candidate tree. Run one check per process; the CLI does, and tests that
//! spawn processes in their own process must serialise.
use rustix::process::Signal;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DOC: &str = include_str!("oci_contract_assets/README.md");
pub const CONTEXT_SCHEMA: &str = include_str!("oci_contract_assets/context.schema.json");
pub const REQUEST_SCHEMA: &str = include_str!("oci_contract_assets/request.schema.json");
pub const ENVELOPE_SCHEMA: &str = include_str!("oci_contract_assets/envelope.schema.json");
pub struct Fixture {
    pub path: &'static str,
    pub schema: &'static str,
    pub rule: Option<&'static str>,
    pub bytes: &'static [u8],
}
pub const FIXTURES: &[Fixture] = &[
    Fixture {
        path: "fixtures/invalid/deadline.json",
        schema: "context",
        rule: Some("OCI-V1-REQUEST"),
        bytes: include_bytes!("oci_contract_assets/fixtures/invalid/deadline.json"),
    },
    Fixture {
        path: "fixtures/invalid/extra-context-field.json",
        schema: "context",
        rule: Some("OCI-V1-REQUEST"),
        bytes: include_bytes!("oci_contract_assets/fixtures/invalid/extra-context-field.json"),
    },
    Fixture {
        path: "fixtures/invalid/logs-type.json",
        schema: "envelope",
        rule: Some("OCI-V1-ENVELOPE"),
        bytes: include_bytes!("oci_contract_assets/fixtures/invalid/logs-type.json"),
    },
    Fixture {
        path: "fixtures/invalid/missing-context.json",
        schema: "request",
        rule: Some("OCI-V1-REQUEST"),
        bytes: include_bytes!("oci_contract_assets/fixtures/invalid/missing-context.json"),
    },
    Fixture {
        path: "fixtures/invalid/missing-result.json",
        schema: "envelope",
        rule: Some("OCI-V1-ENVELOPE"),
        bytes: include_bytes!("oci_contract_assets/fixtures/invalid/missing-result.json"),
    },
    Fixture {
        path: "fixtures/invalid/status.json",
        schema: "envelope",
        rule: Some("OCI-V1-STATUS"),
        bytes: include_bytes!("oci_contract_assets/fixtures/invalid/status.json"),
    },
    Fixture {
        path: "fixtures/legacy/error.json",
        schema: "envelope",
        rule: None,
        bytes: include_bytes!("oci_contract_assets/fixtures/legacy/error.json"),
    },
    Fixture {
        path: "fixtures/legacy/null.json",
        schema: "envelope",
        rule: None,
        bytes: include_bytes!("oci_contract_assets/fixtures/legacy/null.json"),
    },
    Fixture {
        path: "fixtures/legacy/success.json",
        schema: "envelope",
        rule: None,
        bytes: include_bytes!("oci_contract_assets/fixtures/legacy/success.json"),
    },
    Fixture {
        path: "fixtures/valid/context.json",
        schema: "context",
        rule: None,
        bytes: include_bytes!("oci_contract_assets/fixtures/valid/context.json"),
    },
    Fixture {
        path: "fixtures/valid/envelope.json",
        schema: "envelope",
        rule: None,
        bytes: include_bytes!("oci_contract_assets/fixtures/valid/envelope.json"),
    },
    Fixture {
        path: "fixtures/valid/request.json",
        schema: "request",
        rule: None,
        bytes: include_bytes!("oci_contract_assets/fixtures/valid/request.json"),
    },
];
/// Only registered rules can reach the tester's diagnostic output.
#[derive(Debug, Default)]
pub struct Diagnostics(BTreeMap<&'static str, String>);

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, rule: &'static str, message: impl Into<String>) {
        assert!(
            RULE_IDS.contains(&rule),
            "unknown OCI contract rule: {rule}"
        );
        self.0.insert(rule, message.into());
    }

    fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    fn report(self) -> i32 {
        for (rule, message) in &self.0 {
            println!("{rule}: {message}");
        }
        i32::from(!self.0.is_empty())
    }
}

// Expose map inspection without allowing unchecked mutation.
impl std::ops::Deref for Diagnostics {
    type Target = BTreeMap<&'static str, String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub fn check_request(value: &Value) -> Diagnostics {
    let schema: Value = serde_json::from_str(REQUEST_SCHEMA).expect("embedded request schema");
    let mut diagnostics = Diagnostics::new();
    if !shape(value, &schema, &schema, true) {
        diagnostics.insert("OCI-V1-REQUEST", "request/context does not match v1 schema");
    }
    if value.get("params").is_some_and(|params| {
        serde_json::to_vec(params).unwrap().len() as u64
            > schema["properties"]["params"]["x-maxSerializedBytes"]
                .as_u64()
                .unwrap()
    }) {
        diagnostics.insert("OCI-V1-PARAMS-LIMIT", "params exceeds 6 MB");
    }
    diagnostics
}
pub fn check_envelope(value: &Value) -> Diagnostics {
    let mut schema: Value =
        serde_json::from_str(ENVELOPE_SCHEMA).expect("embedded envelope schema");
    // These have their own diagnostics; structural failures must not cascade.
    schema["properties"]["status"]
        .as_object_mut()
        .unwrap()
        .remove("enum");
    schema["properties"]["logs"]
        .as_object_mut()
        .unwrap()
        .remove("maxLength");
    let mut diagnostics = Diagnostics::new();
    if !shape(value, &schema, &schema, true) {
        diagnostics.insert("OCI-V1-ENVELOPE", "envelope does not match v1 shape");
        return diagnostics;
    }
    let full: Value = serde_json::from_str(ENVELOPE_SCHEMA).unwrap();
    if serde_json::to_vec(&value["result"]).unwrap().len() as u64
        > full["properties"]["result"]["x-maxSerializedBytes"]
            .as_u64()
            .unwrap()
    {
        diagnostics.insert("OCI-V1-RESULT-LIMIT", "result exceeds 6 MB");
    }
    let logs = value["logs"].as_str().unwrap();
    let max_logs = full["properties"]["logs"]["x-maxUtf8Bytes"]
        .as_u64()
        .unwrap() as usize;
    if logs.len() > max_logs {
        diagnostics.insert("OCI-V1-LOGS-LIMIT", "logs exceeds 256 KB");
    } else if logs.len() == max_logs
        && !logs.ends_with(
            full["properties"]["logs"]["x-truncationMarker"]
                .as_str()
                .unwrap(),
        )
    {
        diagnostics.insert(
            "OCI-V1-LOGS-TRUNCATION",
            "full-budget logs lacks truncation marker",
        );
    }
    if !full["properties"]["status"]["enum"]
        .as_array()
        .unwrap()
        .contains(&value["status"])
    {
        diagnostics.insert("OCI-V1-STATUS", "unknown envelope status");
    }
    diagnostics
}
pub fn request(params: Value) -> Value {
    request_with_deadline(params, "2026-10-31T12:00:00Z")
}
pub fn request_with_deadline(params: Value, deadline: &str) -> Value {
    json!({"params":params,"context":{"handle":"opaque-attempt", "invocationId":"invocation-1", "deadline":deadline}})
}

/// Deadlines the contract allows: a candidate must accept every one of them.
pub const VALID_DEADLINES: &[&str] = &[
    "2026-10-31T12:00:00Z",
    "2026-10-31T12:00:00.5Z",
    "2026-10-31T12:00:00.123456789Z",
    "2000-02-29T00:00:00Z",
    "2026-12-31T23:59:60Z",
];
/// Deadlines the contract forbids: a candidate must reject every one with HTTP 400.
pub const MALFORMED_DEADLINES: &[&str] = &[
    "2026-10-31T12:00:00+01:00",
    "2026-10-31T12:00:00",
    "2026-10-31T12:00:00.Z",
    "2026-10-31T12:00:00.12345678901Z",
    "2026-13-31T12:00:00Z",
    "2026-02-30T00:00:00Z",
    "1900-02-29T00:00:00Z",
];

// A deliberately bounded evaluator for the structural subset of the three embedded
// schemas. Wire byte budgets have separate rule IDs, not JSON Schema semantics.
fn shape(value: &Value, schema: &Value, root: &Value, strings: bool) -> bool {
    if schema["$ref"] == "#/$defs/context" {
        return shape(value, &root["$defs"]["context"], root, strings);
    }
    match schema["type"].as_str() {
        Some("object") => {
            let Some(object) = value.as_object() else {
                return false;
            };
            let properties = schema["properties"].as_object().unwrap();
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .all(|key| object.contains_key(key.as_str().unwrap()))
                && object.iter().all(|(key, value)| {
                    properties
                        .get(key)
                        .is_some_and(|s| shape(value, s, root, strings))
                })
        }
        Some("string") => {
            let Some(text) = value.as_str() else {
                return false;
            };
            let length = text.chars().count() as u64;
            (!strings
                || (schema["minLength"].as_u64().is_none_or(|min| length >= min)
                    && schema["maxLength"].as_u64().is_none_or(|max| length <= max)))
                && (schema["format"] != "date-time" || utc_deadline(text))
                && schema
                    .get("enum")
                    .is_none_or(|options| options.as_array().unwrap().contains(value))
        }
        None => true,
        _ => false,
    }
}

pub fn utc_deadline(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || !bytes.is_ascii()
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || !text.ends_with('Z')
    {
        return false;
    }
    let number = |start, end| {
        text[start..end]
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| text[start..end].parse::<u32>().ok())
            .flatten()
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    let fraction = &bytes[19..bytes.len() - 1];
    day > 0
        && day <= days
        && hour < 24
        && minute < 60
        && (second < 60 || (second == 60 && hour == 23 && minute == 59))
        && (fraction.is_empty()
            || (fraction[0] == b'.'
                && (2..=10).contains(&fraction.len())
                && fraction[1..].iter().all(u8::is_ascii_digit)))
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}
pub fn read_response(reader: &mut impl std::io::Read) -> std::io::Result<Response> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid or oversized HTTP/1.1 response",
        )
    };
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 16_384 {
            return Err(invalid());
        }
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    let header = std::str::from_utf8(&header).map_err(|_| invalid())?;
    let mut lines = header.split("\r\n");
    let mut first = lines.next().ok_or_else(invalid)?.splitn(3, ' ');
    if first.next() != Some("HTTP/1.1") {
        return Err(invalid());
    }
    let code = first.next().ok_or_else(invalid)?;
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let status = code.parse::<u16>().map_err(|_| invalid())?;
    if !(100..=599).contains(&status) || first.next().is_none() {
        return Err(invalid());
    }
    let mut length = None;
    let mut content_type = None;
    for line in lines.take_while(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').ok_or_else(invalid)?;
        let value = value.trim();
        match key.to_ascii_lowercase().as_str() {
            "content-length" => {
                if length.is_some()
                    || value.is_empty()
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(invalid());
                }
                length = Some(value.parse::<usize>().map_err(|_| invalid())?);
            }
            "content-type" => {
                if content_type.is_some() {
                    return Err(invalid());
                }
                content_type = Some(value.to_owned());
            }
            "transfer-encoding" => return Err(invalid()),
            _ => {}
        }
    }
    let length = length.filter(|n| *n <= 8_000_000).ok_or_else(invalid)?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Response {
        status,
        content_type: content_type.unwrap_or_default(),
        body,
    })
}

use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::{DirBuilderExt, MetadataExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

pub const RULE_IDS: &[&str] = &[
    "OCI-V1-HEALTHZ",
    "OCI-V1-INVOKE",
    "OCI-V1-REQUEST",
    "OCI-V1-ENVELOPE",
    "OCI-V1-STATUS",
    "OCI-V1-PARAMS-LIMIT",
    "OCI-V1-RESULT-LIMIT",
    "OCI-V1-LOGS-LIMIT",
    "OCI-V1-LOGS-TRUNCATION",
    "OCI-V1-PORT",
    "OCI-V1-BINDING",
    "OCI-V1-SIGTERM",
    "OCI-V1-FILESYSTEM",
];
const IO_BUDGET: Duration = Duration::from_secs(2);
const STARTUP_BUDGET: Duration = Duration::from_secs(3);
/// The single absolute budget of one process-mode check.
pub const CHECK_BUDGET: Duration = Duration::from_secs(90);
pub const TERM_GRACE: Duration = Duration::from_secs(10);
/// The only wait that may end after the deadline: reaping what SIGKILL was sent to.
pub const KILL_SETTLE: Duration = Duration::from_secs(1);
/// Full samples of the process tree before the binding check gives up as unstable.
pub const MAX_SCANS: usize = 5;
/// Time to confirm a freshly opened handle once the deadline has already passed.
const CONFIRM_GRACE: Duration = Duration::from_millis(100);

fn deadline_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "conformance deadline exceeded")
}
fn ensure(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(deadline_error())
    } else {
        Ok(())
    }
}

struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
}
impl DeadlineStream {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "HTTP deadline exceeded"))
    }
}
impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}
impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn exchange(
    port: u16,
    method: &str,
    path: &str,
    body: &[u8],
    content_type: &str,
    deadline: Instant,
) -> io::Result<Response> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "HTTP deadline exceeded"))?;
    let mut stream = DeadlineStream {
        stream: TcpStream::connect_timeout(&address, remaining)?,
        deadline,
    };
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    read_response(&mut stream)
}

fn inspect_response(response: Response, diagnostics: &mut Diagnostics) {
    if response.status != 200 {
        diagnostics.insert("OCI-V1-INVOKE", "POST /invoke did not return HTTP 200");
        return;
    }
    let json_type = response
        .content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/json");
    match serde_json::from_slice::<Value>(&response.body) {
        Ok(value) if json_type => diagnostics.extend(check_envelope(&value)),
        _ => {
            diagnostics.insert(
                "OCI-V1-ENVELOPE",
                "response is not an application/json envelope",
            );
        }
    }
}

pub fn check_http(port: u16) -> Diagnostics {
    check_http_until(port, Instant::now() + CHECK_BUDGET)
}

pub fn check_http_until(port: u16, deadline: Instant) -> Diagnostics {
    let probe = |method: &str, path: &str, body: &[u8], content_type: &str| {
        exchange(
            port,
            method,
            path,
            body,
            content_type,
            deadline.min(Instant::now() + IO_BUDGET),
        )
    };
    let mut diagnostics = Diagnostics::new();
    let startup = (Instant::now() + STARTUP_BUDGET).min(deadline);
    loop {
        let response = exchange(
            port,
            "GET",
            "/healthz",
            b"",
            "application/json",
            startup.min(Instant::now() + IO_BUDGET),
        );
        if response.is_ok_and(|response| response.status == 200) {
            break;
        }
        if Instant::now() >= startup {
            diagnostics.insert(
                "OCI-V1-HEALTHZ",
                "GET /healthz failed within startup/HTTP budget",
            );
            return diagnostics;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let payload = serde_json::to_vec(&request(json!({"message":"hello"}))).unwrap();
    match probe("POST", "/invoke", &payload, "application/json") {
        Ok(response) => inspect_response(response, &mut diagnostics),
        Err(_) => {
            diagnostics.insert("OCI-V1-INVOKE", "POST /invoke failed or timed out");
        }
    }
    for (method, path) in [("GET", "/invoke"), ("POST", "/__conformance_missing__")] {
        if !probe(method, path, &payload, "application/json")
            .is_ok_and(|r| matches!(r.status, 404 | 405))
        {
            diagnostics.insert(
                "OCI-V1-INVOKE",
                "wrong method/path was not rejected with 404/405",
            );
        }
    }
    for text in VALID_DEADLINES {
        let body = request_with_deadline(json!({"message":"hello"}), text);
        let body = serde_json::to_vec(&body).unwrap();
        if !probe("POST", "/invoke", &body, "application/json").is_ok_and(|r| r.status == 200) {
            diagnostics.insert(
                "OCI-V1-REQUEST",
                format!("valid deadline {text} was not accepted with 200"),
            );
        }
    }
    for text in MALFORMED_DEADLINES {
        let body = request_with_deadline(json!({}), text);
        let body = serde_json::to_vec(&body).unwrap();
        if !probe("POST", "/invoke", &body, "application/json").is_ok_and(|r| r.status == 400) {
            diagnostics.insert(
                "OCI-V1-REQUEST",
                format!("malformed deadline {text} was not rejected with 400"),
            );
        }
    }
    let mut invalid: Vec<Vec<u8>> = vec![b"{".to_vec(), b"null".to_vec()];
    for fixture in FIXTURES
        .iter()
        .filter(|f| f.path.starts_with("fixtures/invalid/"))
    {
        let value: Value = serde_json::from_slice(fixture.bytes).unwrap();
        match fixture.schema {
            "request" => invalid.push(fixture.bytes.to_vec()),
            "context" => {
                invalid.push(serde_json::to_vec(&json!({"params":{}, "context":value})).unwrap())
            }
            _ => {}
        }
    }
    for field in [
        "credential",
        "token",
        "secret",
        "tenant",
        "workspace",
        "extra",
    ] {
        let mut value = request(json!({}));
        value["context"][field] = json!("forbidden");
        invalid.push(serde_json::to_vec(&value).unwrap());
    }
    let mut extra = request(json!({}));
    extra["extra"] = json!(true);
    invalid.push(serde_json::to_vec(&extra).unwrap());
    let mut missing = request(json!({}));
    missing.as_object_mut().unwrap().remove("params");
    invalid.push(serde_json::to_vec(&missing).unwrap());
    for body in invalid {
        if !probe("POST", "/invoke", &body, "application/json").is_ok_and(|r| r.status == 400) {
            diagnostics.insert(
                "OCI-V1-REQUEST",
                "invalid request/context was not rejected with 400",
            );
            break;
        }
    }
    if !probe("POST", "/invoke", &payload, "text/plain").is_ok_and(|r| r.status == 400) {
        diagnostics.insert(
            "OCI-V1-REQUEST",
            "non-JSON Content-Type was not rejected with 400",
        );
    }
    let schema: Value = serde_json::from_str(REQUEST_SCHEMA).unwrap();
    let budget = schema["properties"]["params"]["x-maxSerializedBytes"]
        .as_u64()
        .unwrap() as usize;
    for (size, accepted) in [(budget - 2, true), (budget - 1, false)] {
        let body = serde_json::to_vec(&request(json!("x".repeat(size)))).unwrap();
        match probe("POST", "/invoke", &body, "application/json") {
            Ok(response) if accepted && response.status == 200 => {
                inspect_response(response, &mut diagnostics)
            }
            Ok(response) if !accepted && response.status == 413 => {}
            _ => {
                diagnostics.insert(
                    "OCI-V1-PARAMS-LIMIT",
                    "params boundary was not accepted/rejected correctly",
                );
            }
        }
    }
    diagnostics
}

pub fn run(args: &[OsString]) -> i32 {
    let port = std::env::var("PORT")
        .ok()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|p| *p != 0);
    let Some(port) = port else {
        let mut diagnostics = Diagnostics::new();
        diagnostics.insert(
            "OCI-V1-PORT",
            "$PORT is mandatory, decimal 1–65535, with no default",
        );
        return diagnostics.report();
    };
    let result = match args {
        [mode] if mode == "http" => Ok(check_http(port)),
        [mode, separator, executable, rest @ ..] if mode == "process" && separator == "--" => {
            check_process(port, executable, rest)
        }
        _ => Err(io::Error::other(
            "usage: oci-function-conformance http | process -- /absolute/candidate [args...]",
        )),
    };
    match result {
        Ok(diagnostics) => diagnostics.report(),
        Err(error) => {
            eprintln!("conformance infrastructure: {error}");
            2
        }
    }
}

pub fn check_process(port: u16, executable: &OsStr, args: &[OsString]) -> io::Result<Diagnostics> {
    check_process_until(port, executable, args, Instant::now() + CHECK_BUDGET)
}

/// Every phase stops at `deadline`; a check that ends after it is the deadline error,
/// never a verdict. The candidate is always killed and reaped first (see [`KILL_SETTLE`]).
pub fn check_process_until(
    port: u16,
    executable: &OsStr,
    args: &[OsString],
    deadline: Instant,
) -> io::Result<Diagnostics> {
    check_process_with(
        Box::new(RealTable::default()),
        port,
        executable,
        args,
        deadline,
    )
}

/// [`check_process_until`] over any process table. An infrastructure error that arrives
/// once the deadline has passed is the deadline error.
pub fn check_process_with(
    table: Box<dyn ProcessTable>,
    port: u16,
    executable: &OsStr,
    args: &[OsString],
    deadline: Instant,
) -> io::Result<Diagnostics> {
    match process_phases(table, port, executable, args, deadline) {
        Err(_) if Instant::now() >= deadline => Err(deadline_error()),
        result => result,
    }
}

fn process_phases(
    table: Box<dyn ProcessTable>,
    port: u16,
    executable: &OsStr,
    args: &[OsString],
    deadline: Instant,
) -> io::Result<Diagnostics> {
    // Declared first, dropped last: the candidate is killed before the scratch tree goes.
    let mut probe = FilesystemProbe::new_until(deadline)?;
    let mut child = Candidate::spawn_until(table, port, executable, args, &probe, deadline)?;
    let mut diagnostics = check_http_until(port, deadline);
    ensure(deadline)?;
    // Health failures must not cascade into fictitious binding failures.
    if !diagnostics.contains_key("OCI-V1-HEALTHZ") {
        diagnostics.extend(child.binding(port)?);
    }
    if !child.terminate()? {
        diagnostics.insert(
            "OCI-V1-SIGTERM",
            "candidate ignored SIGTERM beyond 10 s grace",
        );
    }
    // Kill/reap before looking at the sentinel: no race with a surviving writer.
    child.cleanup()?;
    if !probe.unchanged().unwrap_or(false) {
        diagnostics.insert("OCI-V1-FILESYSTEM", "candidate changed cwd/HOME sentinel");
    }
    // Scratch removal belongs to the verdict: running out of time here is the deadline error.
    probe.remove_tree(deadline)?;
    ensure(deadline)?;
    Ok(diagnostics)
}

#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn stamp(path: &Path) -> io::Result<Stamp> {
    let m = fs::symlink_metadata(path)?;
    Ok(Stamp {
        device: m.dev(),
        inode: m.ino(),
        mode: m.mode(),
        uid: m.uid(),
        gid: m.gid(),
        links: m.nlink(),
        size: m.len(),
        modified: (m.mtime(), m.mtime_nsec()),
        changed: (m.ctime(), m.ctime_nsec()),
    })
}

pub struct FilesystemProbe {
    root: PathBuf,
    baseline: Option<(Stamp, Stamp)>,
    /// Scratch removal has been attempted; it is never started again.
    removed: bool,
    /// The check's deadline, which also bounds the removal run by Drop.
    deadline: Instant,
}
impl FilesystemProbe {
    pub fn new() -> io::Result<Self> {
        Self::new_until(Instant::now() + CHECK_BUDGET)
    }
    /// `deadline` bounds every removal of the scratch tree, Drop's included.
    pub fn new_until(deadline: Instant) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut root = None;
        for _ in 0..32 {
            let candidate = std::env::temp_dir().join(format!(
                "oci-conformance-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::DirBuilder::new().mode(0o700).create(&candidate) {
                Ok(()) => {
                    root = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        let mut probe = Self {
            root: root.ok_or_else(|| io::Error::other("cannot allocate private temp directory"))?,
            baseline: None,
            removed: false,
            deadline,
        };
        fs::create_dir(probe.sentinel())?;
        fs::create_dir(probe.scratch())?;
        fs::write(probe.sentinel().join("unchanged"), b"unchanged\n")?;
        probe.baseline = Some((
            stamp(&probe.sentinel())?,
            stamp(&probe.sentinel().join("unchanged"))?,
        ));
        Ok(probe)
    }
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
    pub fn sentinel(&self) -> std::path::PathBuf {
        self.root.join("sentinel")
    }
    pub fn scratch(&self) -> std::path::PathBuf {
        self.root.join("scratch")
    }
    pub fn unchanged(&self) -> io::Result<bool> {
        let (directory, file) = self.baseline.as_ref().expect("initialized probe");
        let sentinel = self.sentinel();
        if stamp(&sentinel)? != *directory || stamp(&sentinel.join("unchanged"))? != *file {
            return Ok(false);
        }
        for entry in fs::read_dir(&sentinel)? {
            if entry?.file_name() != "unchanged" {
                return Ok(false);
            }
        }
        Ok(fs::read(sentinel.join("unchanged"))? == b"unchanged\n")
    }
    /// Removes the scratch tree, checking `deadline` before every entry. Other failures are
    /// not a verdict (the entry stays behind); the deadline is. Runs out of time at most
    /// once: whatever was left behind is not retried by Drop.
    pub fn remove_tree(&mut self, deadline: Instant) -> io::Result<()> {
        self.removed = true;
        sweep(&self.root, deadline)
    }
}

fn sweep(path: &Path, deadline: Instant) -> io::Result<()> {
    ensure(deadline)?;
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let _ = fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700));
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    sweep(&entry.path(), deadline)?;
                }
            }
            let _ = fs::remove_dir(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
    Ok(())
}

impl Drop for FilesystemProbe {
    /// Removal that did not already run, on the check's deadline and no new budget.
    fn drop(&mut self) {
        if !self.removed {
            let _ = self.remove_tree(self.deadline);
        }
    }
}

/// How the tester sees and acts on processes; the seam for synthetic `/proc` tests.
/// Paths are `/proc/...` paths. Handles are opaque tokens for processes the table holds
/// a pidfd for.
pub trait ProcessTable {
    /// Pid of the tester, the child subreaper that adopts orphaned descendants.
    fn me(&self) -> i32;
    fn read(&self, path: &str) -> io::Result<String>;
    fn list(&self, path: &str) -> io::Result<Vec<String>>;
    fn link(&self, path: &str) -> io::Result<String>;
    /// Starts the command and retains a handle for it before returning.
    fn spawn(&self, command: &mut std::process::Command) -> io::Result<(i32, u64)>;
    fn open(&self, pid: i32) -> io::Result<u64>;
    /// Drops a handle without signalling it (a handle that turned out not to be ours).
    fn release(&self, handle: u64);
    fn signal(&self, handle: u64, signal: Signal) -> io::Result<()>;
    /// True once the process is gone (reaped by us, or dead and left to its parent);
    /// the handle is released then.
    fn reap(&self, handle: u64) -> io::Result<bool>;
}

#[derive(Default)]
pub struct RealTable {
    handles: RefCell<BTreeMap<u64, (i32, OwnedFd)>>,
    next: Cell<u64>,
}
impl RealTable {
    fn retain(&self, pid: i32) -> io::Result<u64> {
        use rustix::process::{Pid, PidfdFlags, pidfd_open};
        let pid = Pid::from_raw(pid).ok_or_else(|| io::Error::other("invalid pid"))?;
        let fd = pidfd_open(pid, PidfdFlags::empty())?;
        let handle = self.next.get();
        self.next.set(handle + 1);
        self.handles
            .borrow_mut()
            .insert(handle, (pid.as_raw_nonzero().get(), fd));
        Ok(handle)
    }
}
impl ProcessTable for RealTable {
    fn me(&self) -> i32 {
        std::process::id() as i32
    }
    fn read(&self, path: &str) -> io::Result<String> {
        fs::read_to_string(path)
    }
    fn list(&self, path: &str) -> io::Result<Vec<String>> {
        fs::read_dir(path)?
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect()
    }
    fn link(&self, path: &str) -> io::Result<String> {
        fs::read_link(path).map(|target| target.to_string_lossy().into_owned())
    }
    fn spawn(&self, command: &mut std::process::Command) -> io::Result<(i32, u64)> {
        use rustix::process::{Pid, set_child_subreaper};
        // Adopt orphaned descendants so cleanup can find and reap them.
        set_child_subreaper(Some(Pid::from_raw(1).unwrap()))?;
        spawn_retained(command, |pid| self.retain(pid))
    }
    fn open(&self, pid: i32) -> io::Result<u64> {
        self.retain(pid)
    }
    fn release(&self, handle: u64) {
        self.handles.borrow_mut().remove(&handle);
    }
    fn signal(&self, handle: u64, signal: Signal) -> io::Result<()> {
        let handles = self.handles.borrow();
        let (_, fd) = handles
            .get(&handle)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown handle"))?;
        rustix::process::pidfd_send_signal(fd, signal).map_err(Into::into)
    }
    fn reap(&self, handle: u64) -> io::Result<bool> {
        use rustix::{
            io::Errno,
            process::{WaitId, WaitIdOptions, waitid},
        };
        let mut handles = self.handles.borrow_mut();
        let Some((pid, fd)) = handles.get(&handle) else {
            return Ok(true);
        };
        let gone = match waitid(
            WaitId::PidFd(fd.as_fd()),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
        ) {
            Ok(status) => status.is_some(),
            Err(Errno::INTR) => false,
            // Not our child: its parent reaps it. Dead (or already reaped) is enough for us.
            Err(Errno::CHILD) => match fs::read_to_string(format!("/proc/{pid}/status")) {
                Err(error) => error.kind() == io::ErrorKind::NotFound,
                Ok(status) => parse_state(&status).is_ok_and(dead),
            },
            Err(error) => return Err(error.into()),
        };
        if gone {
            handles.remove(&handle);
        }
        Ok(gone)
    }
}

/// Starts the command and retains its handle, retrying a transient refusal (descriptor or
/// memory exhaustion) for at most [`KILL_SETTLE`]. The child is never signalled without a
/// handle: `Child::kill` may fall back to the numeric pid, and a numeric signal is never
/// sent. When no handle can be had the error names the pid of the child left running and
/// the caller (which still owns the process tree) must deal with it.
pub fn spawn_retained(
    command: &mut std::process::Command,
    mut retain: impl FnMut(i32) -> io::Result<u64>,
) -> io::Result<(i32, u64)> {
    let child = command.spawn()?;
    let pid = child.id() as i32;
    // The unreaped child keeps its pid reserved, so a handle opened now names this process.
    let settle = Instant::now() + KILL_SETTLE;
    let error = loop {
        match retain(pid) {
            Ok(handle) => return Ok((pid, handle)),
            Err(error) if Instant::now() >= settle => break error,
            Err(_) => thread::sleep(Duration::from_millis(5)),
        }
    };
    Err(io::Error::other(format!(
        "{error}; candidate pid {pid} has no process handle and was not signalled"
    )))
}

/// Why a read of the process table cannot be trusted.
enum Scan {
    Deadline,
    Unstable(String),
}

/// Checks the deadline after a read, success or not.
fn after<T>(deadline: Instant, result: io::Result<T>) -> Result<io::Result<T>, Scan> {
    if Instant::now() >= deadline {
        Err(Scan::Deadline)
    } else {
        Ok(result)
    }
}
/// A read that must succeed: any error invalidates the scan.
fn must<T>(deadline: Instant, path: &str, result: io::Result<T>) -> Result<T, Scan> {
    after(deadline, result)?.map_err(|error| Scan::Unstable(format!("{path}: {error}")))
}

fn dead(state: char) -> bool {
    matches!(state, 'Z' | 'X' | 'x')
}
fn parse_state(status: &str) -> Result<char, &'static str> {
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("State:"))
        .ok_or("missing task state")?
        .trim_start();
    let mut chars = value.chars();
    let letter = chars.next().ok_or("missing task state")?;
    if !"RSDTtZXxKWPI".contains(letter) || chars.next().is_some_and(|c| !c.is_whitespace()) {
        return Err("malformed task state");
    }
    Ok(letter)
}
/// Running and sleeping flip constantly; only stopped and dead are changes of substance.
fn state_class(state: char) -> u8 {
    match state {
        'T' | 't' => 1,
        _ if dead(state) => 2,
        _ => 0,
    }
}
fn parse_pids(text: &str) -> Result<Vec<i32>, Scan> {
    text.split_whitespace()
        .map(|word| {
            word.parse::<i32>()
                .ok()
                .filter(|pid| *pid > 0)
                .ok_or_else(|| Scan::Unstable("malformed pid list".into()))
        })
        .collect()
}
fn parse_ids(names: Vec<String>) -> Result<Vec<i32>, Scan> {
    names
        .iter()
        .map(|name| {
            name.parse::<i32>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or_else(|| Scan::Unstable(format!("malformed /proc entry {name:?}")))
        })
        .collect()
}
enum Link {
    Socket(u64),
    Other,
    Malformed,
}
fn classify_link(target: &str) -> Link {
    match target.strip_prefix("socket:[") {
        None => Link::Other,
        Some(rest) => match rest.strip_suffix(']').and_then(|n| n.parse().ok()) {
            Some(inode) => Link::Socket(inode),
            None => Link::Malformed,
        },
    }
}

/// What the final re-read compares.
#[derive(Default, PartialEq, Eq)]
struct Shape {
    /// Children of the tester's own tasks: the roots of the candidate tree.
    roots: BTreeSet<i32>,
    /// Every task of every process in the tree: state class and children.
    tasks: BTreeMap<(i32, i32), (u8, Vec<i32>)>,
}
#[derive(Default)]
struct Seen {
    shape: Shape,
    /// Socket inode to the tree processes holding it.
    owners: BTreeMap<u64, BTreeSet<i32>>,
    /// Who listed each process as its child; the tester for the roots.
    parents: BTreeMap<i32, i32>,
}
impl Seen {
    fn pids(&self) -> BTreeSet<i32> {
        self.shape.tasks.keys().map(|(pid, _)| *pid).collect()
    }
}

/// Samples every live task of every process reachable from the tester's children. Any
/// unreadable, malformed or vanished entry invalidates the whole sample, and a process
/// counts as visited only once all its tasks were read.
fn walk(table: &dyn ProcessTable, deadline: Instant, sockets: bool) -> Result<Seen, Scan> {
    let mut seen = Seen::default();
    let me = table.me();
    let own = format!("/proc/{me}/task");
    for tid in parse_ids(must(deadline, &own, table.list(&own))?)? {
        let path = format!("{own}/{tid}/children");
        match after(deadline, table.read(&path))? {
            Ok(text) => {
                for pid in parse_pids(&text)? {
                    seen.shape.roots.insert(pid);
                    seen.parents.insert(pid, me);
                }
            }
            // A tester thread exited and its children moved to a sibling task; the final
            // re-read notices if that changed what we saw.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(Scan::Unstable(format!("{path}: {error}"))),
        }
    }
    let mut pending: Vec<i32> = seen.shape.roots.iter().copied().collect();
    let mut done = BTreeSet::new();
    while let Some(pid) = pending.pop() {
        if done.contains(&pid) {
            continue;
        }
        let dir = format!("/proc/{pid}/task");
        let tids = parse_ids(must(deadline, &dir, table.list(&dir))?)?;
        if tids.is_empty() {
            return Err(Scan::Unstable(format!("{dir}: no tasks")));
        }
        for tid in tids {
            let base = format!("{dir}/{tid}");
            let path = format!("{base}/status");
            let status = must(deadline, &path, table.read(&path))?;
            let state = parse_state(&status)
                .map_err(|reason| Scan::Unstable(format!("{path}: {reason}")))?;
            let path = format!("{base}/children");
            let mut children = parse_pids(&must(deadline, &path, table.read(&path))?)?;
            children.sort_unstable();
            children.dedup();
            for child in &children {
                seen.parents.entry(*child).or_insert(pid);
            }
            pending.extend(&children);
            // A dead task has released its descriptors; its live siblings are read below.
            if sockets && !dead(state) {
                let fds = format!("{base}/fd");
                for fd in must(deadline, &fds, table.list(&fds))? {
                    let path = format!("{fds}/{fd}");
                    match classify_link(&must(deadline, &path, table.link(&path))?) {
                        Link::Socket(inode) => {
                            seen.owners.entry(inode).or_default().insert(pid);
                        }
                        Link::Other => {}
                        Link::Malformed => {
                            return Err(Scan::Unstable(format!("{path}: malformed socket link")));
                        }
                    }
                }
            }
            seen.shape
                .tasks
                .insert((pid, tid), (state_class(state), children));
        }
        done.insert(pid);
    }
    Ok(seen)
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Listener {
    address: SocketAddr,
    /// Zero when the table did not say.
    inode: u64,
}

fn parse_address(hex: &str, ipv6: bool) -> Option<IpAddr> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) || hex.len() != if ipv6 { 32 } else { 8 } {
        return None;
    }
    // The kernel prints each 32-bit word of the address in host byte order.
    let mut bytes = Vec::new();
    for word in hex.as_bytes().chunks(8) {
        let word = u32::from_str_radix(std::str::from_utf8(word).ok()?, 16).ok()?;
        bytes.extend(word.to_ne_bytes());
    }
    if ipv6 {
        Some(Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?).into())
    } else {
        Some(Ipv4Addr::from(<[u8; 4]>::try_from(bytes).ok()?).into())
    }
}

/// Listening rows of one `/proc/net/tcp{,6}` table. Every row is validated, not only
/// listeners: a malformed value anywhere invalidates the read.
fn parse_table(text: &str, ipv6: bool) -> Result<Vec<Listener>, &'static str> {
    let mut listeners = Vec::new();
    for line in text.lines().skip(1).filter(|l| !l.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 4 {
            return Err("short socket row");
        }
        let (address, port) = fields[1]
            .split_once(':')
            .ok_or("malformed socket address")?;
        let address = parse_address(address, ipv6).ok_or("malformed socket address")?;
        let port = (port.len() == 4 && port.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u16::from_str_radix(port, 16).ok())
            .flatten()
            .ok_or("malformed socket port")?;
        let state = (fields[3].len() == 2 && fields[3].bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u8::from_str_radix(fields[3], 16).ok())
            .flatten()
            .filter(|state| (0x01..=0x0c).contains(state))
            .ok_or("malformed socket state")?;
        let inode = match fields.get(9) {
            Some(inode) => inode.parse().map_err(|_| "malformed socket inode")?,
            None => 0,
        };
        if state == 0x0a {
            listeners.push(Listener {
                address: SocketAddr::new(address, port),
                inode,
            });
        }
    }
    Ok(listeners)
}

fn read_tables(table: &dyn ProcessTable, deadline: Instant) -> Result<Vec<Listener>, Scan> {
    let mut all = Vec::new();
    for (path, ipv6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        let text = must(deadline, path, table.read(path))?;
        all.extend(
            parse_table(&text, ipv6)
                .map_err(|reason| Scan::Unstable(format!("{path}: {reason}")))?,
        );
    }
    all.sort();
    Ok(all)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Tree,
    Outside,
    Unknown,
}

/// Only 127.0.0.1:$PORT held by the tree passes. A listener positively held by a process
/// outside the tree is not the candidate's; one with no known owner fails.
fn binding_verdict(listeners: &[(Listener, Owner)], port: u16) -> Diagnostics {
    let mut loopback = false;
    let (mut extra, mut unknown) = (Vec::new(), Vec::new());
    for (listener, owner) in listeners {
        let allowed = listener.address == SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        match owner {
            Owner::Outside => {}
            Owner::Tree if allowed => loopback = true,
            Owner::Tree => extra.push(listener.address.to_string()),
            Owner::Unknown => unknown.push(listener.address.to_string()),
        }
    }
    let mut parts = Vec::new();
    if !extra.is_empty() {
        parts.push(format!(
            "candidate also listens on {} (only 127.0.0.1:$PORT is allowed)",
            extra.join(", ")
        ));
    }
    if !unknown.is_empty() {
        parts.push(format!(
            "cannot attribute listener {} to any process",
            unknown.join(", ")
        ));
    }
    if !loopback {
        parts.push("candidate is not listening on 127.0.0.1:$PORT".to_owned());
    }
    let mut diagnostics = Diagnostics::new();
    if !parts.is_empty() {
        diagnostics.insert("OCI-V1-BINDING", parts.join("; "));
    }
    diagnostics
}

/// Table-only form of the binding rule: every listener is taken as the candidate's.
pub fn check_binding_snapshot(tcp: &str, tcp6: &str, port: u16) -> Diagnostics {
    let parsed = parse_table(tcp, false).and_then(|mut all| {
        all.extend(parse_table(tcp6, true)?);
        Ok(all)
    });
    match parsed {
        Ok(listeners) => {
            let listeners: Vec<_> = listeners.into_iter().map(|l| (l, Owner::Tree)).collect();
            binding_verdict(&listeners, port)
        }
        Err(reason) => {
            let mut diagnostics = Diagnostics::new();
            diagnostics.insert(
                "OCI-V1-BINDING",
                format!("cannot inspect Linux listening sockets: {reason}"),
            );
            diagnostics
        }
    }
}

/// Socket inodes of `wanted` held by processes outside the tree that we can read.
fn outside_inodes(
    table: &dyn ProcessTable,
    tree: &BTreeSet<i32>,
    wanted: &BTreeSet<u64>,
    deadline: Instant,
) -> Result<BTreeSet<u64>, Scan> {
    let mut found = BTreeSet::new();
    let Ok(procs) = after(deadline, table.list("/proc"))? else {
        return Ok(found);
    };
    for pid in procs.iter().filter_map(|name| name.parse::<i32>().ok()) {
        if tree.contains(&pid) {
            continue;
        }
        let dir = format!("/proc/{pid}/fd");
        let Ok(fds) = after(deadline, table.list(&dir))? else {
            continue;
        };
        for fd in fds {
            let path = format!("{dir}/{fd}");
            if let Ok(target) = after(deadline, table.link(&path))?
                && let Link::Socket(inode) = classify_link(&target)
                && wanted.contains(&inode)
            {
                found.insert(inode);
            }
        }
    }
    Ok(found)
}

/// One sample plus a final consistency re-read of the socket tables and of the tree.
fn sample(table: &dyn ProcessTable, port: u16, deadline: Instant) -> Result<Diagnostics, Scan> {
    let first = read_tables(table, deadline)?;
    let seen = walk(table, deadline, true)?;
    let second = read_tables(table, deadline)?;
    let again = walk(table, deadline, false)?;
    if first != second {
        return Err(Scan::Unstable(
            "socket tables changed while the tree was sampled".into(),
        ));
    }
    if seen.shape != again.shape {
        return Err(Scan::Unstable(
            "process tree changed while it was sampled".into(),
        ));
    }
    let wanted: BTreeSet<u64> = first
        .iter()
        .filter(|l| l.inode != 0 && !seen.owners.contains_key(&l.inode))
        .map(|l| l.inode)
        .collect();
    let outside = if wanted.is_empty() {
        BTreeSet::new()
    } else {
        outside_inodes(table, &seen.pids(), &wanted, deadline)?
    };
    let listeners: Vec<_> = first
        .into_iter()
        .map(|listener| {
            let owner = if seen.owners.contains_key(&listener.inode) {
                Owner::Tree
            } else if outside.contains(&listener.inode) {
                Owner::Outside
            } else {
                Owner::Unknown
            };
            (listener, owner)
        })
        .collect();
    ensure(deadline).map_err(|_| Scan::Deadline)?;
    Ok(binding_verdict(&listeners, port))
}

/// OCI-V1-BINDING over the whole candidate tree. Rescans on any anomaly, at most
/// [`MAX_SCANS`] times, then fails as unstable; never passes on incomplete evidence.
pub fn inspect_binding(
    table: &dyn ProcessTable,
    port: u16,
    deadline: Instant,
) -> io::Result<Diagnostics> {
    let mut reason = String::new();
    for _ in 0..MAX_SCANS {
        match sample(table, port, deadline) {
            Ok(diagnostics) => return Ok(diagnostics),
            Err(Scan::Deadline) => return Err(deadline_error()),
            Err(Scan::Unstable(why)) => reason = why,
        }
    }
    let mut diagnostics = Diagnostics::new();
    diagnostics.insert(
        "OCI-V1-BINDING",
        format!("inspection unstable after {MAX_SCANS} scans: {reason}"),
    );
    Ok(diagnostics)
}

/// The candidate and everything it starts. Every process of the tree is retained by
/// handle (a pidfd) as it is discovered and signalled only through that handle.
pub struct Candidate {
    table: Box<dyn ProcessTable>,
    root: i32,
    handles: BTreeMap<i32, u64>,
    deadline: Instant,
    cleaned: bool,
}
impl Candidate {
    pub fn spawn(
        port: u16,
        executable: &OsStr,
        args: &[OsString],
        probe: &FilesystemProbe,
    ) -> io::Result<Self> {
        Self::spawn_until(
            Box::new(RealTable::default()),
            port,
            executable,
            args,
            probe,
            Instant::now() + CHECK_BUDGET,
        )
    }
    pub fn spawn_until(
        table: Box<dyn ProcessTable>,
        port: u16,
        executable: &OsStr,
        args: &[OsString],
        probe: &FilesystemProbe,
        deadline: Instant,
    ) -> io::Result<Self> {
        use std::os::unix::process::CommandExt;
        if !Path::new(executable).is_absolute() {
            return Err(io::Error::other(
                "candidate executable must be an absolute path",
            ));
        }
        let mut command = std::process::Command::new(executable);
        command
            .args(args)
            .env_clear()
            .env("PORT", port.to_string())
            .env("HOME", probe.sentinel())
            .env("TMPDIR", probe.scratch())
            .current_dir(probe.sentinel())
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // The root handle is retained before anything can fail; from here on Drop owns the kill.
        let (root, handle) = table.spawn(&mut command)?;
        let mut candidate = Self {
            table,
            root,
            handles: BTreeMap::from([(root, handle)]),
            deadline,
            cleaned: false,
        };
        match candidate.discover(deadline, false) {
            Err(Scan::Deadline) => Err(deadline_error()),
            _ => Ok(candidate),
        }
    }
    pub fn pid(&self) -> rustix::process::Pid {
        rustix::process::Pid::from_raw(self.root).expect("candidate pid")
    }
    /// Retains a handle for every process present, then inspects. A tree whose members
    /// cannot be retained and confirmed in [`MAX_SCANS`] attempts is unstable: it fails.
    pub fn binding(&mut self, port: u16) -> io::Result<Diagnostics> {
        for _ in 0..MAX_SCANS {
            match self.discover(self.deadline, false) {
                Err(Scan::Deadline) => return Err(deadline_error()),
                Ok(Some(_)) => return inspect_binding(&*self.table, port, self.deadline),
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }
        let mut diagnostics = Diagnostics::new();
        diagnostics.insert(
            "OCI-V1-BINDING",
            format!("inspection unstable after {MAX_SCANS} scans: a process could not be retained"),
        );
        Ok(diagnostics)
    }

    /// True when `pid` is listed as a child of `parent` or of the tester, read now. A child
    /// stays listed (even as a zombie) until its parent reaps it, so a pid that is listed
    /// after its handle was opened cannot have been reused by an unrelated process.
    fn still_a_child(&self, pid: i32, parent: i32, deadline: Instant) -> Result<bool, Scan> {
        for owner in [parent, self.table.me()] {
            let dir = format!("/proc/{owner}/task");
            let tids = match after(deadline, self.table.list(&dir))? {
                Ok(names) => parse_ids(names)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(Scan::Unstable(format!("{dir}: {error}"))),
            };
            for tid in tids {
                let path = format!("{dir}/{tid}/children");
                match after(deadline, self.table.read(&path))? {
                    Ok(text) => {
                        if parse_pids(&text)?.contains(&pid) {
                            return Ok(true);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(Scan::Unstable(format!("{path}: {error}"))),
                }
            }
        }
        Ok(false)
    }

    /// Retains a handle for every tree process not yet held. A handle counts only once the
    /// process is confirmed to still be in the tree after the handle was opened; one that
    /// cannot be confirmed is released unsignalled. `Ok(None)` means the scan was incomplete,
    /// a process vanished or a handle could not be had or confirmed: not evidence of absence.
    fn discover(&mut self, deadline: Instant, kill: bool) -> Result<Option<usize>, Scan> {
        let seen = match walk(&*self.table, deadline, false) {
            Ok(seen) => seen,
            Err(Scan::Unstable(_)) => return Ok(None),
            Err(Scan::Deadline) => return Err(Scan::Deadline),
        };
        let pids = seen.pids();
        let mut complete = true;
        for pid in &pids {
            if self.handles.contains_key(pid) {
                continue;
            }
            // A handle that was opened and confirmed is retained (and killed when asked)
            // before the deadline is looked at: expiry never discards it.
            match self.table.open(*pid) {
                Ok(handle) => {
                    let parent = seen.parents.get(pid).copied().unwrap_or(self.table.me());
                    // An unconfirmed handle is released, never signalled, so the answer is
                    // read even when the deadline passed while the handle was opened.
                    let until = deadline.max(Instant::now() + CONFIRM_GRACE);
                    let ours = matches!(self.still_a_child(*pid, parent, until), Ok(true));
                    if ours {
                        self.handles.insert(*pid, handle);
                        if kill {
                            let _ = self.table.signal(handle, Signal::KILL);
                        }
                    } else {
                        self.table.release(handle);
                        complete = false;
                    }
                }
                Err(_) => complete = false,
            }
            if Instant::now() >= deadline {
                return Err(Scan::Deadline);
            }
        }
        Ok(complete.then_some(pids.len()))
    }

    /// SIGTERM to the candidate; true when it exits within the grace. Descendants are not
    /// signalled: [`Candidate::cleanup`] ends whatever is left.
    pub fn terminate(&mut self) -> io::Result<bool> {
        let Some(&handle) = self.handles.get(&self.root) else {
            return Ok(true);
        };
        if self.table.reap(handle)? {
            self.handles.remove(&self.root);
            return Ok(true);
        }
        match self.table.signal(handle, Signal::TERM) {
            Err(error) if error.raw_os_error() != Some(rustix::io::Errno::SRCH.raw_os_error()) => {
                return Err(error);
            }
            _ => {}
        }
        let grace = Instant::now() + TERM_GRACE;
        loop {
            if self.table.reap(handle)? {
                self.handles.remove(&self.root);
                return Ok(true);
            }
            ensure(self.deadline)?;
            if Instant::now() >= grace {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// SIGKILL to every retained handle, then reap until a fresh scan of the tester's
    /// descendants, taken after the last reap, is empty. The kill is never skipped; the
    /// wait for it is bounded by [`KILL_SETTLE`] from the first SIGKILL, deadline or not.
    /// Runs at most once: Drop does not start a second wait.
    pub fn cleanup(&mut self) -> io::Result<()> {
        self.cleaned = true;
        let mut settle = None;
        loop {
            for handle in self.handles.values() {
                let _ = self.table.signal(*handle, Signal::KILL);
            }
            let settle = *settle.get_or_insert_with(|| Instant::now() + KILL_SETTLE);
            let timed_out =
                || io::Error::other("candidate cleanup did not finish within the kill settle time");
            let found = self.discover(settle, true).map_err(|_| timed_out())?;
            let mut reaped = false;
            for (pid, handle) in self.handles.clone() {
                ensure(settle).map_err(|_| timed_out())?;
                if self.table.reap(handle).unwrap_or(false) {
                    self.handles.remove(&pid);
                    reaped = true;
                }
            }
            if found == Some(0) && !reaped && self.handles.is_empty() {
                return Ok(());
            }
            ensure(settle).map_err(|_| timed_out())?;
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}
