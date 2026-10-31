//! Standalone OCI v1 conformance boundary; not connected to the execution worker.
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
pub type Diagnostics = BTreeMap<&'static str, &'static str>;
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

fn utc_deadline(text: &str) -> bool {
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

fn probe(
    port: u16,
    method: &str,
    path: &str,
    body: &[u8],
    content_type: &str,
) -> io::Result<Response> {
    exchange(
        port,
        method,
        path,
        body,
        content_type,
        Instant::now() + IO_BUDGET,
    )
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
    let mut diagnostics = Diagnostics::new();
    let startup = Instant::now() + STARTUP_BUDGET;
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
    match probe(port, "POST", "/invoke", &payload, "application/json") {
        Ok(response) => inspect_response(response, &mut diagnostics),
        Err(_) => {
            diagnostics.insert("OCI-V1-INVOKE", "POST /invoke failed or timed out");
        }
    }
    for (method, path) in [("GET", "/invoke"), ("POST", "/__conformance_missing__")] {
        if !probe(port, method, path, &payload, "application/json")
            .is_ok_and(|r| matches!(r.status, 404 | 405))
        {
            diagnostics.insert(
                "OCI-V1-INVOKE",
                "wrong method/path was not rejected with 404/405",
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
        if !probe(port, "POST", "/invoke", &body, "application/json").is_ok_and(|r| r.status == 400)
        {
            diagnostics.insert(
                "OCI-V1-REQUEST",
                "invalid request/context was not rejected with 400",
            );
            break;
        }
    }
    if !probe(port, "POST", "/invoke", &payload, "text/plain").is_ok_and(|r| r.status == 400) {
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
        match probe(port, "POST", "/invoke", &body, "application/json") {
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
        println!("OCI-V1-PORT: $PORT is mandatory, decimal 1–65535, with no default");
        return 1;
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
        Ok(diagnostics) => {
            for (rule, message) in &diagnostics {
                println!("{rule}: {message}");
            }
            i32::from(!diagnostics.is_empty())
        }
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
    let probe = FilesystemProbe::new()?;
    let mut child = Candidate::spawn(port, executable, args, &probe)?;
    let mut diagnostics = check_http(port);
    // Health failures must not cascade into fictitious binding failures.
    if !diagnostics.contains_key("OCI-V1-HEALTHZ") {
        match (
            fs::read_to_string("/proc/net/tcp"),
            fs::read_to_string("/proc/net/tcp6"),
        ) {
            (Ok(tcp), Ok(tcp6)) => diagnostics.extend(check_binding_snapshot(&tcp, &tcp6, port)),
            _ => {
                diagnostics.insert("OCI-V1-BINDING", "cannot inspect Linux listening sockets");
            }
        }
    }
    if !child.terminate()? {
        diagnostics.insert(
            "OCI-V1-SIGTERM",
            "candidate ignored SIGTERM beyond 10 s grace",
        );
    }
    // Kill/reap before looking at the sentinel: no race with a surviving writer.
    drop(child);
    if !probe.unchanged().unwrap_or(false) {
        diagnostics.insert("OCI-V1-FILESYSTEM", "candidate changed cwd/HOME sentinel");
    }
    Ok(diagnostics)
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
    baseline: Option<(Stamp, Stamp)>,
}
impl FilesystemProbe {
    pub fn new() -> io::Result<Self> {
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
}

impl Drop for FilesystemProbe {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn check_binding_snapshot(tcp: &str, tcp6: &str, port: u16) -> Diagnostics {
    let mut seen = false;
    let mut invalid = false;
    for (table, ipv6) in [(tcp, false), (tcp6, true)] {
        for line in table.lines().skip(1) {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.get(3) != Some(&"0A") {
                continue;
            }
            let Some((address, value)) = fields.get(1).and_then(|s| s.split_once(':')) else {
                continue;
            };
            if u16::from_str_radix(value, 16) != Ok(port) {
                continue;
            }
            seen = true;
            invalid |= ipv6 || address != "0100007F";
        }
    }
    let mut diagnostics = Diagnostics::new();
    if !seen || invalid {
        diagnostics.insert(
            "OCI-V1-BINDING",
            "candidate is not listening exclusively on 127.0.0.1:$PORT",
        );
    }
    diagnostics
}

pub struct Candidate {
    child: std::process::Child,
    group: rustix::process::Pid,
}
impl Candidate {
    pub fn spawn(
        port: u16,
        executable: &std::ffi::OsStr,
        args: &[OsString],
        probe: &FilesystemProbe,
    ) -> io::Result<Self> {
        use rustix::process::{Pid, set_child_subreaper};
        use std::os::unix::process::CommandExt;
        if !Path::new(executable).is_absolute() {
            return Err(io::Error::other(
                "candidate executable must be an absolute path",
            ));
        }
        // Adopt orphaned descendants so cleanup can reap this process group too.
        set_child_subreaper(Some(Pid::from_raw(1).unwrap()))?;
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
        let group = Pid::from_child(&child);
        Ok(Self { child, group })
    }
    pub fn pid(&self) -> rustix::process::Pid {
        self.group
    }
    pub fn terminate(&mut self) -> io::Result<bool> {
        use rustix::process::{Signal, kill_process};
        if self.child.try_wait()?.is_some() {
            return Ok(true);
        }
        if let Err(error) = kill_process(self.group, Signal::TERM)
            && error != rustix::io::Errno::SRCH
        {
            return Err(error.into());
        }
        let deadline = Instant::now() + TERM_GRACE;
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        use rustix::process::{Signal, WaitOptions, kill_process_group, waitpgid};
        let _ = kill_process_group(self.group, Signal::KILL);
        let _ = self.child.wait();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match waitpgid(self.group, WaitOptions::NOHANG) {
                Ok(Some(_)) => continue,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Err(rustix::io::Errno::INTR) => continue,
                _ => break,
            }
        }
    }
}
