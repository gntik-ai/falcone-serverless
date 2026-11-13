//! HTTP contract cases shared by local tests and the restricted RUN-01 pod runner.
use falcone_serverless_worker::{
    api,
    server::{Server, Settings},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

pub struct Fixture {
    pub server: Server,
    pub request: Value,
    root: PathBuf,
}

impl Fixture {
    pub fn stage(&self, bytes: &[u8]) -> Value {
        let digest = format!("sha256:{:x}", Sha256::digest(bytes));
        std::fs::write(self.root.join("components").join(&digest[7..]), bytes).unwrap();
        let mut request = self.request.clone();
        request["projection"]["component_digest"] = digest.into();
        request
    }
    pub fn new(bytes: &[u8], concurrency: usize, wall_clock_ms: u64) -> Self {
        use ring::rand::SecureRandom;
        let mut nonce = [0; 16];
        ring::rand::SystemRandom::new().fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("run01-{:032x}", u128::from_be_bytes(nonce)));
        std::fs::create_dir(&root).unwrap();
        let digest = format!("sha256:{:x}", Sha256::digest(bytes));
        std::fs::create_dir(root.join("components")).unwrap();
        std::fs::write(root.join("components").join(&digest[7..]), bytes).unwrap();
        let exe = std::env::current_exe().unwrap();
        let parent = exe.parent().unwrap();
        let binaries = if parent.file_name().is_some_and(|name| name == "deps") {
            parent.parent().unwrap()
        } else {
            parent
        };
        let server = Server::start(Settings {
            execute_addr: "127.0.0.1:0".parse().unwrap(),
            control_addr: "127.0.0.1:0".parse().unwrap(),
            concurrency,
            wall_clock_ms,
            artifact_directory: root.join("components"),
            cache_directory: root.join("cache"),
            compiler: binaries.join("artifact-compile-child"),
        })
        .unwrap();
        let mut request: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/contract/request/valid/wasm-no-base.json"
        ))
        .unwrap();
        request["projection"]["component_digest"] = digest.into();
        request["projection"]["imports"] = json!([]);
        request["projection"]["grants"] = json!([]);
        request["timeout_ms"] = json!(30000);
        Self {
            server,
            request,
            root,
        }
    }

    pub fn execute(&self) -> (u16, Value) {
        request(self.server.execute_addr(), "/v1/execute", &self.request)
    }
}

