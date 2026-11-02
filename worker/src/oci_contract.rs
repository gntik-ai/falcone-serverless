//! Standalone OCI v1 conformance boundary; not connected to the execution worker.
//!
//! Rule 3 residual risk: only the candidate process tree is untrusted. Outside
//! processes are trusted platform components. This inspector does not defend
//! against an outside process signalling (including SIGSTOP/SIGCONT), tracing,
//! or passing file descriptors to the candidate. Such a process can itself open
//! a forbidden listener. Once every in-tree task is stopped, it cannot resume
//! itself; final task-state reads close transitions originating inside the tree.
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
    json!({"params":params,"context":{"handle":"opaque-attempt", "invocationId":"invocation-1", "deadline":"2026-10-31T12:00:00Z"}})
}

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
    ffi::OsString,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
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
pub const TERM_GRACE: Duration = Duration::from_secs(10);

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
        let result = self.stream.read(bytes);
        self.remaining()?;
        result
    }
}
impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        let result = self.stream.write(bytes);
        self.remaining()?;
        result
    }
    fn flush(&mut self) -> io::Result<()> {
        self.remaining()?;
        let result = self.stream.flush();
        self.remaining()?;
        result
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
    let response = read_response(&mut stream);
    ensure_deadline(deadline)?;
    response
}

