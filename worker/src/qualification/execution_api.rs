//! RUN-01 black-box scenarios. Selected only by the restricted-pod qualification harness.
use falcone_serverless_worker::{api, artifact::cache_key::ENGINE_VERSION, server};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const RESULT: &[u8] = include_bytes!("api_fixtures/result.wasm");
const CONTEXT: &[u8] = include_bytes!("api_fixtures/context.wasm");
const LOGGING: &[u8] = include_bytes!("api_fixtures/logging.wasm");
const LOOP: &[u8] = include_bytes!("api_fixtures/loop.wasm");
const FORBIDDEN: &[u8] = include_bytes!("api_fixtures/forbidden.wasm");
const ENVIRONMENT: &[u8] = include_bytes!("api_fixtures/environment.wasm");
const FILESYSTEM: &[u8] = include_bytes!("api_fixtures/filesystem.wasm");
const CANARY: &str = "secret-canary-credential-130";
const CANCEL_MS: u64 = 1000;

struct Worker {
    child: Child,
    root: PathBuf,
    port: u16,
}
impl Worker {
    fn start(concurrency: u32) -> Self {
        let port = loop {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            if let Ok(control_port) = server::control_port(port)
                && let Ok(_control) = TcpListener::bind(("127.0.0.1", control_port))
            {
                break port;
            }
        };
        let root = std::env::temp_dir().join(format!("run01-api-{}-{port}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let config = root.join("runtime.toml");
        std::fs::write(&config, format!("pool_id = 'qualification'\nfalcone_api_endpoint = 'https://invalid.test'\nfalcone_dispatcher_endpoint = 'https://invalid.test'\nlisten_port = {port}\nconcurrency = {concurrency}\ncache_path = '{}'\ncancel_reserve_ms = {CANCEL_MS}\n", root.display())).unwrap();
        let executable = std::env::current_exe()
            .unwrap()
            .with_file_name("falcone-serverless-worker");
        let child = Command::new(executable)
            .arg("serve")
            .arg(config)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(root.join("diagnostics")).unwrap())
            .spawn()
            .unwrap();
        let mut worker = Self { child, root, port };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok()
                && TcpStream::connect(("127.0.0.1", server::control_port(port).unwrap())).is_ok()
            {
                break;
            }
            assert!(worker.child.try_wait().unwrap().is_none(), "worker_exited");
            assert!(Instant::now() < deadline, "worker_start_deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        worker
    }
    fn request(&self, bytes: &[u8], imports: &[&str], attempt: usize) -> Value {
        let hex = format!("{:x}", Sha256::digest(bytes));
        let directory = self.root.join("components");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("{hex}.wasm")), bytes).unwrap();
        let attempt = format!("attempt-{attempt}");
        json!({
            "version":"1", "type":"execute", "activation_id":"activation", "attempt_id":attempt,
            "attempt_number":1, "deadline":"2099-01-01T00:00:00Z",
            "invocation":{"invocation_key":"invocation"},
            // Synthetic credential claims only; no cluster credentials are read or copied.
            "credential":{"iss":CANARY,"aud":"qualification","tenant":"synthetic","workspace":"synthetic",
                "pool_profile":"qualification","revision":"revision","activation":"activation","attempt":attempt,
                "iat":0,"exp":4070908800_u64,"allowed_capabilities":[]},
            "projection":{"revision":"revision","runtime_mode":"wasm","artifact_digest":format!("sha256:{hex}"),
                "component_digest":format!("sha256:{hex}"), "language_profile":{"name":"qualification","version":"1.0.0"},
                "engine_version":ENGINE_VERSION,"wasi_version":"0.2.6","interface_version":"0.2.0","imports":imports,
                "budgets":{"fuel":10000000,"memory_bytes":67108864,"timeout_ms":30000,"max_host_calls":1000},"grants":[]},
            "params":{"__ow_path":"/"}
        })
    }
    fn post(&self, path: &str, request: &Value) -> (u16, Value) {
        post(self.port, path, request)
    }
    fn assert_redacted(&self) {
        let logs = std::fs::read_to_string(self.root.join("diagnostics")).unwrap();
        assert!(!logs.contains(CANARY), "credential_in_diagnostics");
        assert!(logs.contains("worker_http"), "diagnostic_negative_control");
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn post(port: u16, path: &str, request: &Value) -> (u16, Value) {
    let port = if matches!(path, "/v1/cancel" | "/v1/update") {
        server::control_port(port).unwrap()
    } else {
        port
    };
    let body = serde_json::to_vec(request).unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(310)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(&body).unwrap();
    read_response(stream)
}
fn read_response(stream: TcpStream) -> (u16, Value) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut length = None;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
        assert!(!line.is_empty(), "incomplete_http_headers");
    }
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}
fn cancel(attempt: usize) -> Value {
    json!({"version":"1","type":"cancel","activation_id":"activation","attempt_id":format!("attempt-{attempt}"),"cancel":{"state":"requested"}})
}
fn update(attempt: usize) -> Value {
    json!({"version":"1","type":"update","activation_id":"activation","attempt_id":format!("attempt-{attempt}"),"sequence":1,"update":{"kind":"heartbeat"}})
}
fn identity_imports_and_credential_isolation() {
    let worker = Worker::start(1);
    let mut request = worker.request(RESULT, &[], 0);
    for mode in ["oci", "unknown"] {
        request["projection"]["runtime_mode"] = mode.into();
        let (status, response) = worker.post("/v1/execute", &request);
        assert_eq!(status, 409);
        assert_eq!(response["code"], "runtime_mode_not_supported");
        assert!(!worker.root.join("native").exists(), "non_wasm_compiled");
    }
    for base in [
        None,
        Some(
            json!({"kind":"catalog","id":"base","version":"1.0.0","digest":format!("sha256:{}","a".repeat(64))}),
        ),
    ] {
        let mut request = worker.request(
            LOGGING,
            &[
                "falcone:host/invocation-context@0.2.0",
                "falcone:host/logging@0.2.0",
            ],
            0,
        );
        if let Some(base) = base {
            request["projection"]["base"] = base;
        }
        let parsed = api::parse_request(&serde_json::to_vec(&request).unwrap()).unwrap();
        let (status, response) = worker.post("/v1/execute", &request);
        assert_eq!(status, 200);
        api::parse_result(&serde_json::to_vec(&response).unwrap(), &parsed).unwrap();
        assert_eq!(response["identity"]["wasmtime_version"], ENGINE_VERSION);
        assert_eq!(response["result"]["body"]["credential_in_memory"], false);
        assert!(!response["logs"].as_array().unwrap().is_empty());
        assert!(!response.to_string().contains(CANARY));
        // Independent sensitivity control: the guest's full-memory scanner finds a
        // canary deliberately supplied as a normal action parameter.
        request["params"]["scanner_control"] = CANARY.into();
        let (status, control) = worker.post("/v1/execute", &request);
        assert_eq!(status, 200);
        assert_eq!(control["result"]["body"]["credential_in_memory"], true);
    }
    for (component, imports) in [
        (CONTEXT, vec![]),
        (CONTEXT, vec!["falcone:host/logging@0.2.0"]),
        (FORBIDDEN, vec!["tenant:ambient/escape@0.2.0"]),
        (ENVIRONMENT, vec!["wasi:cli/environment@0.2.6"]),
        (FILESYSTEM, vec!["wasi:filesystem/preopens@0.2.6"]),
    ] {
        let entries = std::fs::read_dir(worker.root.join("native"))
            .unwrap()
            .count();
        let (status, response) =
            worker.post("/v1/execute", &worker.request(component, &imports, 0));
        assert_eq!(status, 422);
        assert_eq!(response["code"], "undeclared_import");
        assert_eq!(
            std::fs::read_dir(worker.root.join("native"))
                .unwrap()
                .count(),
            entries
        );
    }
    worker.assert_redacted();
}
fn saturation(concurrency: u32) {
    let worker = Worker::start(concurrency);
    let request = worker.request(LOOP, &[], 0);
    // Prewarm only the trusted compile cache, then prove cancellation before load.
    std::thread::scope(|scope| {
        let mut executions = Vec::new();
        for id in 0..concurrency as usize {
            let mut request = request.clone();
            request["attempt_id"] = format!("attempt-{id}").into();
            request["credential"]["attempt"] = format!("attempt-{id}").into();
            let port = worker.port;
            executions.push(scope.spawn(move || post(port, "/v1/execute", &request)));
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if worker.post("/v1/update", &update(id)).0 == 200 {
                    break;
                }
                assert!(Instant::now() < deadline, "guest_never_started");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // Header-phase execute connections must not take any of the four control
        // slots, even when no further bytes arrive from these senders.
        let mut incomplete = Vec::new();
        for _ in 0..server::CONTROL_RESERVE {
            let mut stream = TcpStream::connect(("127.0.0.1", worker.port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .write_all(b"POST /v1/execute HTTP/1.1\r\nX-Incomplete: ")
                .unwrap();
            incomplete.push(stream);
        }
        // Use a fresh attempt: duplicate registration must not satisfy this check.
        let extra = worker.request(LOOP, &[], concurrency as usize);
        let start = Instant::now();
        let (status, response) = worker.post("/v1/execute", &extra);
        assert_eq!(status, 503);
        assert_eq!(response["code"], "busy");
        assert!(start.elapsed() < Duration::from_millis(CANCEL_MS));
        let start = Instant::now();
        let (status, response) = worker.post("/v1/cancel", &cancel(0));
        assert_eq!(status, 200);
        assert!(matches!(
            response["cancel"]["state"].as_str(),
            Some("acknowledged" | "uncertain")
        ));
        assert!(
            start.elapsed() < Duration::from_millis(CANCEL_MS),
            "cancel_deadline"
        );
        for stream in incomplete {
            let (status, response) = read_response(stream);
            assert_eq!(status, 503);
            assert_eq!(response["code"], "busy");
        }
        for id in 1..concurrency as usize {
            let port = worker.port;
            let (status, _) = post(port, "/v1/cancel", &cancel(id));
            assert_eq!(status, 200);
        }
        for execution in executions {
            let (status, response) = execution.join().unwrap();
            assert_eq!(status, 422);
            assert_eq!(response["code"], "cancelled");
        }
    });
    worker.assert_redacted();
}

pub fn run() -> ExitCode {
    identity_imports_and_credential_isolation();
    saturation(1);
    saturation(65);
    println!("{}", json!({"check":"execution_api","status":"passed"}));
    ExitCode::SUCCESS
}