pub fn wait(
    fixture: &Fixture,
    listener: falcone_serverless_worker::server::Listener,
    phase: falcone_serverless_worker::server::Phase,
    count: usize,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(330);
    let mut seen = 0;
    while seen < count {
        let event = fixture
            .server
            .next_event(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if event.listener == listener && event.phase == phase {
            seen += 1;
        }
    }
}

fn cancel(fixture: &Fixture, execute: &Value) -> (u16, Value) {
    request(
        fixture.server.control_addr(),
        "/v1/cancel",
        &json!({"version":"1","type":"cancel",
        "activation_id":execute["activation_id"],"attempt_id":execute["attempt_id"],"cancel":{"state":"requested"}}),
    )
}

pub fn saturation() {
    use falcone_serverless_worker::server::{
        CONTROL_CONNECTIONS, EXECUTE_ADMISSION, Listener, Phase,
    };
    let fixture = Fixture::new(
        include_bytes!("execution_api_fixtures/success.wasm"),
        2,
        30000,
    );
    let mut blocked = fixture.stage(include_bytes!("execution_api_fixtures/blocking.wasm"));
    let mut executions = Vec::new();
    for n in 0..2 {
        let activation = format!("blocked-{n}");
        blocked["activation_id"] = json!(activation);
        blocked["credential"]["activation"] = json!(activation);
        let value = blocked.clone();
        let addr = fixture.server.execute_addr();
        executions.push((
            value.clone(),
            std::thread::spawn(move || request(addr, "/v1/execute", &value)),
        ));
    }
    wait(&fixture, Listener::Execute, Phase::GuestStarted, 2);
    let update = json!({"version":"1","type":"update","activation_id":executions[0].0["activation_id"],
        "attempt_id":executions[0].0["attempt_id"],"sequence":1,"update":{"kind":"heartbeat"}});
    assert_eq!(
        request(fixture.server.control_addr(), "/v1/update", &update),
        (200, update.clone())
    );
    assert_eq!(
        request(fixture.server.control_addr(), "/v1/update", &update).0,
        409
    );
    for (selector, expected) in [("wasm", 503), ("unknown", 409)] {
        let mut stalled_body = TcpStream::connect(fixture.server.execute_addr()).unwrap();
        stalled_body
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(stalled_body, "POST /v1/execute HTTP/1.1\r\nContent-Length: 8388608\r\nX-Falcone-Runtime-Mode: {selector}\r\n\r\n").unwrap();
        assert_eq!(response(&mut stalled_body).0, expected);
    }
    // Discard lifecycle events for the just-completed admission checks.
    while fixture.server.next_event(Duration::ZERO).is_some() {}
    let stalls: Vec<_> = (0..EXECUTE_ADMISSION)
        .map(|_| TcpStream::connect(fixture.server.execute_addr()).unwrap())
        .collect();
    wait(
        &fixture,
        Listener::Execute,
        Phase::Accepted,
        EXECUTE_ADMISSION,
    );
    let mut extra = TcpStream::connect(fixture.server.execute_addr()).unwrap();
    extra
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    // No request bytes: overflow cannot read a body or consume a thread.
    assert_eq!(response(&mut extra).0, 503);
    let start = std::time::Instant::now();
    let (status, reply) = cancel(&fixture, &executions[0].0);
    assert_eq!(status, 200);
    assert!(matches!(
        reply["cancel"]["state"].as_str(),
        Some("acknowledged" | "uncertain")
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
    let controls: Vec<_> = (0..CONTROL_CONNECTIONS)
        .map(|_| TcpStream::connect(fixture.server.control_addr()).unwrap())
        .collect();
    wait(
        &fixture,
        Listener::Control,
        Phase::Accepted,
        CONTROL_CONNECTIONS + 1,
    );
    // Wait for the server's deadline events, never a guessed scheduling delay.
    wait(
        &fixture,
        Listener::Control,
        Phase::Closed,
        CONTROL_CONNECTIONS,
    );
    drop(controls);
    assert_eq!(cancel(&fixture, &executions[1].0).0, 200);
    for (_, thread) in executions {
        assert_eq!(thread.join().unwrap().0, 499);
    }
    drop(stalls);
    assert_eq!(fixture.execute().0, 200);
    assert_eq!(cancel(&fixture, &fixture.request).0, 200);
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.shutdown();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

pub fn request(addr: std::net::SocketAddr, path: &str, value: &Value) -> (u16, Value) {
    let body = serde_json::to_vec(value).unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(330)))
        .unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    response(&mut stream)
}

pub fn response(stream: &mut TcpStream) -> (u16, Value) {
    let mut bytes = Vec::new();
    if let Err(error) = stream.read_to_end(&mut bytes) {
        assert!(
            error.kind() == std::io::ErrorKind::ConnectionReset && !bytes.is_empty(),
            "response transport: {error}"
        );
    }
    let split = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap();
    let header = std::str::from_utf8(&bytes[..split]).unwrap();
    let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_slice(&bytes[split + 4..]).unwrap())
}

fn recovery(fixture: &Fixture) {
    assert_eq!(fixture.execute().0, 200);
    assert_eq!(cancel(fixture, &fixture.request).0, 200);
    while fixture.server.next_event(Duration::ZERO).is_some() {}
}

pub fn ceilings_and_transfer_deadlines() {
    use std::time::Instant;
    let fixture = Fixture::new(
        include_bytes!("execution_api_fixtures/success.wasm"),
        1,
        30000,
    );
    for (name, addr, input) in [
        (
            "http-header-size",
            fixture.server.execute_addr(),
            format!(
                "POST /v1/execute HTTP/1.1\r\nX-Large: {}",
                "x".repeat(16384)
            ),
        ),
        (
            "http-request-size",
            fixture.server.execute_addr(),
            "POST /v1/execute HTTP/1.1\r\nContent-Length: 8388609\r\n\r\n".into(),
        ),
        (
            "http-control-body-size",
            fixture.server.control_addr(),
            "POST /v1/cancel HTTP/1.1\r\nContent-Length: 16385\r\n\r\n".into(),
        ),
    ] {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(7)))
            .unwrap();
        let _ = stream.write_all(input.as_bytes());
        let mut bytes = Vec::new();
        let result = stream.read_to_end(&mut bytes);
        assert!(
            result.is_ok()
                || result
                    .as_ref()
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset)
        );
        if !bytes.is_empty() {
            let status = std::str::from_utf8(&bytes)
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            assert!(matches!(status, "400" | "413" | "431"));
        }
        no_guest(&fixture);
        recovery(&fixture);
        println!("{}", json!({"case":name,"status":"passed"}));
    }
    let mut oversized = fixture.request.clone();
    oversized["params"]["__ow_body"] = json!("x".repeat(1048577));
    assert_eq!(
        request(fixture.server.execute_addr(), "/v1/execute", &oversized).0,
        413
    );
    no_guest(&fixture);
    recovery(&fixture);
    println!(
        "{}",
        json!({"case":"http-action-body-size","status":"passed"})
    );
    let mut params = fixture.request.clone();
    params["params"]["input"] = json!("x".repeat(1048576));
    assert_eq!(
        request(fixture.server.execute_addr(), "/v1/execute", &params).0,
        413
    );
    no_guest(&fixture);
    recovery(&fixture);
    println!("{}", json!({"case":"http-params-size","status":"passed"}));
    for addr in [fixture.server.execute_addr(), fixture.server.control_addr()] {
        for mode in ["idle", "dripped-header", "stalled-body", "dripped-body"] {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(if mode == "dripped-body" {
                    13
                } else {
                    7
                })))
                .unwrap();
            let start = Instant::now();
            if matches!(mode, "stalled-body" | "dripped-body") {
                let path = if addr == fixture.server.execute_addr() {
                    "/v1/execute"
                } else {
                    "/v1/cancel"
                };
                write!(
                    stream,
                    "POST {path} HTTP/1.1\r\nContent-Length: 100\r\n\r\nx"
                )
                .unwrap();
            }
            let dripper = if matches!(mode, "dripped-header" | "dripped-body") {
                let mut sender = stream.try_clone().unwrap();
                let traffic: &[u8] = if mode == "dripped-body" {
                    b"xxxxxxxxxxxxxxxxx"
                } else {
                    b"POST /v1"
                };
                Some(std::thread::spawn(move || {
                    for byte in traffic {
                        if sender.write_all(&[*byte]).is_err() {
                            break;
                        }
                        // Deliberate traffic pacing, not test synchronization.
                        std::thread::park_timeout(Duration::from_millis(750));
                    }
                }))
            } else {
                None
            };
            let mut byte = [0];
            let result = stream.read(&mut byte);
            assert!(
                matches!(result, Ok(0))
                    || result
                        .as_ref()
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset),
                "{mode}: {result:?}"
            );
            let bound = match mode {
                "stalled-body" => 4,
                "dripped-body" => 12,
                _ => 7,
            };
            assert!(
                start.elapsed() < Duration::from_secs(bound),
                "{mode} deadline"
            );
            if let Some(dripper) = dripper {
                dripper.join().unwrap();
            }
            no_guest(&fixture);
            recovery(&fixture);
            let listener = if addr == fixture.server.execute_addr() {
                "execute"
            } else {
                "control"
            };
            println!(
                "{}",
                json!({"case":format!("{listener}-{mode}"),"status":"passed"})
            );
        }
    }
}