fn probe(
    port: u16,
    method: &str,
    path: &str,
    body: &[u8],
    content_type: &str,
    deadline: Instant,
) -> io::Result<Response> {
    ensure_deadline(deadline)?;
    let response = exchange(
        port,
        method,
        path,
        body,
        content_type,
        deadline.min(Instant::now() + IO_BUDGET),
    );
    ensure_deadline(deadline)?;
    response
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

pub fn check_http(port: u16) -> io::Result<Diagnostics> {
    check_http_until(port, Instant::now() + CHECK_BUDGET)
}

fn check_http_until(port: u16, deadline: Instant) -> io::Result<Diagnostics> {
    ensure_deadline(deadline)?;
    let mut diagnostics = Diagnostics::new();
    let startup = deadline.min(Instant::now() + STARTUP_BUDGET);
    loop {
        ensure_deadline(deadline)?;
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
            ensure_deadline(deadline)?;
            return Ok(diagnostics);
        }
        thread::sleep(Duration::from_millis(20));
    }
    let payload = serde_json::to_vec(&request(json!({"message":"hello"}))).unwrap();
    match probe(
        port,
        "POST",
        "/invoke",
        &payload,
        "application/json",
        deadline,
    ) {
        Ok(response) => inspect_response(response, &mut diagnostics),
        Err(_) => {
            diagnostics.insert("OCI-V1-INVOKE", "POST /invoke failed or timed out");
        }
    }
    for (method, path) in [("GET", "/invoke"), ("POST", "/__conformance_missing__")] {
        if !probe(port, method, path, &payload, "application/json", deadline)
            .is_ok_and(|r| matches!(r.status, 404 | 405))
        {
            diagnostics.insert(
                "OCI-V1-INVOKE",
                "wrong method/path was not rejected with 404/405",
            );
        }
    }
    for valid_deadline in [
        "2000-02-29T00:00:00Z",
        "2026-10-31T12:00:00.5Z",
        "2026-10-31T12:00:00.123456789Z",
    ] {
        ensure_deadline(deadline)?;
        let mut value = request(json!({}));
        value["context"]["deadline"] = json!(valid_deadline);
        let body = serde_json::to_vec(&value).unwrap();
        match probe(port, "POST", "/invoke", &body, "application/json", deadline) {
            Ok(response) if response.status == 200 => inspect_response(response, &mut diagnostics),
            _ => diagnostics.insert(
                "OCI-V1-REQUEST",
                "valid UTC deadline was not accepted with 200",
            ),
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
    for invalid_deadline in [
        "yesterday",
        "2026-10-31T12:00:00+00:00",
        "2026-10-31T12:00:00.1234567890Z",
        "2026-10-31T12:00:00z",
        "2026-02-30T12:00:00Z",
        "2026-10-31T24:00:00Z",
    ] {
        ensure_deadline(deadline)?;
        let mut value = request(json!({}));
        value["context"]["deadline"] = json!(invalid_deadline);
        invalid.push(serde_json::to_vec(&value).unwrap());
    }
    let mut extra = request(json!({}));
    extra["extra"] = json!(true);
    invalid.push(serde_json::to_vec(&extra).unwrap());
    let mut missing = request(json!({}));
    missing.as_object_mut().unwrap().remove("params");
    invalid.push(serde_json::to_vec(&missing).unwrap());
    for body in invalid {
        ensure_deadline(deadline)?;
        if !probe(port, "POST", "/invoke", &body, "application/json", deadline)
            .is_ok_and(|r| r.status == 400)
        {
            diagnostics.insert(
                "OCI-V1-REQUEST",
                "invalid request/context was not rejected with 400",
            );
            break;
        }
    }
    if !probe(port, "POST", "/invoke", &payload, "text/plain", deadline)
        .is_ok_and(|r| r.status == 400)
    {
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
        ensure_deadline(deadline)?;
        let body = serde_json::to_vec(&request(json!("x".repeat(size)))).unwrap();
        match probe(port, "POST", "/invoke", &body, "application/json", deadline) {
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
    ensure_deadline(deadline)?;
    Ok(diagnostics)
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
        [mode] if mode == "http" => check_http(port),
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

pub fn check_process(
    port: u16,
    executable: &std::ffi::OsStr,
    args: &[OsString],
) -> io::Result<Diagnostics> {
    let deadline = Instant::now() + CHECK_BUDGET;
    let mut probe = FilesystemProbe::new_until(deadline)?;
    let mut child = Candidate::spawn_until(port, executable, args, &probe, deadline)?;
    let inspected: io::Result<Diagnostics> = (|| {
        let mut diagnostics = check_http_until(port, deadline)?;
        if !diagnostics.contains_key("OCI-V1-HEALTHZ") {
            match child.inspect_binding(port)? {
                BindingVerdict::Compliant => {}
                BindingVerdict::Violation(detail) | BindingVerdict::Unstable(detail) => {
                    diagnostics.insert("OCI-V1-BINDING", detail)
                }
            }
        }
        Ok(diagnostics)
    })();
    let terminated = child.terminate();
    drop(child);
    let result = (|| {
        let mut diagnostics = inspected?;
        if !terminated? {
            diagnostics.insert(
                "OCI-V1-SIGTERM",
                "candidate ignored SIGTERM beyond 10 s grace",
            );
        }
        // Inspect the sentinel only after successful kill/reap of the entire tree.
        if !probe.unchanged().unwrap_or(false) {
            diagnostics.insert("OCI-V1-FILESYSTEM", "candidate changed cwd/HOME sentinel");
        }
        ensure_deadline(deadline)?;
        Ok(diagnostics)
    })();
    let cleanup = probe.cleanup();
    // Filesystem cleanup is part of the result, never deferred until after success.
    ensure_deadline(deadline)?;
    cleanup?;
    result
}

use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

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
    deadline: Instant,
    cleanup_attempted: bool,
    baseline: Option<(Stamp, Stamp)>,
}
impl FilesystemProbe {
    pub fn new() -> io::Result<Self> {
        Self::new_until(Instant::now() + CHECK_BUDGET)
    }
    pub fn new_until(deadline: Instant) -> io::Result<Self> {
        ensure_deadline(deadline)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut root = None;
        for _ in 0..32 {
            ensure_deadline(deadline)?;
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
            deadline,
            cleanup_attempted: false,
            baseline: None,
        };
        ensure_deadline(deadline)?;
        fs::create_dir(probe.sentinel())?;
        ensure_deadline(deadline)?;
        fs::create_dir(probe.scratch())?;
        ensure_deadline(deadline)?;
        fs::write(probe.sentinel().join("unchanged"), b"unchanged\n")?;
        ensure_deadline(deadline)?;
        let directory = stamp(&probe.sentinel())?;
        ensure_deadline(deadline)?;
        let file = stamp(&probe.sentinel().join("unchanged"))?;
        ensure_deadline(deadline)?;
        probe.baseline = Some((directory, file));
        ensure_deadline(deadline)?;
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
        ensure_deadline(self.deadline)?;
        let (directory, file) = self.baseline.as_ref().expect("initialized probe");
        let sentinel = self.sentinel();
        let directory_now = stamp(&sentinel)?;
        ensure_deadline(self.deadline)?;
        if directory_now != *directory {
            return Ok(false);
        }
        let file_now = stamp(&sentinel.join("unchanged"))?;
        ensure_deadline(self.deadline)?;
        if file_now != *file {
            return Ok(false);
        }
        for entry in fs::read_dir(&sentinel)? {
            ensure_deadline(self.deadline)?;
            let entry = entry?;
            ensure_deadline(self.deadline)?;
            if entry.file_name() != "unchanged" {
                return Ok(false);
            }
        }
        let bytes = fs::read(sentinel.join("unchanged"))?;
        ensure_deadline(self.deadline)?;
        Ok(bytes == b"unchanged\n")
    }
    pub fn cleanup(&mut self) -> io::Result<()> {
        self.cleanup_with(remove_tree)
    }
    /// Filesystem boundary: a late successful removal still reports deadline exhaustion.
    pub fn cleanup_with(
        &mut self,
        remove: impl FnOnce(&Path, Instant) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.cleanup_attempted {
            return Err(io::Error::other("scratch cleanup already attempted"));
        }
        self.cleanup_attempted = true;
        ensure_deadline(self.deadline)?;
        let result = remove(&self.root, self.deadline);
        ensure_deadline(self.deadline)?;
        result
    }
}
fn remove_tree(root: &Path, deadline: Instant) -> io::Result<()> {
    let mut pending = vec![(root.to_path_buf(), false)];
    while let Some((path, directory_done)) = pending.pop() {
        ensure_deadline(deadline)?;
        if directory_done {
            fs::remove_dir(&path)?;
        } else {
            let metadata = fs::symlink_metadata(&path)?;
            ensure_deadline(deadline)?;
            if metadata.is_dir() {
                pending.push((path.clone(), true));
                for entry in fs::read_dir(&path)? {
                    ensure_deadline(deadline)?;
                    let entry = entry?;
                    ensure_deadline(deadline)?;
                    pending.push((entry.path(), false));
                }
            } else {
                fs::remove_file(&path)?;
            }
        }
        ensure_deadline(deadline)?;
    }
    ensure_deadline(deadline)
}
impl Drop for FilesystemProbe {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let _ = self.cleanup();
        }
    }
}

pub fn check_binding_snapshot(tcp: &str, tcp6: &str, port: u16) -> Diagnostics {
    let mut diagnostics = Diagnostics::new();
    let mut seen = false;
    for (table, ipv6) in [(tcp, false), (tcp6, true)] {
        for line in table.lines().skip(1) {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.get(3) != Some(&"0A") {
                continue;
            }
            let Some(local) = fields.get(1).and_then(|s| endpoint(s, ipv6).ok()) else {
                diagnostics.insert("OCI-V1-BINDING", "inspection unstable: malformed listener");
                return diagnostics;
            };
            if local != SocketAddr::from((Ipv4Addr::LOCALHOST, port)) {
                diagnostics.insert("OCI-V1-BINDING", format!("candidate is not listening exclusively on 127.0.0.1:$PORT: listener {local}"));
                return diagnostics;
            }
            seen = true;
        }
    }
    if !seen {
        diagnostics.insert(
            "OCI-V1-BINDING",
            "candidate has no listener on 127.0.0.1:$PORT",
        );
    }
    diagnostics
}

fn endpoint(text: &str, ipv6: bool) -> io::Result<SocketAddr> {
    let invalid = || io::Error::other("malformed TCP endpoint");
    let (address, port) = text.split_once(':').ok_or_else(invalid)?;
    if port.len() != 4 {
        return Err(invalid());
    }
    if !port.bytes().all(|b| b.is_ascii_hexdigit())
        || !address.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid());
    }
    let port = u16::from_str_radix(port, 16).map_err(|_| invalid())?;
    if ipv6 {
        if address.len() != 32 || !address.is_ascii() {
            return Err(invalid());
        }
        let mut bytes = [0; 16];
        for (word, chunk) in bytes.chunks_mut(4).enumerate() {
            chunk.copy_from_slice(
                &u32::from_str_radix(&address[word * 8..word * 8 + 8], 16)
                    .map_err(|_| invalid())?
                    .to_le_bytes(),
            );
        }
        Ok(SocketAddr::from((std::net::Ipv6Addr::from(bytes), port)))
    } else {
        if address.len() != 8 {
            return Err(invalid());
        }
        let value = u32::from_str_radix(address, 16).map_err(|_| invalid())?;
        Ok(SocketAddr::from((
            Ipv4Addr::from(value.to_le_bytes()),
            port,
        )))
    }
}

