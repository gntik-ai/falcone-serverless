//! The execution API cases that run against the production worker image: the runner container
//! of the pod talks over the pod's loopback to the unmodified `falcone-serverless-worker serve`
//! container, whose artifacts and configuration an init container wrote. Nothing here can see
//! inside the worker, so every case asserts on what a dispatcher sees; the cases that need
//! server-side hooks (blocked setup, validation counters, host-call counts) run in the
//! qualification image instead (`execute_api_cases.rs`).
use crate::execute_api_support::*;
use serde_json::json;
use std::io::Write;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const EXECUTE: ([u8; 4], u16) = ([127, 0, 0, 1], 8080);
const CONTROL: ([u8; 4], u16) = ([127, 0, 0, 1], 8082);
const LOGGING: &str = "falcone:host/logging@0.2.0";
const CONTEXT: &str = "falcone:host/invocation-context@0.2.0";

/// Concurrency of the production worker under qualification.
const CONCURRENCY: usize = 2;

/// The artifacts the pod's worker may run: the embedded fixtures, by digest.
const FIXTURES: [&str; 4] = ["api-ok", "api-log-ok", "api-log-spin", "api-context"];

fn execute() -> SocketAddr {
    SocketAddr::from(EXECUTE)
}

fn control() -> SocketAddr {
    SocketAddr::from(CONTROL)
}

fn digest_of(name: &str) -> [u8; 32] {
    digest(&fixture(name))
}

/// Init-container mode: writes the worker's configuration and artifacts under `directory`.
pub fn prepare(directory: &std::path::Path) -> Result<(), String> {
    let artifacts = directory.join("artifacts");
    std::fs::create_dir_all(&artifacts).map_err(|error| error.to_string())?;
    for name in FIXTURES {
        let hex: String = digest_of(name)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        std::fs::write(artifacts.join(format!("{hex}.wasm")), fixture(name))
            .map_err(|error| error.to_string())?;
    }
    let config = format!(
        "pool_id = \"pool-qualification\"\nconcurrency = {CONCURRENCY}\n\
         listen_address = \"127.0.0.1\"\nlisten_port = {}\ncontrol_port = {}\n\
         cache_path = \"/tmp/cache\"\nartifact_path = \"{}\"\n\
         falcone_api_endpoint = \"https://api.invalid\"\n\
         falcone_dispatcher_endpoint = \"https://dispatcher.invalid\"\n",
        EXECUTE.1,
        CONTROL.1,
        artifacts.display()
    );
    std::fs::write(directory.join("worker.toml"), config).map_err(|error| error.to_string())
}

/// Waits (until the safety timeout) for the worker container to listen.
fn wait_listening() {
    let begun = Instant::now();
    while std::net::TcpStream::connect_timeout(&execute(), Duration::from_millis(200)).is_err()
        || std::net::TcpStream::connect_timeout(&control(), Duration::from_millis(200)).is_err()
    {
        assert!(begun.elapsed() < SAFETY, "the worker never listened");
        std::thread::yield_now();
    }
}

fn run(spec: &Spec) -> Answer {
    post(execute(), "/v1/execute", &spec.bytes())
}

fn recovered() {
    assert_eq!(run(&Spec::new(digest_of("api-ok"))).status, 200);
}