pub fn timeout_selection() {
    for (request_ms, projection_ms, worker_ms) in
        [(1000, 5000, 5000), (5000, 1000, 5000), (5000, 5000, 1000)]
    {
        let mut fixture = Fixture::new(
            include_bytes!("execution_api_fixtures/blocking.wasm"),
            1,
            worker_ms,
        );
        fixture.request["timeout_ms"] = json!(request_ms);
        fixture.request["projection"]["budgets"]["timeout_ms"] = json!(projection_ms);
        // Warm the trusted compilation cache outside the measured invocation.
        assert_eq!(fixture.execute().0, 504);
        let start = std::time::Instant::now();
        let (status, value) = fixture.execute();
        assert_eq!(status, 504);
        assert_eq!(value["result"]["code"], "timeout");
        assert_eq!(value["activation_id"], fixture.request["activation_id"]);
        assert_eq!(value["attempt_id"], fixture.request["attempt_id"]);
        assert_eq!(value["identity"]["wasmtime_version"], "49.0.5");
        let pinned = api::parse_request(&serde_json::to_vec(&fixture.request).unwrap()).unwrap();
        api::parse_result(&serde_json::to_vec(&value).unwrap(), &pinned).unwrap();
        assert!((Duration::from_millis(900)..Duration::from_secs(3)).contains(&start.elapsed()));
        let success = fixture.stage(include_bytes!("execution_api_fixtures/success.wasm"));
        assert_eq!(
            request(fixture.server.execute_addr(), "/v1/execute", &success).0,
            200
        );
        let name = if request_ms == 1000 {
            "timeout-request"
        } else if projection_ms == 1000 {
            "timeout-projection"
        } else {
            "timeout-worker"
        };
        println!("{}", json!({"case":name,"status":"passed"}));
    }
}

