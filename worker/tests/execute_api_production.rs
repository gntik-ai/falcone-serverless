//! Black-box tests of the worker's execution API against the production worker, over real
//! sockets, with no test hook, no instrumented platform and no knowledge of the server's
//! internals.
//!
//! Two targets, one suite. Without `EXECUTE_API_TARGET` the suite starts the built
//! `falcone-serverless-worker serve` binary (or `EXECUTE_API_WORKER`) on loopback ports, one per
//! test. With `EXECUTE_API_TARGET=<execute port>,<control port>` and `EXECUTE_API_ARTIFACTS=<dir>`
//! it drives a worker that is already running next to it: in the restricted qualification pod
//! that is the digest-pinned production image, sharing the pod's network and the artifact
//! directory (`worker/src/qualification/execute_api_runner.rs`). The suite builds nothing from
//! the worker's sources in that mode and never needs a path from the build.
//!
//! A guest proves that it ran by what it logged: the result envelope of a timeout carries the
//! guest's own first log line.
#![cfg(target_os = "linux")]

#[path = "support/execute_api_fixtures.rs"]
mod support;

use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use support::*;

/// The worker configuration of the target. A worker the suite starts itself uses these values;
/// in the qualification pod the runner states the values it wrote to the worker's
/// configuration in `EXECUTE_API_LIMITS=<concurrency>,<max wall clock ms>,<max memory bytes>`.
const CONCURRENCY: usize = 2;
const MAX_WALL_CLOCK_MS: u64 = 10_000;
const MAX_MEMORY_BYTES: u64 = 64 * 1024 * 1024;

fn limits() -> (usize, u64, u64) {
    let Ok(stated) = std::env::var("EXECUTE_API_LIMITS") else {
        return (CONCURRENCY, MAX_WALL_CLOCK_MS, MAX_MEMORY_BYTES);
    };
    let parts: Vec<&str> = stated.split(',').collect();
    let [concurrency, wall_clock, memory] = parts[..] else {
        panic!("EXECUTE_API_LIMITS is <concurrency>,<wall clock ms>,<memory bytes>");
    };
    (
        concurrency.parse().unwrap(),
        wall_clock.parse().unwrap(),
        memory.parse().unwrap(),
    )
}

struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Target {
    execute: SocketAddr,
    control: SocketAddr,
    artifacts: PathBuf,
    scratch: Option<PathBuf>,
    _worker: Option<Killed>,
}