fn cancel_of(spec: &Spec) -> String {
    let answer = post(control(), "/v1/cancel", &spec.cancel());
    answer.json()["cancel"]["state"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Whether the control listener knows the execution as running: `204` to a heartbeat.
fn running(spec: &Spec) -> bool {
    let update = serde_json::to_vec(&json!({
        "version": "1", "type": "update",
        "activation_id": spec.activation, "attempt_id": spec.attempt(),
        "sequence": 1, "update": {"kind": "heartbeat"}
    }))
    .unwrap();
    post(control(), "/v1/update", &update).status == 204
}

fn spinner() -> Spec {
    Spec::new(digest_of("api-log-spin")).imports(&[LOGGING])
}

fn envelope_and_identity() {
    let spec = Spec::new(digest_of("api-ok"));
    let answer = run(&spec);
    assert_eq!(answer.status, 200);
    let body = answer.json();
    assert_eq!(body["result"], json!({"statusCode": 200, "body": "ok"}));
    assert_eq!(body["identity"]["wasmtime_version"], "49.0.5");
    assert_eq!(body["identity"]["function_revision"], "rev-local-1");
    assert_eq!(body["identity"]["runtime_mode"], "wasm");
    assert_eq!(
        body["identity"]["artifact_digest"],
        digest_text(&spec.digest)
    );
}

fn other_runtime_modes_are_refused() {
    for mode in ["oci", "image"] {
        let mut spec = Spec::new(digest_of("api-ok"));
        spec.runtime_mode = mode.into();
        let answer = run(&spec);
        assert_eq!(answer.status, 409, "{mode}");
        assert_eq!(answer.failure().0, "runtime_mode_not_supported");
    }
}

fn imports_must_be_declared_and_allowed() {
    let log = digest_of("api-log-ok");
    for imports in [vec![], vec![CONTEXT]] {
        let answer = run(&Spec::new(log).imports(&imports));
        assert_eq!(
            (answer.status, answer.failure().0.as_str()),
            (422, "undeclared_import")
        );
    }
    let context = digest_of("api-context");
    assert_eq!(run(&Spec::new(context)).status, 422);
    assert_eq!(run(&Spec::new(context).imports(&[CONTEXT])).status, 200);
    assert_eq!(run(&Spec::new(log).imports(&[LOGGING])).status, 200);
}

fn memory_and_fuel_budgets_are_enforced() {
    let mut small = Spec::new(digest_of("api-ok"));
    small.memory_bytes = 1024 * 1024;
    let answer = run(&small);
    assert_eq!(
        answer.failure(),
        ("execution_failed".into(), "memory_budget_exceeded".into())
    );
    let mut enough = Spec::new(digest_of("api-ok"));
    enough.memory_bytes = 8 * 1024 * 1024;
    assert_eq!(run(&enough).status, 200);
    let mut fuel = spinner();
    fuel.fuel = 5_000_000;
    let begun = Instant::now();
    assert_eq!(
        run(&fuel).failure(),
        ("execution_failed".into(), "fuel_exhausted".into())
    );
    assert!(begun.elapsed() < Duration::from_secs(10));
    let mut above = Spec::new(digest_of("api-ok"));
    above.memory_bytes = 1 << 40;
    assert_eq!(run(&above).failure().1, "budget_above_worker_ceiling");
    recovered();
}

fn the_shortest_timeout_wins() {
    let short = Duration::from_millis(600);
    for projection_limited in [false, true] {
        let mut spec = spinner();
        if projection_limited {
            spec.timeout_ms = 600;
        } else {
            spec.deadline_in = short;
        }
        let begun = Instant::now();
        let answer = run(&spec);
        let elapsed = begun.elapsed();
        assert_eq!(answer.status, 504);
        assert_eq!(answer.json()["identity"]["wasmtime_version"], "49.0.5");
        assert!(elapsed >= short - Duration::from_millis(100), "{elapsed:?}");
        assert!(elapsed < short + Duration::from_millis(2500), "{elapsed:?}");
    }
    recovered();
}

fn listeners_refuse_each_others_operations() {
    let spec = Spec::new(digest_of("api-ok"));
    assert_eq!(post(control(), "/v1/execute", &spec.bytes()).status, 404);
    assert_eq!(post(execute(), "/v1/cancel", &spec.cancel()).status, 404);
    assert_eq!(post(execute(), "/v1/update", b"{}").status, 404);
}

/// With every slot running a guest and the execute listener full of stalled connections, a
/// cancel on the control listener takes effect and one more execute is busy.
fn cancel_succeeds_under_full_load() {
    let busy: Vec<_> = (0..CONCURRENCY)
        .map(|_| {
            let spec = spinner();
            let body = spec.bytes();
            let handle = std::thread::spawn(move || post(execute(), "/v1/execute", &body));
            (spec, handle)
        })
        .collect();
    let begun = Instant::now();
    for (spec, _) in &busy {
        while !running(spec) {
            assert!(begun.elapsed() < SAFETY, "the guest never started");
            std::thread::yield_now();
        }
    }
    let stalled: Vec<_> = (0..8)
        .map(|index| {
            let mut stream = connect(execute());
            if index % 2 == 0 {
                stream.write_all(b"POST /v1/exec").unwrap();
            } else {
                stream.write_all(&head("/v1/execute", 1000)).unwrap();
                stream.write_all(b"{").unwrap();
            }
            stream
        })
        .collect();
    let more = run(&Spec::new(digest_of("api-ok")));
    assert_eq!((more.status, more.failure().0.as_str()), (503, "busy"));
    for (spec, handle) in busy {
        assert_eq!(cancel_of(&spec), "acknowledged");
        let answer = handle.join().unwrap();
        assert_eq!(
            (answer.status, answer.failure().0.as_str()),
            (499, "cancelled")
        );
    }
    drop(stalled);
    recovered();
}

fn http_ceilings_refuse_without_running_a_guest() {
    let mut headers = connect(execute());
    let mut request = b"POST /v1/execute HTTP/1.1\r\n".to_vec();
    while request.len() < 20 * 1024 {
        request.extend_from_slice(b"X-Padding: 0123456789012345678901234567890123456789\r\n");
    }
    request.extend_from_slice(b"Content-Length: 0\r\n\r\n");
    let _ = headers.write_all(&request);
    if let Some(answer) = read_answer(&mut headers) {
        assert_eq!(answer.status, 431);
    }
    let mut large = connect(execute());
    large
        .write_all(&head("/v1/execute", 8 * 1024 * 1024 + 1))
        .unwrap();
    assert_eq!(read_answer(&mut large).expect("refused").status, 413);
    let mut body = Spec::new(digest_of("api-ok"));
    body.body_chars = 1_048_577;
    assert_eq!(run(&body).status, 413);
    let mut control_large = connect(control());
    control_large
        .write_all(&head("/v1/cancel", 64 * 1024 + 1))
        .unwrap();
    assert_eq!(
        read_answer(&mut control_large).expect("refused").status,
        413
    );
    recovered();
    assert_eq!(cancel_of(&Spec::new(digest_of("api-ok"))), "uncertain");
}

fn idle_and_stalled_connections_are_closed_within_their_deadlines() {
    let header =
        falcone_serverless_worker::server::ListenerLimits::execute(CONCURRENCY).header_deadline;
    let begun = Instant::now();
    let mut idle = connect(execute());
    let mut sink = [0u8; 16];
    let _ = std::io::Read::read(&mut idle, &mut sink);
    let elapsed = begun.elapsed();
    assert!(
        elapsed >= header - Duration::from_millis(200),
        "{elapsed:?}"
    );
    assert!(elapsed < header + Duration::from_secs(2), "{elapsed:?}");
    recovered();
}

/// Every production-image case, by name.
pub const CASES: &[(&str, fn())] = &[
    ("production-envelope", envelope_and_identity),
    ("production-runtime-mode", other_runtime_modes_are_refused),
    ("production-imports", imports_must_be_declared_and_allowed),
    ("production-budgets", memory_and_fuel_budgets_are_enforced),
    ("production-timeout", the_shortest_timeout_wins),
    (
        "production-routing",
        listeners_refuse_each_others_operations,
    ),
    ("production-saturation", cancel_succeeds_under_full_load),
    (
        "production-http-ceilings",
        http_ceilings_refuse_without_running_a_guest,
    ),
    (
        "production-idle",
        idle_and_stalled_connections_are_closed_within_their_deadlines,
    ),
];

/// Runs every case against the worker container, after it listens.
pub fn run_all() -> bool {
    wait_listening();
    let mut failed = false;
    for (name, case) in CASES {
        let outcome = std::panic::catch_unwind(case);
        failed |= outcome.is_err();
        let status = if outcome.is_ok() { "passed" } else { "failed" };
        println!("{}", json!({"case": name, "status": status}));
    }
    !failed
}
