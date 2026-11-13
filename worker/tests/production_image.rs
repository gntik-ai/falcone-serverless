//! The production image can execute a valid cold request (RUN-01).
//!
//! `falcone-serverless-worker serve` resolves the trusted compiler beside its own executable, so
//! an image that ships only the worker refuses every cold request as `artifact_rejected`. This
//! test lays out a directory exactly as the runtime stage of `worker/Containerfile` does (the
//! layout is read from its `COPY --from=build` lines, with the cargo-built binaries standing in
//! for the build stage's outputs), starts the entrypoint from there, and sends a valid request
//! whose component has never been compiled. The qualification image carries more binaries and
//! cannot show this defect, which is why the production layout is tested on its own.

#![cfg(target_os = "linux")]

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const CONTAINERFILE: &str = include_str!("../Containerfile");
const ECHO: &[u8] = include_bytes!("../src/qualification/execute_fixtures/handler.wasm");

/// The runtime stage's copies from the build stage: (source basename, destination path).
fn runtime_copies(containerfile: &str) -> Vec<(String, String)> {
    let runtime = containerfile
        .split("\nFROM ")
        .last()
        .expect("a runtime stage");
    runtime
        .lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            (words.first() == Some(&"COPY") && words.contains(&"--from=build")).then(|| {
                let source = words[words.len() - 2];
                let destination = words[words.len() - 1];
                (
                    Path::new(source)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    destination.to_owned(),
                )
            })
        })
        .collect()
}

fn entrypoint(containerfile: &str) -> String {
    let line = containerfile
        .lines()
        .find(|line| line.starts_with("ENTRYPOINT"))
        .expect("an entrypoint");
    let start = line.find('"').unwrap() + 1;
    let end = line[start..].find('"').unwrap() + start;
    line[start..end].to_owned()
}

/// The cargo-built binary that stands in for a build-stage output.
fn built(name: &str) -> PathBuf {
    match name {
        "falcone-serverless-worker" => env!("CARGO_BIN_EXE_falcone-serverless-worker").into(),
        "artifact-compile-child" => env!("CARGO_BIN_EXE_artifact-compile-child").into(),
        other => panic!("the runtime stage copies {other}, which this test cannot stand in for"),
    }
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Served {
    child: Child,
    execute: SocketAddr,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve_from(layout: &Path, entrypoint: &str, config: &Path) -> Served {
    let mut child = Command::new(layout.join(entrypoint.trim_start_matches('/')))
        .arg("serve")
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the entrypoint starts");
    let stderr = child.stderr.take().unwrap();
    // Block on the worker's own `listening` event; keep draining so it never blocks on stderr.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    let listening = loop {
        let line = receiver
            .recv_timeout(Duration::from_secs(180))
            .expect("the worker reports that it is listening");
        if let Ok(event) = serde_json::from_str::<Value>(&line)
            && event["event"] == "listening"
        {
            break event;
        }
    };
    std::thread::spawn(move || for _ in receiver {});
    let bound: SocketAddr = listening["execute"].as_str().unwrap().parse().unwrap();
    Served {
        child,
        execute: SocketAddr::from(([127, 0, 0, 1], bound.port())),
    }
}

fn request(digest: &str) -> Value {
    let credential_strings = ["issuer", "audience", "tenant", "workspace", "pool"];
    json!({
        "version": "1",
        "type": "execute",
        "activation_id": "act-production-image",
        "attempt_id": "att-production-image",
        "attempt_number": 1,
        "deadline": "2100-01-01T00:00:00.000Z",
        "invocation": {"invocation_key": "inv-production-image"},
        "credential": {
            "iss": credential_strings[0], "aud": credential_strings[1],
            "tenant": credential_strings[2], "workspace": credential_strings[3],
            "pool_profile": credential_strings[4],
            "revision": "rev-production-image",
            "activation": "act-production-image",
            "attempt": "att-production-image",
            "iat": 1_700_000_000, "exp": 4_100_000_000u64,
            "allowed_capabilities": []
        },
        "projection": {
            "revision": "rev-production-image",
            "runtime_mode": "wasm",
            "artifact_digest": digest,
            "component_digest": digest,
            "language_profile": {"name": "profile-qualification", "version": "1"},
            "engine_version": "49.0.5",
            "wasi_version": "0.2.6",
            "interface_version": "0.2.0",
            "imports": [],
            "budgets": {"fuel": 9_007_199_254_740_991u64, "memory_bytes": 67_108_864u64,
                "timeout_ms": 30_000, "max_host_calls": 5_000},
            "grants": []
        },
        "params": {"__ow_path": "/hello", "__ow_method": "post", "name": "world"}
    })
}

#[test]
fn the_production_layout_ships_the_trusted_compiler_beside_the_worker() {
    let copies = runtime_copies(CONTAINERFILE);
    let destination = |name: &str| {
        copies
            .iter()
            .find(|(source, _)| source == name)
            .map(|(_, destination)| destination.clone())
            .unwrap_or_else(|| panic!("the runtime stage does not copy {name}"))
    };
    let worker = destination("falcone-serverless-worker");
    let compiler = destination("artifact-compile-child");
    assert_eq!(entrypoint(CONTAINERFILE), worker);
    assert_eq!(
        Path::new(&worker).parent(),
        Path::new(&compiler).parent(),
        "the worker looks for the compiler beside itself"
    );
    assert_eq!(
        Path::new(&compiler).file_name().unwrap(),
        "artifact-compile-child"
    );
}

#[test]
fn a_valid_cold_request_executes_in_the_production_layout() {
    let id = std::process::id();
    let scratch = Scratch(std::env::temp_dir().join(format!("production-image-{id}")));
    let layout = scratch.0.join("image");
    for (name, destination) in runtime_copies(CONTAINERFILE) {
        let target = layout.join(destination.trim_start_matches('/'));
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(built(&name), &target).unwrap();
    }
    let cache = scratch.0.join("cache");
    std::fs::create_dir_all(cache.join("artifacts")).unwrap();
    let digest = format!("sha256:{:x}", Sha256::digest(ECHO));
    std::fs::write(
        cache
            .join("artifacts")
            .join(format!("{}.wasm", &digest["sha256:".len()..])),
        ECHO,
    )
    .unwrap();
    let config = scratch.0.join("worker.toml");
    std::fs::write(
        &config,
        format!(
            "pool_id = \"qualification\"\n\
             falcone_api_endpoint = \"https://falcone.invalid/api\"\n\
             falcone_dispatcher_endpoint = \"https://dispatcher.invalid\"\n\
             listen_port = 0\ncontrol_port = 0\nconcurrency = 1\n\
             default_wall_clock_ms = 30000\nmax_wall_clock_ms = 300000\n\
             default_memory_bytes = 134217728\nmax_memory_bytes = 536870912\n\
             cache_path = \"{}\"\n",
            cache.display()
        ),
    )
    .unwrap();

    let served = serve_from(&layout, &entrypoint(CONTAINERFILE), &config);
    let body = request(&digest).to_string();
    let mut stream = TcpStream::connect(served.execute).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(180)))
        .unwrap();
    write!(
        stream,
        "POST /v1/execute HTTP/1.1\r\nHost: worker\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, answer) = raw.split_once("\r\n\r\n").expect("a response");
    let answer: Value = serde_json::from_str(answer).expect("a JSON response");
    assert!(head.starts_with("HTTP/1.1 200"), "{head}\n{answer}");
    assert_eq!(answer["status"], "success", "{answer}");
    assert_eq!(answer["identity"]["wasmtime_version"], "49.0.5");
}