impl Drop for Target {
    fn drop(&mut self) {
        if let Some(scratch) = &self.scratch {
            let _ = std::fs::remove_dir_all(scratch);
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn worker_binary() -> PathBuf {
    std::env::var_os("EXECUTE_API_WORKER")
        .map(Into::into)
        .or_else(|| option_env!("CARGO_BIN_EXE_falcone-serverless-worker").map(Into::into))
        .expect("EXECUTE_API_WORKER names the worker binary")
}

impl Target {
    fn start() -> Self {
        let mut target = match std::env::var("EXECUTE_API_TARGET") {
            Ok(ports) => {
                let (execute, control) = ports.split_once(',').expect("<execute>,<control>");
                Self {
                    execute: format!("127.0.0.1:{execute}").parse().unwrap(),
                    control: format!("127.0.0.1:{control}").parse().unwrap(),
                    artifacts: std::env::var_os("EXECUTE_API_ARTIFACTS")
                        .expect("EXECUTE_API_ARTIFACTS")
                        .into(),
                    scratch: None,
                    _worker: None,
                }
            }
            Err(_) => Self::spawn(),
        };
        // Readiness: poll until both listeners accept.
        let limit = Instant::now() + LONG;
        while TcpStream::connect(target.execute).is_err()
            || TcpStream::connect(target.control).is_err()
        {
            assert!(Instant::now() < limit, "the worker never listened");
            if let Some(Killed(worker)) = &mut target._worker {
                assert!(
                    worker.try_wait().unwrap().is_none(),
                    "the worker exited before it listened"
                );
            }
            std::thread::yield_now();
        }
        target
    }

    fn spawn() -> Self {
        let id = NEXT.fetch_add(1, Ordering::AcqRel);
        let scratch = std::env::temp_dir().join(format!(
            "execute-api-production-{}-{id}",
            std::process::id()
        ));
        let artifacts = scratch.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let (execute, control) = (free_port(), free_port());
        let config = scratch.join("worker.toml");
        std::fs::write(
            &config,
            format!(
                "version = 2\npool_id = \"pool-local\"\nfalcone_api_endpoint = \"https://api.invalid\"\n\
                 falcone_dispatcher_endpoint = \"https://dispatcher.invalid\"\n\
                 listen_port = {execute}\ncontrol_port = {control}\n\
                 concurrency = {CONCURRENCY}\nmax_wall_clock_ms = {MAX_WALL_CLOCK_MS}\n\
                 default_wall_clock_ms = {MAX_WALL_CLOCK_MS}\n\
                 max_memory_bytes = {MAX_MEMORY_BYTES}\ndefault_memory_bytes = {MAX_MEMORY_BYTES}\n\
                 cache_path = \"{}\"\nartifact_path = \"{}\"\n",
                scratch.join("cache").display(),
                artifacts.display()
            ),
        )
        .unwrap();
        let worker = std::process::Command::new(worker_binary())
            .arg("serve")
            .arg(&config)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        Self {
            execute: format!("127.0.0.1:{execute}").parse().unwrap(),
            control: format!("127.0.0.1:{control}").parse().unwrap(),
            artifacts,
            scratch: Some(scratch),
            _worker: Some(Killed(worker)),
        }
    }

    /// Publishes an artifact to the worker's platform and returns its digest.
    fn add(&self, bytes: &[u8]) -> String {
        let hex = format!("{:x}", Sha256::digest(bytes));
        std::fs::write(self.artifacts.join(format!("{hex}.wasm")), bytes).unwrap();
        format!("sha256:{hex}")
    }

    fn run(&self, spec: &Spec) -> Reply2 {
        post(
            self.execute,
            "/v1/execute",
            spec.request().to_string().as_bytes(),
        )
    }

    /// Starts an execution that is meant to hold a slot. Its request is retried while the
    /// worker is busy (a probe may hold a slot for a moment), so it ends only when it was
    /// admitted and then ran to its outcome.
    fn spawn_held(&self, spec: &Spec) -> std::thread::JoinHandle<Reply2> {
        let (addr, body) = (self.execute, spec.request().to_string());
        std::thread::spawn(move || {
            loop {
                let reply = post(addr, "/v1/execute", body.as_bytes());
                if reply.status != 503 {
                    return reply;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    }

    fn cancel(&self, activation: &str) -> Reply2 {
        let body = json!({
            "version": "1", "type": "cancel", "activation_id": activation,
            "attempt_id": "att-local-0001", "cancel": {"state": "requested"}
        });
        post(self.control, "/v1/cancel", body.to_string().as_bytes())
    }

    /// Runs the spin guest to a short timeout so that its compile is cached, and returns the
    /// envelope: the guest's own first log line proves that it ran.
    fn warm(&self, spin: &Spec) {
        let reply = self.run(&spin.clone().with(|s| s.timeout_ms = 5000));
        assert_eq!(code(&reply).1, "timeout", "{}", text(&reply));
        assert!(ran(&reply), "the guest never ran: {}", text(&reply));
    }

    /// Admits probes until one is refused as busy: every slot is then held.
    fn wait_until_busy(&self, probe: &Spec) {
        let limit = Instant::now() + LONG;
        loop {
            let reply = self.run(probe);
            match reply.status {
                503 => return,
                200 => assert!(Instant::now() < limit, "the slots were never all taken"),
                _ => panic!("probe: {}", text(&reply)),
            }
        }
    }
}

fn text(reply: &Reply2) -> String {
    String::from_utf8_lossy(&reply.body).into_owned()
}

/// Whether the result envelope carries the guest's first log line.
fn ran(reply: &Reply2) -> bool {
    reply.json()["logs"].as_array().is_some_and(|lines| {
        lines
            .iter()
            .any(|line| line.as_str().is_some_and(|l| l.contains(STARTED)))
    })
}

fn headers_only(addr: SocketAddr, length: usize) -> Reply2 {
    let mut stream = connect(addr);
    let head =
        format!("POST /v1/execute HTTP/1.1\r\nHost: worker\r\nContent-Length: {length}\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    read_reply(stream).expect("a response")
}

#[test]
fn a_wasm_execute_returns_the_legacy_envelope_with_the_runtime_identity() {
    let target = Target::start();
    let digest = target.add(&success());
    let spec = Spec::new(&digest);
    let reply = target.run(&spec);
    assert_eq!(reply.status, 200, "{}", text(&reply));
    let result = reply.json();
    assert_eq!(result["type"], "result");
    assert_eq!(result["status"], "success");
    assert_eq!(result["activation_id"], spec.activation);
    let identity = &result["identity"];
    assert_eq!(identity["runtime_mode"], "wasm");
    assert_eq!(identity["function_revision"], "rev-local-0001");
    assert_eq!(identity["artifact_digest"], digest);
    assert_eq!(identity["interface_version"], "0.2.0");
    assert_eq!(identity["wasmtime_version"], WORKER_ENGINE);
}

#[test]
fn a_non_wasm_or_image_request_is_refused_409_even_with_every_slot_busy_and_over_capacity_is_503() {
    let target = Target::start();
    let spin = Spec::blocking(&target.add(&spin()));
    let probe = Spec::new(&target.add(&success()));
    target.warm(&spin);
    assert_eq!(target.run(&probe).status, 200);
    let refused = (
        409,
        "runtime_mode_not_supported".to_owned(),
        "runtime_mode_not_wasm".to_owned(),
    );
    let oci = spin.clone().with(|s| s.runtime_mode = "oci");
    let image = oci.clone().with(|s| s.base = true);
    assert_eq!(code(&target.run(&oci)), refused);
    assert_eq!(code(&target.run(&image)), refused);
    // Take every slot.
    let running: Vec<_> = (0..limits().0)
        .map(|_| {
            let held = spin.clone().with(|s| {
                s.activation = Spec::new(&spin.digest).activation;
                s.timeout_ms = 20_000;
            });
            (held.clone(), target.spawn_held(&held))
        })
        .collect();
    target.wait_until_busy(&probe);
    // The refusal still comes first, with the complete body already buffered.
    assert_eq!(code(&target.run(&oci)), refused);
    assert_eq!(code(&target.run(&image)), refused);
    // A wasm request is busy, and so is one whose body never arrives: its body is not read.
    let busy = (503, "busy".to_owned(), "at_capacity".to_owned());
    assert_eq!(code(&target.run(&probe)), busy);
    let started = Instant::now();
    assert_eq!(code(&headers_only(target.execute, 4096)), busy);
    assert!(started.elapsed() < Duration::from_secs(5));
    for (held, handle) in running {
        assert_eq!(target.cancel(&held.activation).status, 200);
        assert_eq!(code(&handle.join().unwrap()).1, "cancelled");
    }
}

#[test]
fn an_import_that_is_not_declared_is_422_before_any_guest_runs() {
    let target = Target::start();
    let both = target.add(&guest(
        &["logging", "progress"],
        64,
        &[(1024, OK_JSON)],
        &ret(OK_JSON.len()),
    ));
    let undeclared = (
        422,
        "undeclared_import".to_owned(),
        "import_not_declared".to_owned(),
    );
    assert_eq!(code(&target.run(&Spec::new(&both))), undeclared);
    let partial = Spec::new(&both).with(|s| s.imports = vec!["logging"]);
    assert_eq!(code(&target.run(&partial)), undeclared);
    // Declared, but not an interface the worker allows.
    let other = target.add(&guest(
        &["other"],
        64,
        &[(1024, OK_JSON)],
        &ret(OK_JSON.len()),
    ));
    let declared_other = Spec::new(&other).with(|s| s.imports = vec!["other"]);
    assert_eq!(code(&target.run(&declared_other)), undeclared);
    // Every import declared.
    let all = Spec::new(&both).with(|s| s.imports = vec!["logging", "progress"]);
    assert_eq!(target.run(&all).status, 200);
}

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
fn the_credential_never_reaches_logs_including_across_the_capture_cut() {
    let target = Target::start();
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
        format!("start {long} end"),
        format!("{}{crossing} after", "a".repeat(cut - 7)),
        format!("{}{at_cut}", "b".repeat(cut - at_cut.len())),
        format!("{}{short} {at_cut} {crossing} {long}", "c".repeat(cut - 20)),
        format!("{} {} {}", canaries[0], canaries[2], short),
    ];
    for (index, message) in messages.iter().enumerate() {
        let digest = target.add(&logging(message, 1));
        let spec = Spec::new(&digest).with(|s| {
            s.imports = vec!["logging"];
            s.secrets = secrets.iter().map(|secret| (*secret).to_owned()).collect();
        });
        let reply = target.run(&spec);
        assert_eq!(reply.status, 200, "{index}: {}", text(&reply));
        let logs: Vec<String> = serde_json::from_value(reply.json()["logs"].clone()).unwrap();
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
        let body = text(&reply);
        for secret in canaries.iter().chain([crossing, at_cut].iter()) {
            assert!(!body.contains(secret), "{index}");
        }
    }
}

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
    let target = Target::start();
    let mut secrets = CLAIM_CANARIES.to_vec();
    secrets.push(SCOPE_SECRET);
    // The positive controls: what the host writes into a guest's memory (its params and the
    // invocation key), so that a scan which cannot see it proves nothing about absence.
    let digest = target.add(&memory_probe(&secrets, &["/v1/local", "inv-local-0001"]));
    let spec = Spec::new(&digest).with(|s| {
        s.imports = vec!["context"];
        s.secrets = vec![SCOPE_SECRET.to_owned()];
    });
    let reply = target.run(&spec);
    assert_eq!(reply.status, 200, "{}", text(&reply));
    assert_eq!(probe_report(&reply.json()), (0, 2));
    // The same probe finds material that is in its memory.
    let detecting = target.add(&memory_probe(&["inv-local-0001"], &["/v1/local"]));
    let spec = Spec::new(&detecting).with(|s| s.imports = vec!["context"]);
    assert_eq!(probe_report(&target.run(&spec).json()), (1, 1));
}

#[test]
fn guest_access_to_the_environment_or_a_file_is_refused_by_name_before_any_guest_runs() {
    let target = Target::start();
    for prohibited in ["wasi-environment", "wasi-preopens", "wasi-filesystem"] {
        let digest = target.add(&guest(
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
            code(&target.run(&Spec::new(&digest))),
            refused,
            "{prohibited}"
        );
        let declared = Spec::new(&digest).with(|s| s.imports = vec![prohibited]);
        assert_eq!(code(&target.run(&declared)), refused, "{prohibited}");
    }
    // A component that exports anything beside the handler is refused as well.
    let extra = target.add(&guest_with(
        &[],
        64,
        &[(1024, OK_JSON)],
        "",
        r#"(export "wasi:cli/run@0.2.6" (instance $handler))"#,
        &ret(OK_JSON.len()),
    ));
    let (status, failure, reason) = code(&target.run(&Spec::new(&extra)));
    assert_eq!(
        (status, failure.as_str(), reason.as_str()),
        (500, "execution_failed", "artifact_rejected")
    );
    // Nothing was left running: a normal execute is admitted.
    let ok = target.add(&success());
    assert_eq!(target.run(&Spec::new(&ok)).status, 200);
}

#[test]
fn a_cancel_on_the_control_listener_takes_effect_within_the_budget_with_the_execute_listener_full()
{
    let target = Target::start();
    let spin = Spec::blocking(&target.add(&spin()));
    let probe = Spec::new(&target.add(&success()));
    target.warm(&spin);
    assert_eq!(target.run(&probe).status, 200);
    let running: Vec<_> = (0..limits().0)
        .map(|_| {
            let held = spin.clone().with(|s| {
                s.activation = Spec::new(&spin.digest).activation;
                s.timeout_ms = 20_000;
            });
            (held.clone(), target.spawn_held(&held))
        })
        .collect();
    target.wait_until_busy(&probe);
    // Fill the rest of the execute listener with stalled connections: mid-header, and with
    // headers that promise a body.
    let mut held = Vec::new();
    for index in 0..4 {
        let mut stream = connect(target.execute);
        stream
            .write_all(if index % 2 == 0 {
                &b"POST /v1/exec"[..]
            } else {
                &b"POST /v1/execute HTTP/1.1\r\nContent-Length: 4096\r\n"[..]
            })
            .unwrap();
        held.push(stream);
    }
    // A further execute is busy; a cancel on the control listener is accepted and takes effect:
    // the slot is free within the budget, counted from before the cancel was sent.
    assert_eq!(code(&target.run(&probe)).1, "busy");
    let mut running = running.into_iter();
    let (first, first_reply) = running.next().unwrap();
    let sent = Instant::now();
    let answer = target.cancel(&first.activation);
    assert_eq!(answer.status, 200, "{}", text(&answer));
    assert_eq!(answer.json()["cancel"]["state"], "acknowledged");
    assert_eq!(code(&first_reply.join().unwrap()).1, "cancelled");
    // The stalled connections hold execute connection capacity; a slot is free when an execute
    // that is admitted ends with 200.
    drop(held);
    loop {
        let reply = target.run(&probe);
        if reply.status == 200 {
            break;
        }
        assert_eq!(reply.status, 503, "{}", text(&reply));
        assert!(sent.elapsed() < BUDGET, "the slot was not free in time");
    }
    assert!(sent.elapsed() < BUDGET, "{:?}", sent.elapsed());
    for (held, handle) in running {
        assert_eq!(target.cancel(&held.activation).status, 200);
        assert_eq!(code(&handle.join().unwrap()).1, "cancelled");
    }
}

#[test]
fn each_listener_refuses_the_others_operations_and_unknown_paths() {
    let target = Target::start();
    let digest = target.add(&success());
    let execute = Spec::new(&digest).request().to_string();
    let cancel = json!({
        "version": "1", "type": "cancel", "activation_id": "act-x-0001",
        "attempt_id": "att-x-0001", "cancel": {"state": "requested"}
    })
    .to_string();
    let not_found = |reply: Reply2| {
        (
            reply.status,
            reply.json()["code"].as_str().unwrap().to_owned(),
        )
    };
    let refused = (404, "not_found".to_owned());
    assert_eq!(
        not_found(post(target.control, "/v1/execute", execute.as_bytes())),
        refused
    );
    assert_eq!(
        not_found(post(target.execute, "/v1/cancel", cancel.as_bytes())),
        refused
    );
    assert_eq!(
        not_found(post(target.execute, "/v1/update", cancel.as_bytes())),
        refused
    );
    assert_eq!(not_found(post(target.execute, "/nothing", b"{}")), refused);
    assert_eq!(not_found(post(target.control, "/nothing", b"{}")), refused);
    let unknown = post(target.control, "/v1/cancel", cancel.as_bytes());
    assert_eq!(
        code(&unknown),
        (404, "not_found".into(), "unknown_execution".into())
    );
    assert_eq!(
        post(target.execute, "/v1/execute", execute.as_bytes()).status,
        200
    );
}

#[test]
fn the_shortest_of_the_request_the_projection_and_the_worker_limit_wins() {
    let target = Target::start();
    let spin = Spec::blocking(&target.add(&spin()));
    target.warm(&spin);
    let timed = |spec: Spec, shortest: Duration| {
        let started = Instant::now();
        let reply = target.run(&spec);
        let elapsed = started.elapsed();
        assert_eq!(
            code(&reply),
            (504, "timeout".into(), "deadline_exceeded".into()),
            "{}",
            text(&reply)
        );
        let result = reply.json();
        assert_eq!(result["type"], "result");
        assert_eq!(result["activation_id"], spec.activation);
        assert_eq!(result["identity"]["wasmtime_version"], WORKER_ENGINE);
        assert_eq!(result["class"], "uncertain");
        // The guest ran (its own first log line), and the execution ended at the shortest limit.
        assert!(ran(&reply), "the guest never ran: {}", text(&reply));
        assert!(
            elapsed >= shortest.saturating_sub(Duration::from_millis(50)),
            "{elapsed:?} < {shortest:?}"
        );
        assert!(
            elapsed < shortest + BUDGET,
            "{elapsed:?} >= {shortest:?} + budget"
        );
    };
    // The request's own absolute deadline is the shortest (whole seconds: 1 to 2 s away).
    let started = Instant::now();
    let reply = target.run(&spin.clone().with(|s| s.deadline_secs = 2));
    assert_eq!(code(&reply).1, "timeout", "{}", text(&reply));
    assert!(ran(&reply));
    let elapsed = started.elapsed();
    assert!(
        elapsed > Duration::from_millis(900) && elapsed < Duration::from_secs(2) + BUDGET,
        "{elapsed:?}"
    );
    // The projection's limit is the shortest.
    timed(
        spin.clone().with(|s| s.timeout_ms = 700),
        Duration::from_millis(700),
    );
    // The worker's wall-clock limit is the shortest of all.
    timed(
        spin.clone().with(|s| s.timeout_ms = 20_000),
        Duration::from_millis(limits().1),
    );
    // Capacity is recovered.
    let ok = target.add(&success());
    assert_eq!(target.run(&Spec::new(&ok)).status, 200);
}

#[test]
fn memory_fuel_and_host_call_budgets_end_the_execution_with_their_own_reason() {
    let target = Target::start();
    let failed = |reason: &str| (500, "execution_failed".to_owned(), reason.to_owned());
    let ok = target.add(&success());
    // Memory: the 4 MiB fixture fails at 1 MiB and passes with enough.
    let reply = target.run(&Spec::new(&ok).with(|s| s.memory_bytes = 1024 * 1024));
    assert_eq!(code(&reply), failed("memory_budget_exceeded"));
    assert_eq!(reply.json()["result"]["retryable"], false);
    assert_eq!(
        target
            .run(&Spec::new(&ok).with(|s| s.memory_bytes = 8 * 1024 * 1024))
            .status,
        200
    );
    // Fuel: a small budget stops a looping guest well before `timeout_ms`.
    let looping = Spec::blocking(&target.add(&spin()));
    let started = Instant::now();
    let reply = target.run(&looping.clone().with(|s| s.fuel = 100_000));
    assert_eq!(code(&reply), failed("fuel_exhausted"));
    assert!(started.elapsed() < Duration::from_secs(20));
    // Host calls: N+1 calls are over the budget, N are not.
    let reports = target.add(&reporting(4));
    let over = Spec::new(&reports).with(|s| {
        s.imports = vec!["progress"];
        s.max_host_calls = 3;
    });
    assert_eq!(
        code(&target.run(&over)),
        failed("host_call_budget_exceeded")
    );
    let enough = over.with(|s| s.max_host_calls = 4);
    assert_eq!(target.run(&enough).status, 200);
    // A budget above the worker's ceiling is refused before anything is compiled.
    let fresh = target.add(&guest(&[], 1, &[(1024, OK_JSON)], &ret(OK_JSON.len())));
    let above = Spec::new(&fresh).with(|s| s.memory_bytes = limits().2 * 2);
    assert_eq!(
        code(&target.run(&above)),
        (
            400,
            "invalid_request".into(),
            "budget_above_worker_ceiling".into()
        )
    );
}

#[test]
fn http_ceilings_hold_over_a_real_socket_and_the_worker_recovers() {
    let target = Target::start();
    let ok = Spec::new(&target.add(&success()));
    let mut head = b"POST /v1/execute HTTP/1.1\r\nX-Padding: ".to_vec();
    head.extend(std::iter::repeat_n(b'a', 20 * 1024));
    head.extend_from_slice(b"\r\nContent-Length: 0\r\n\r\n");
    let mut stream = connect(target.execute);
    stream.write_all(&head).unwrap();
    let reply = read_reply(stream).expect("a response");
    assert_eq!(
        code(&reply),
        (413, "request_too_large".into(), "headers_too_large".into())
    );
    assert_eq!(target.run(&ok).status, 200);
    let reply = headers_only(target.execute, 8 * 1024 * 1024 + 1);
    assert_eq!(
        code(&reply),
        (413, "request_too_large".into(), "body_too_large".into())
    );
    assert_eq!(target.run(&ok).status, 200);
    let mut stream = connect(target.control);
    stream
        .write_all(b"POST /v1/cancel HTTP/1.1\r\nContent-Length: 1048576\r\n\r\n")
        .unwrap();
    let reply = read_reply(stream).expect("a response");
    assert_eq!(
        code(&reply),
        (413, "request_too_large".into(), "body_too_large".into())
    );
    // A cancel and an execute both succeed afterwards: no slot leaked.
    assert_eq!(code(&target.cancel("act-none-0001")).1, "not_found");
    assert_eq!(target.run(&ok).status, 200);
}