/// Maximum cleanup polling after SIGKILL, clipped to the check's absolute deadline.
/// This adds at most one second to TERM_GRACE; procfs reads are checked on return.
pub const CLEANUP_BOUND: Duration = Duration::from_secs(1);
pub const CHECK_BUDGET: Duration = Duration::from_secs(90);

pub struct Candidate {
    tree: ProcessTree<Procfs>,
    _child: std::process::Child,
    // Subreaping is process-wide. Never confuse another concurrent candidate's roots.
    _guard: std::sync::MutexGuard<'static, ()>,
}
impl Candidate {
    pub fn spawn(
        port: u16,
        executable: &std::ffi::OsStr,
        args: &[OsString],
        probe: &FilesystemProbe,
    ) -> io::Result<Self> {
        Self::spawn_until(port, executable, args, probe, probe.deadline)
    }
    fn spawn_until(
        port: u16,
        executable: &std::ffi::OsStr,
        args: &[OsString],
        probe: &FilesystemProbe,
        deadline: Instant,
    ) -> io::Result<Self> {
        use rustix::process::{Pid, PidfdFlags, pidfd_open, set_child_subreaper};
        use std::os::unix::process::CommandExt;
        if !Path::new(executable).is_absolute() {
            return Err(io::Error::other(
                "candidate executable must be an absolute path",
            ));
        }
        static CANDIDATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = loop {
            ensure_deadline(deadline)?;
            match CANDIDATE.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
            }
        };
        let supervisor = std::process::id();
        let proc_self = fs::read_to_string("/proc/self/stat")?;
        ensure_deadline(deadline)?;
        if proc_self
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            != Some(supervisor)
        {
            return Err(io::Error::other(
                "procfs PID namespace mismatch: /proc must describe the candidate PID namespace",
            ));
        }
        // Refuse unsupported kernels before starting a candidate.
        drop(pidfd_open(
            Pid::from_raw(supervisor as i32).unwrap(),
            PidfdFlags::empty(),
        )?);
        set_child_subreaper(Some(Pid::from_raw(1).unwrap()))?;
        let mut table = Procfs;
        let mut excluded = BTreeMap::new();
        for tid in table.list(&format!("/proc/{supervisor}/task"), deadline)? {
            ensure_deadline(deadline)?;
            let text = table.read(&format!("/proc/{supervisor}/task/{tid}/children"))?;
            ensure_deadline(deadline)?;
            for child in text.split_whitespace() {
                ensure_deadline(deadline)?;
                let pid = child
                    .parse()
                    .map_err(|_| io::Error::other("malformed adopted roots"))?;
                excluded.insert(pid, table.open(pid, deadline)?);
                ensure_deadline(deadline)?;
            }
        }
        let child = std::process::Command::new(executable)
            .args(args)
            .env_clear()
            .env("PORT", port.to_string())
            .env("HOME", probe.sentinel())
            .env("TMPDIR", probe.scratch())
            .current_dir(probe.sentinel())
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let root = child.id();
        let tree = ProcessTree::new(table, root, supervisor, excluded, deadline);
        let mut candidate = Self {
            tree,
            _child: child,
            _guard: guard,
        };
        candidate.tree.discover(root)?;
        candidate.tree.check_time()?;
        Ok(candidate)
    }
    pub fn pid(&self) -> rustix::process::Pid {
        rustix::process::Pid::from_raw(self.tree.root as i32).unwrap()
    }
    pub fn terminate(&mut self) -> io::Result<bool> {
        self.tree.terminate()
    }
    pub fn inspect_binding(&mut self, port: u16) -> io::Result<BindingVerdict> {
        self.tree.inspect_binding(port)
    }
}