pub fn slow_readers() {
    use falcone_serverless_worker::server::{Listener, Phase};
    let mut fixture = Fixture::new(include_bytes!("execution_api_fixtures/logs.wasm"), 2, 30000);
    fixture.request["projection"]["imports"] = json!(["falcone:host/logging@0.2.0"]);
    let mut readers = Vec::new();
    for n in 0..2 {
        let mut value = fixture.request.clone();
        value["activation_id"] = json!(format!("slow-{n}"));
        value["credential"]["activation"] = value["activation_id"].clone();
        let body = serde_json::to_vec(&value).unwrap();
        let mut stream = TcpStream::connect(fixture.server.execute_addr()).unwrap();
        rustix::net::sockopt::set_socket_recv_buffer_size(&stream, 16 * 1024).unwrap();
        write!(
            stream,
            "POST /v1/execute HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        readers.push(stream);
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let threads: Vec<_> = readers
        .into_iter()
        .map(|mut stream| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                stream
                    .set_read_timeout(Some(Duration::from_secs(7)))
                    .unwrap();
                let mut bytes = Vec::new();
                while !stop.load(std::sync::atomic::Ordering::Acquire) {
                    let mut byte = [0];
                    match stream.read(&mut byte) {
                        Ok(1) => bytes.push(byte[0]),
                        _ => break,
                    }
                    // An intentionally slow reader, not a scheduling assumption.
                    std::thread::park_timeout(Duration::from_millis(50));
                }
                if let Err(error) = stream.read_to_end(&mut bytes) {
                    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
                }
                let split = bytes
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .unwrap();
                let header = std::str::from_utf8(&bytes[..split]).unwrap();
                let length: usize = header
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(
                    length > 8_000_000,
                    "fixture must exercise a maximum-size response"
                );
                assert!(
                    bytes.len() - split - 4 < length,
                    "slow response was not cut off"
                );
            })
        })
        .collect();
    // Preserve close events even if one response finishes before the other
    // starts. No phase-filtered wait can accidentally discard that evidence.
    let deadline = std::time::Instant::now() + Duration::from_secs(330);
    let mut started = Vec::new();
    let mut closed = 0;
    while closed < 2 {
        let event = fixture
            .server
            .next_event(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if event.listener == Listener::Execute {
            if event.phase == Phase::ResponseStarted {
                started.push(std::time::Instant::now());
            }
            if event.phase == Phase::Closed {
                assert!(!started.is_empty());
                assert!(started.remove(0).elapsed() < Duration::from_secs(7));
                closed += 1;
            }
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Release);
    for thread in threads {
        thread.join().unwrap();
    }
    let success = fixture.stage(include_bytes!("execution_api_fixtures/success.wasm"));
    assert_eq!(
        request(fixture.server.execute_addr(), "/v1/execute", &success).0,
        200
    );
    assert_eq!(cancel(&fixture, &fixture.request).0, 200);
}

fn no_guest(fixture: &Fixture) {
    use falcone_serverless_worker::server::Phase;
    while let Some(event) = fixture.server.next_event(Duration::ZERO) {
        assert_ne!(event.phase, Phase::GuestStarted);
    }
}

pub fn refusals() {
    let fixture = Fixture::new(
        include_bytes!("execution_api_fixtures/success.wasm"),
        1,
        30000,
    );
    for mode in [json!("oci"), json!("unknown"), json!(false)] {
        let value = json!({"projection":{"runtime_mode":mode},"credential":"malformed"});
        let (status, reply) = request(fixture.server.execute_addr(), "/v1/execute", &value);
        assert_eq!(status, 409);
        assert_eq!(reply["code"], "runtime_mode_not_supported");
        no_guest(&fixture);
    }
    let image = json!({"projection":{"runtime_mode":"wasm","image":"registry.invalid/image"}});
    assert_eq!(
        request(fixture.server.execute_addr(), "/v1/execute", &image).0,
        409
    );
    no_guest(&fixture);
    let mut missing = fixture.request.clone();
    missing["params"]
        .as_object_mut()
        .unwrap()
        .remove("__ow_path");
    assert_eq!(
        request(fixture.server.execute_addr(), "/v1/execute", &missing).0,
        400
    );
    no_guest(&fixture);
    let mut invalid = fixture.request.clone();
    invalid["params"]["__ow_headers"] = json!({"content-type":"application/json"});
    invalid["params"]["__ow_body"] = json!("{");
    assert_eq!(
        request(fixture.server.execute_addr(), "/v1/execute", &invalid).0,
        400
    );
    no_guest(&fixture);
    invalid["params"]["__ow_body"] = json!("x".repeat(1048577));
    assert_eq!(
        request(fixture.server.execute_addr(), "/v1/execute", &invalid).0,
        413
    );
    no_guest(&fixture);
    recovery(&fixture);
}

pub fn declared_imports() {
    let fixture = Fixture::new(
        include_bytes!("execution_api_fixtures/success.wasm"),
        1,
        30000,
    );
    for (bytes, declared, status) in [
        (
            include_bytes!("execution_api_fixtures/context.wasm").as_slice(),
            json!([]),
            422,
        ),
        (
            include_bytes!("execution_api_fixtures/context.wasm").as_slice(),
            json!(["falcone:host/logging@0.2.0"]),
            422,
        ),
        (
            include_bytes!("execution_api_fixtures/environment.wasm").as_slice(),
            json!(["wasi:cli/environment@0.2.6"]),
            422,
        ),
        (
            include_bytes!("execution_api_fixtures/context.wasm").as_slice(),
            json!(["falcone:host/invocation-context@0.2.0"]),
            200,
        ),
    ] {
        let mut value = fixture.stage(bytes);
        value["projection"]["imports"] = declared;
        let (actual, reply) = request(fixture.server.execute_addr(), "/v1/execute", &value);
        assert_eq!(actual, status);
        if status == 422 {
            assert_eq!(reply["result"]["code"], "undeclared_import");
            no_guest(&fixture);
        } else {
            while fixture.server.next_event(Duration::ZERO).is_some() {}
        }
        recovery(&fixture);
    }
    let mut scoped = fixture.stage(include_bytes!("execution_api_fixtures/storage.wasm"));
    scoped["projection"]["imports"] = json!(["falcone:host/storage@0.2.0"]);
    scoped["projection"]["grants"] = json!([{"family":"storage","interface":"falcone:host/storage@0.2.0","scope":{"prefixes":["tenants/synthetic/"]}}]);
    assert_eq!(
        request(fixture.server.execute_addr(), "/v1/execute", &scoped).0,
        200
    );
    recovery(&fixture);
}

pub fn credential_isolation() {
    let mut fixture = Fixture::new(
        include_bytes!("execution_api_fixtures/credential.wasm"),
        1,
        30000,
    );
    fixture.request["credential"]["iss"] = json!("credOnlyRun01HostCanary");
    let (status, reply) = fixture.execute();
    assert_eq!(
        status, 200,
        "guest scans its entire linear memory for the canary"
    );
    assert!(!reply.to_string().contains("credOnlyRun01HostCanary"));
    while fixture.server.next_event(Duration::ZERO).is_some() {}
    for (bytes, import) in [
        (
            include_bytes!("execution_api_fixtures/environment.wasm").as_slice(),
            "wasi:cli/environment@0.2.6",
        ),
        (
            include_bytes!("execution_api_fixtures/files.wasm").as_slice(),
            "wasi:filesystem/preopens@0.2.6",
        ),
    ] {
        let mut value = fixture.stage(bytes);
        value["projection"]["imports"] = json!([import]);
        assert_eq!(
            request(fixture.server.execute_addr(), "/v1/execute", &value).0,
            422
        );
        no_guest(&fixture);
        recovery(&fixture);
    }
}

pub fn redaction_budget() {
    let mut fixture = Fixture::new(include_bytes!("execution_api_fixtures/logs.wasm"), 1, 30000);
    fixture.request["projection"]["imports"] = json!(["falcone:host/logging@0.2.0"]);
    assert_eq!(fixture.execute().0, 200);
    let small = fixture.request.clone();
    fixture.request["credential"]["allowed_capabilities"][0]["scope"]["hosts"] = json!([]);
    let baseline = serde_json::to_vec(&fixture.request).unwrap().len();
    let count = (8 * 1024 * 1024 - baseline + 1) / 12;
    fixture.request["credential"]["allowed_capabilities"][0]["scope"]["hosts"] =
        json!((0..count).map(|n| format!("s{n:08x}")).collect::<Vec<_>>());
    assert!(serde_json::to_vec(&fixture.request).unwrap().len() <= 8 * 1024 * 1024);
    let start = std::time::Instant::now();
    let (status, value) = fixture.execute();
    assert_eq!(status, 200);
    assert_eq!(value["logs"], json!(["<redacted>"]));
    assert!(start.elapsed() < Duration::from_secs(10));
    fixture.request = small;
    recovery(&fixture);
}

pub const CASES: &[(&str, fn())] = &[
    ("identity", identity),
    ("refusals", refusals),
    ("declared-imports", declared_imports),
    ("credential-isolation", credential_isolation),
    ("redaction-budget", redaction_budget),
    ("listener-routes", listener_routes),
    ("saturation", saturation),
    ("http-ceilings-deadlines", ceilings_and_transfer_deadlines),
    ("slow-readers", slow_readers),
    ("timeout-selection", timeout_selection),
];

pub fn run() -> std::process::ExitCode {
    for (name, case) in CASES {
        case();
        println!("{}", json!({"case":name,"status":"passed"}));
    }
    println!("{}", json!({"check":"execution_api","status":"passed"}));
    std::process::ExitCode::SUCCESS
}

pub fn identity() {
    let fixture = Fixture::new(
        include_bytes!("execution_api_fixtures/success.wasm"),
        1,
        30000,
    );
    let (status, value) = fixture.execute();
    assert_eq!(status, 200);
    assert_eq!(value["result"], json!({"statusCode":200,"body":"ok"}));
    assert_eq!(value["identity"]["wasmtime_version"], "49.0.5");
    assert_eq!(
        value["identity"]["function_revision"],
        fixture.request["projection"]["revision"]
    );
    let pinned = api::parse_request(&serde_json::to_vec(&fixture.request).unwrap()).unwrap();
    api::parse_result(&serde_json::to_vec(&value).unwrap(), &pinned).unwrap();
}

pub fn listener_routes() {
    let fixture = Fixture::new(
        include_bytes!("execution_api_fixtures/success.wasm"),
        1,
        30000,
    );
    let cancel = json!({"version":"1","type":"cancel","activation_id":"missing","attempt_id":"missing","cancel":{"state":"requested"}});
    assert_eq!(
        request(fixture.server.execute_addr(), "/v1/cancel", &cancel).0,
        404
    );
    assert_eq!(
        request(
            fixture.server.control_addr(),
            "/v1/execute",
            &fixture.request
        )
        .0,
        404
    );
    let (status, reply) = request(fixture.server.control_addr(), "/v1/cancel", &cancel);
    assert_eq!(status, 200);
    assert_eq!(reply["cancel"]["state"], "uncertain");
    let update = json!({"version":"1","type":"update","activation_id":"missing","attempt_id":"missing","sequence":1,"update":{"kind":"heartbeat"}});
    assert_eq!(
        request(fixture.server.control_addr(), "/v1/update", &update).0,
        404
    );
    assert_eq!(fixture.execute().0, 200);
}