pub struct Procfs;
pub struct ProcessHandle {
    fd: std::os::fd::OwnedFd,
    pid: u32,
    start: u64,
}
impl ProcessTable for Procfs {
    type Handle = ProcessHandle;
    fn read(&mut self, path: &str) -> io::Result<String> {
        fs::read_to_string(path)
    }
    fn list(&mut self, path: &str, deadline: Instant) -> io::Result<Vec<u32>> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(path)? {
            ensure_deadline(deadline)?;
            let name = entry?.file_name();
            ensure_deadline(deadline)?;
            let text = name
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF8 procfs entry"))?;
            if text.bytes().all(|b| b.is_ascii_digit()) {
                ids.push(
                    text.parse()
                        .map_err(|_| io::Error::other("malformed procfs PID"))?,
                );
            } else if path != "/proc" {
                return Err(io::Error::other("malformed task ID"));
            }
        }
        ensure_deadline(deadline)?;
        Ok(ids)
    }
    fn sockets(&mut self, pid: u32, tid: u32, deadline: Instant) -> io::Result<BTreeSet<u64>> {
        let mut sockets = BTreeSet::new();
        for entry in fs::read_dir(format!("/proc/{pid}/task/{tid}/fd"))? {
            ensure_deadline(deadline)?;
            let target = fs::read_link(entry?.path())?;
            ensure_deadline(deadline)?;
            let target = target
                .to_str()
                .ok_or_else(|| io::Error::other("malformed fd target"))?;
            if target.starts_with("socket:") {
                let inode = target
                    .strip_prefix("socket:[")
                    .and_then(|s| s.strip_suffix(']'))
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| io::Error::other("malformed socket inode"))?;
                sockets.insert(inode);
            }
        }
        ensure_deadline(deadline)?;
        Ok(sockets)
    }
    fn open(&mut self, pid: u32, deadline: Instant) -> io::Result<ProcessHandle> {
        use rustix::process::{Pid, PidfdFlags, pidfd_open};
        ensure_deadline(deadline)?;
        let start = proc_start(pid)?;
        ensure_deadline(deadline)?;
        let id = i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| io::Error::other("invalid PID"))?;
        let fd = pidfd_open(id, PidfdFlags::empty())?;
        ensure_deadline(deadline)?;
        let after = proc_start(pid)?;
        ensure_deadline(deadline)?;
        if after != start {
            return Err(io::Error::other("PID changed while opening pidfd"));
        }
        Ok(ProcessHandle { fd, pid, start })
    }
    fn is_current(&mut self, handle: &ProcessHandle, deadline: Instant) -> io::Result<bool> {
        use std::os::fd::AsRawFd;
        ensure_deadline(deadline)?;
        let info = fs::read_to_string(format!("/proc/self/fdinfo/{}", handle.fd.as_raw_fd()))?;
        ensure_deadline(deadline)?;
        let pid = info
            .lines()
            .find_map(|line| line.strip_prefix("Pid:"))
            .and_then(|pid| pid.trim().parse::<i64>().ok())
            .ok_or_else(|| io::Error::other("malformed pidfd identity"))?;
        Ok(pid == i64::from(handle.pid))
    }
    fn signal(
        &mut self,
        handle: &ProcessHandle,
        signal: rustix::process::Signal,
    ) -> io::Result<()> {
        rustix::process::pidfd_send_signal(&handle.fd, signal).map_err(Into::into)
    }
    fn reap(&mut self, handle: &ProcessHandle, deadline: Instant) -> io::Result<bool> {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        use std::os::fd::AsFd;
        ensure_deadline(deadline)?;
        let result = waitid(
            WaitId::PidFd(handle.fd.as_fd()),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
        );
        ensure_deadline(deadline)?;
        match result {
            Ok(status) => Ok(status.is_some()),
            Err(rustix::io::Errno::CHILD) => {
                let start = proc_start(handle.pid);
                ensure_deadline(deadline)?;
                match start {
                    Ok(start) => Ok(start != handle.start), // Already reaped by its parent; never signal a reused PID.
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error.into()),
        }
    }
}
fn proc_start(pid: u32) -> io::Result<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    text.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other("malformed process identity"))
}
fn ensure_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(deadline_error())
    } else {
        Ok(())
    }
}

/// OS boundary for process identity, task-local procfs evidence and time.
/// Handles must refer to a process lifetime, never a reusable numeric PID.
pub trait ProcessTable {
    type Handle;
    fn read(&mut self, path: &str) -> io::Result<String>;
    fn list(&mut self, path: &str, deadline: Instant) -> io::Result<Vec<u32>>;
    fn sockets(
        &mut self,
        pid: u32,
        tid: u32,
        deadline: Instant,
    ) -> io::Result<std::collections::BTreeSet<u64>>;
    fn open(&mut self, pid: u32, deadline: Instant) -> io::Result<Self::Handle>;
    fn is_current(&mut self, handle: &Self::Handle, deadline: Instant) -> io::Result<bool>;
    fn signal(&mut self, handle: &Self::Handle, signal: rustix::process::Signal) -> io::Result<()>;
    /// Nonblocking reap, through the same process handle.
    fn reap(&mut self, handle: &Self::Handle, deadline: Instant) -> io::Result<bool>;
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
    fn event(&mut self, _event: ProcessEvent) {}
}

/// Sampling boundaries let synthetic procfs sources deliver named lifecycle events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessEvent {
    FdSampleComplete,
    FdResampleComplete,
    BeforeReap(u32),
}

#[derive(Debug, PartialEq, Eq)]
pub enum BindingVerdict {
    Compliant,
    Violation(String),
    Unstable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskEvidence {
    state: char,
    start: u64,
    children: Vec<u32>,
}
type TreeSnapshot = BTreeMap<(u32, u32), TaskEvidence>;
type Owners = BTreeMap<u64, BTreeSet<(u32, u32)>>;

pub struct ProcessTree<P: ProcessTable> {
    table: P,
    root: u32,
    supervisor: u32,
    excluded: BTreeMap<u32, P::Handle>,
    deadline: Instant,
    handles: BTreeMap<u32, P::Handle>,
    reaped: BTreeSet<u32>,
    cleanup_done: bool,
}
impl<P: ProcessTable> ProcessTree<P> {
    pub fn new(
        table: P,
        root: u32,
        supervisor: u32,
        excluded: BTreeMap<u32, P::Handle>,
        deadline: Instant,
    ) -> Self {
        Self {
            table,
            root,
            supervisor,
            excluded,
            deadline,
            handles: BTreeMap::new(),
            reaped: BTreeSet::new(),
            cleanup_done: false,
        }
    }
    fn check_time(&self) -> io::Result<()> {
        if self.table.now() >= self.deadline {
            Err(deadline_error())
        } else {
            Ok(())
        }
    }
    fn read(&mut self, path: &str) -> io::Result<String> {
        self.check_time()?;
        let result = self.table.read(path);
        self.check_time()?;
        result
    }
    fn list(&mut self, path: &str) -> io::Result<Vec<u32>> {
        self.check_time()?;
        let result = self.table.list(path, self.deadline);
        self.check_time()?;
        let mut ids = result?;
        ids.sort_unstable();
        if ids.contains(&0) || ids.windows(2).any(|ids| ids[0] == ids[1]) {
            return Err(io::Error::other("malformed process/task list"));
        }
        Ok(ids)
    }
    fn children(&mut self, pid: u32, tid: u32) -> io::Result<Vec<u32>> {
        let text = self.read(&format!("/proc/{pid}/task/{tid}/children"))?;
        let mut children = Vec::new();
        for value in text.split_whitespace() {
            self.check_time()?;
            children.push(
                value
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| io::Error::other("malformed children"))?,
            );
        }
        children.sort_unstable();
        if children.windows(2).any(|ids| ids[0] == ids[1]) {
            return Err(io::Error::other("duplicate child"));
        }
        Ok(children)
    }
    fn task_state(&mut self, pid: u32, tid: u32) -> io::Result<(char, u64)> {
        let text = self.read(&format!("/proc/{pid}/task/{tid}/stat"))?;
        let invalid = || io::Error::other("malformed task state");
        let (identity, rest) = text.rsplit_once(')').ok_or_else(invalid)?;
        if identity
            .split_once(" (")
            .and_then(|(id, _)| id.parse::<u32>().ok())
            != Some(tid)
        {
            return Err(invalid());
        }
        let fields: Vec<_> = rest.split_whitespace().collect();
        if fields.len() < 20 {
            return Err(invalid());
        }
        for number in &fields[1..] {
            self.check_time()?;
            let digits = number.strip_prefix('-').unwrap_or(number);
            if digits.is_empty()
                || !digits.bytes().all(|b| b.is_ascii_digit())
                || number.parse::<i128>().is_err()
            {
                return Err(invalid());
            }
        }
        let state = fields
            .first()
            .filter(|s| s.len() == 1)
            .and_then(|s| s.chars().next())
            .filter(|s| "RSDZTtXxKWPI".contains(*s))
            .ok_or_else(invalid)?;
        let start = fields
            .get(19)
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(invalid)?;
        Ok((state, start))
    }
    fn discover(&mut self, pid: u32) -> io::Result<()> {
        self.check_time()?;
        if self.reaped.contains(&pid) {
            return Err(io::Error::other("reaped PID reappeared"));
        }
        if !self.handles.contains_key(&pid) {
            let handle = self.table.open(pid, self.deadline);
            self.check_time()?;
            self.handles.insert(pid, handle?);
        }
        Ok(())
    }
    fn send(&mut self, pid: u32, signal: rustix::process::Signal) -> io::Result<()> {
        self.check_time()?;
        let result = self
            .table
            .signal(self.handles.get(&pid).expect("discovered process"), signal);
        self.check_time()?;
        result
    }
    fn adopted(&mut self) -> io::Result<Vec<u32>> {
        let mut roots = BTreeSet::new();
        for tid in self.list(&format!("/proc/{}/task", self.supervisor))? {
            self.check_time()?;
            for pid in self.children(self.supervisor, tid)? {
                self.check_time()?;
                let outside = match self.excluded.get(&pid) {
                    Some(handle) => self.table.is_current(handle, self.deadline)?,
                    None => false,
                };
                self.check_time()?;
                if !outside {
                    roots.insert(pid);
                }
            }
        }
        Ok(roots.into_iter().collect())
    }
    fn stopped_tree(&mut self) -> io::Result<TreeSnapshot> {
        let mut pending = self.adopted()?;
        pending.push(self.root);
        let mut visited = BTreeSet::new();
        let mut snapshot = BTreeMap::new();
        while let Some(pid) = pending.pop() {
            self.check_time()?;
            if visited.contains(&pid) {
                continue;
            }
            self.discover(pid)?;
            self.send(pid, rustix::process::Signal::STOP)?;
            // STOP delivery is asynchronous. Evidence begins only after all tasks stop.
            let tasks = loop {
                let tids = self.list(&format!("/proc/{pid}/task"))?;
                if tids.is_empty() {
                    return Err(io::Error::other("vanished process"));
                }
                let mut stopped = true;
                for tid in &tids {
                    self.check_time()?;
                    let (state, _) = self.task_state(pid, *tid)?;
                    stopped &= matches!(state, 'T' | 't' | 'Z' | 'X' | 'x');
                }
                if stopped {
                    break tids;
                }
                self.table.sleep(Duration::from_millis(1));
                self.check_time()?;
            };
            let mut zombie = false;
            let mut live = false;
            for tid in tasks {
                self.check_time()?;
                let (state, start) = self.task_state(pid, tid)?;
                zombie |= matches!(state, 'Z' | 'X' | 'x');
                live |= !matches!(state, 'Z' | 'X' | 'x');
                if !matches!(state, 'T' | 't' | 'Z' | 'X' | 'x') {
                    return Err(io::Error::other("task changed state"));
                }
                let children = self.children(pid, tid)?;
                for child in &children {
                    self.check_time()?;
                    pending.push(*child);
                }
                snapshot.insert(
                    (pid, tid),
                    TaskEvidence {
                        state,
                        start,
                        children,
                    },
                );
            }
            if zombie && live {
                return Err(io::Error::other("zombie task with live threads"));
            }
            // A failed task inspection never marks this process visited.
            visited.insert(pid);
        }
        self.check_time()?;
        Ok(snapshot)
    }
    fn verify_tree(&mut self, expected: &TreeSnapshot) -> io::Result<()> {
        // Do not send STOP again: an in-tree state change after fd sampling is evidence.
        let mut current = BTreeMap::new();
        let mut pending = self.adopted()?;
        pending.push(self.root);
        let mut visited = BTreeSet::new();
        while let Some(pid) = pending.pop() {
            self.check_time()?;
            if visited.contains(&pid) {
                continue;
            }
            if !self.handles.contains_key(&pid) {
                return Err(io::Error::other("new PID during inspection"));
            }
            let tids = self.list(&format!("/proc/{pid}/task"))?;
            if tids.is_empty() {
                return Err(io::Error::other("vanished process"));
            }
            for tid in tids {
                self.check_time()?;
                let (state, start) = self.task_state(pid, tid)?;
                let children = self.children(pid, tid)?;
                for child in &children {
                    self.check_time()?;
                    pending.push(*child);
                }
                current.insert(
                    (pid, tid),
                    TaskEvidence {
                        state,
                        start,
                        children,
                    },
                );
            }
            visited.insert(pid);
        }
        if &current != expected {
            return Err(io::Error::other("process/task tree changed"));
        }
        self.check_time()
    }
    fn owners(&mut self, tree: &TreeSnapshot, completion: ProcessEvent) -> io::Result<Owners> {
        let pids = self.list("/proc")?;
        if tree.keys().any(|(pid, _)| !pids.contains(pid)) {
            return Err(io::Error::other("vanished PID"));
        }
        let mut owners: Owners = BTreeMap::new();
        for pid in pids {
            self.check_time()?;
            let in_tree = tree.keys().any(|(id, _)| *id == pid);
            let tids = match self.list(&format!("/proc/{pid}/task")) {
                Ok(tids) => tids,
                Err(error) if in_tree || error.kind() == io::ErrorKind::TimedOut => {
                    return Err(error);
                }
                Err(_) => continue, // Trusted outside owners need positive evidence only.
            };
            for tid in tids {
                self.check_time()?;
                let result = self.table.sockets(pid, tid, self.deadline);
                self.check_time()?;
                match result {
                    Ok(inodes) => {
                        for inode in inodes {
                            self.check_time()?;
                            owners.entry(inode).or_default().insert((pid, tid));
                        }
                    }
                    Err(error) if in_tree => return Err(error),
                    Err(_) => {}
                }
            }
        }
        self.table.event(completion);
        self.check_time()?;
        Ok(owners)
    }
    fn tcp(&mut self) -> io::Result<BTreeSet<TcpEntry>> {
        let mut entries = BTreeSet::new();
        for (path, ipv6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
            let text = self.read(path)?;
            if !text
                .lines()
                .next()
                .is_some_and(|header| header.split_whitespace().next() == Some("sl"))
            {
                return Err(io::Error::other("malformed TCP header"));
            }
            for line in text.lines().skip(1) {
                self.check_time()?;
                let fields: Vec<_> = line.split_whitespace().collect();
                let invalid = || io::Error::other("malformed TCP entry/state");
                if fields.len() < 10 {
                    return Err(invalid());
                }
                let decimal = |s: &str| {
                    !s.is_empty()
                        && s.bytes().all(|b| b.is_ascii_digit())
                        && s.parse::<u64>().is_ok()
                };
                let hex = |s: &str, width: usize| {
                    s.len() == width && s.bytes().all(|b| b.is_ascii_hexdigit())
                };
                let pair = |s: &str, left, right| {
                    s.split_once(':')
                        .is_some_and(|(a, b)| hex(a, left) && hex(b, right))
                };
                if !fields[0].strip_suffix(':').is_some_and(decimal)
                    || !hex(fields[3], 2)
                    || !pair(fields[4], 8, 8)
                    || !pair(fields[5], 2, 8)
                    || !hex(fields[6], 8)
                    || !fields[7..10].iter().all(|field| decimal(field))
                {
                    return Err(invalid());
                }
                let state = u8::from_str_radix(fields[3], 16)
                    .ok()
                    .filter(|s| (1..=12).contains(s))
                    .ok_or_else(invalid)?;
                entries.insert(TcpEntry {
                    local: endpoint(fields[1], ipv6)?,
                    remote: endpoint(fields[2], ipv6)?,
                    state,
                    inode: fields[9].parse().map_err(|_| invalid())?,
                    evidence: fields[4..]
                        .iter()
                        .map(|field| (*field).to_owned())
                        .collect(),
                });
            }
        }
        self.check_time()?;
        Ok(entries)
    }
    fn binding_sample(&mut self, port: u16) -> io::Result<BindingVerdict> {
        let tree = self.stopped_tree()?;
        let first = self.tcp()?;
        let owners = self.owners(&tree, ProcessEvent::FdSampleComplete)?;
        // A positively observed violation is never erased by later samples.
        for entry in first.iter().filter(|entry| entry.state == 10) {
            self.check_time()?;
            if entry.local != SocketAddr::from((Ipv4Addr::LOCALHOST, port))
                && owners
                    .get(&entry.inode)
                    .is_some_and(|owners| owners.iter().any(|owner| tree.contains_key(owner)))
            {
                return Ok(BindingVerdict::Violation(format!(
                    "candidate is not listening exclusively on 127.0.0.1:$PORT: listener {}",
                    entry.local
                )));
            }
        }
        self.verify_tree(&tree)?;
        let second_owners = self.owners(&tree, ProcessEvent::FdResampleComplete)?;
        // These reads must follow the LAST fd sample.
        let second = self.tcp()?;
        self.verify_tree(&tree)?;
        if first != second {
            return Err(io::Error::other("TCP tables changed after fd sample"));
        }
        for entry in &first {
            self.check_time()?;
            if entry.inode > 0 && owners.get(&entry.inode) != second_owners.get(&entry.inode) {
                return Err(io::Error::other("socket moved between owners"));
            }
        }
        let mut found = false;
        for entry in first.iter().filter(|entry| entry.state == 10) {
            self.check_time()?;
            let owner = owners
                .get(&entry.inode)
                .filter(|owners| !owners.is_empty())
                .ok_or_else(|| {
                    io::Error::other(format!("unknown listener owner: {}", entry.local))
                })?;
            if second_owners.get(&entry.inode) != Some(owner) {
                return Err(io::Error::other("listener moved between owners"));
            }
            if owner.iter().any(|owner| tree.contains_key(owner)) {
                found |= entry.local == SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            }
        }
        self.check_time()?;
        Ok(if found {
            BindingVerdict::Compliant
        } else {
            BindingVerdict::Violation("candidate has no listener on 127.0.0.1:$PORT".into())
        })
    }
    pub fn inspect_binding(&mut self, port: u16) -> io::Result<BindingVerdict> {
        let mut unstable = None;
        let mut result = Ok(BindingVerdict::Unstable("inspection unstable".into()));
        for _ in 0..5 {
            self.check_time()?;
            match self.binding_sample(port) {
                Ok(BindingVerdict::Compliant) if unstable.is_none() => {
                    result = Ok(BindingVerdict::Compliant);
                    break;
                }
                Ok(BindingVerdict::Compliant) => {}
                Ok(verdict) => {
                    result = Ok(verdict);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    result = Err(error);
                    break;
                }
                Err(error) => {
                    unstable = Some(error.to_string());
                }
            }
            if let Some(detail) = &unstable {
                result = Ok(BindingVerdict::Unstable(format!(
                    "inspection unstable: {detail}"
                )));
            }
        }
        // Retained handles make every resume immune to PID reuse as well.
        for pid in self.handles.keys().copied().collect::<Vec<_>>() {
            if let Err(error) = self.send(pid, rustix::process::Signal::CONT) {
                if error.kind() == io::ErrorKind::TimedOut {
                    result = Err(error);
                } else {
                    result = Ok(BindingVerdict::Unstable(format!(
                        "inspection unstable: cannot resume process: {error}"
                    )));
                }
            }
        }
        self.check_time()?;
        result
    }
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TcpEntry {
    local: SocketAddr,
    remote: SocketAddr,
    state: u8,
    inode: u64,
    evidence: Vec<String>,
}
fn deadline_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "conformance deadline exceeded")
}

impl<P: ProcessTable> ProcessTree<P> {
    fn reap_known(&mut self) -> io::Result<bool> {
        let mut changed = false;
        for pid in self.handles.keys().copied().collect::<Vec<_>>() {
            self.check_time()?;
            self.table.event(ProcessEvent::BeforeReap(pid));
            self.check_time()?;
            let result = self.table.reap(
                self.handles.get(&pid).expect("discovered process"),
                self.deadline,
            );
            self.check_time()?;
            if result? {
                self.handles.remove(&pid);
                self.reaped.insert(pid);
                changed = true;
            }
        }
        Ok(changed)
    }
    /// Only our own unreaped children are safe numeric discovery roots. Killing and
    /// reaping each root adopts its surviving descendants; never walk a cached
    /// descendant PID that its untrusted parent could already have reaped/reused.
    fn refresh_cleanup(&mut self) -> io::Result<bool> {
        let mut roots = self.adopted()?;
        if !self.reaped.contains(&self.root) {
            roots.push(self.root);
        }
        let mut complete = true;
        for pid in roots {
            self.check_time()?;
            if let Err(error) = self.discover(pid) {
                if error.kind() == io::ErrorKind::TimedOut {
                    return Err(error);
                }
                complete = false;
            }
        }
        self.check_time()?;
        Ok(complete)
    }
    fn cleanup_time(&self, until: Instant) -> io::Result<()> {
        self.check_time()?;
        if self.table.now() >= until {
            Err(io::Error::other(
                "candidate cleanup incomplete within 1 s bound",
            ))
        } else {
            Ok(())
        }
    }
    fn cleanup_until(&mut self, until: Instant) -> io::Result<()> {
        // Mark BEFORE doing work: errors and exhausted budgets must not restart in Drop.
        self.cleanup_done = true;
        let mut empty_scans = 0;
        loop {
            self.cleanup_time(until)?;
            let complete = match self.refresh_cleanup() {
                Ok(complete) => complete,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => return Err(error),
                Err(_) => false,
            };
            self.cleanup_time(until)?;
            let reaped_before = self.reap_known()?;
            for pid in self.handles.keys().copied().collect::<Vec<_>>() {
                self.cleanup_time(until)?;
                if let Err(error) = self.send(pid, rustix::process::Signal::KILL)
                    && error.raw_os_error() != Some(rustix::io::Errno::SRCH.raw_os_error())
                {
                    return Err(error);
                }
            }
            self.cleanup_time(until)?;
            let reaped_after = self.reap_known()?;
            let adopted = self.adopted()?;
            self.cleanup_time(until)?;
            if complete
                && !reaped_before
                && !reaped_after
                && adopted.is_empty()
                && self.handles.is_empty()
                && self.reaped.contains(&self.root)
            {
                empty_scans += 1;
                if empty_scans == 2 {
                    self.cleanup_time(until)?;
                    return Ok(());
                }
            } else {
                empty_scans = 0;
            }
            self.table.sleep(Duration::from_millis(1));
        }
    }
    pub fn cleanup(&mut self) -> io::Result<()> {
        if self.cleanup_done {
            return Err(io::Error::other("candidate cleanup already attempted"));
        }
        self.cleanup_until(self.deadline.min(self.table.now() + CLEANUP_BOUND))
    }
    pub fn terminate(&mut self) -> io::Result<bool> {
        if self.cleanup_done {
            return Err(io::Error::other("candidate cleanup already attempted"));
        }
        // Reserve the kill/reap allowance inside the existing absolute deadline.
        let grace = self
            .deadline
            .checked_sub(CLEANUP_BOUND)
            .unwrap_or(self.deadline)
            .min(self.table.now() + TERM_GRACE);
        let graceful = (|| {
            if let Err(error) = self.refresh_cleanup()
                && error.kind() == io::ErrorKind::TimedOut
            {
                return Err(error);
            }
            self.reap_known()?;
            let mut signalled = BTreeSet::new();
            loop {
                self.check_time()?;
                for pid in self.handles.keys().copied().collect::<Vec<_>>() {
                    self.check_time()?;
                    if signalled.insert(pid)
                        && let Err(error) = self.send(pid, rustix::process::Signal::TERM)
                        && error.raw_os_error() != Some(rustix::io::Errno::SRCH.raw_os_error())
                    {
                        return Err(error);
                    }
                }
                self.reap_known()?;
                if self.reaped.contains(&self.root) {
                    self.check_time()?;
                    return Ok(true);
                }
                if self.table.now() >= grace {
                    return Ok(false);
                }
                self.table.sleep(Duration::from_millis(1));
                if let Err(error) = self.refresh_cleanup()
                    && error.kind() == io::ErrorKind::TimedOut
                {
                    return Err(error);
                }
            }
        })();
        // The grace and kill bound only tighten the original absolute deadline.
        let until = self
            .deadline
            .min(grace + CLEANUP_BOUND)
            .min(self.table.now() + CLEANUP_BOUND);
        self.cleanup_until(until)?;
        self.check_time()?;
        graceful
    }
}
impl<P: ProcessTable> Drop for ProcessTree<P> {
    fn drop(&mut self) {
        if !self.cleanup_done {
            let _ = self.cleanup();
        }
    }
}
