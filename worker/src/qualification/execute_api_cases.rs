//! The execution API cases: run in process by `execute_api_local.rs` and, one pod at a time,
//! by the restricted-pod runner (`execute_api_runner.rs`). Guests run for real, over real
//! sockets. Nothing here sleeps for a fixed time to synchronise: cases wait on server-side
//! events (a guest started, a stage was reached) with a safety timeout only.
use crate::execute_api_support::*;
use falcone_serverless_worker::artifact::supervisor::CompileLimits;
use falcone_serverless_worker::cache::{CONTRACT_VERSION, Cache};
use falcone_serverless_worker::host::{self, outcome::InvocationOutcome};
use falcone_serverless_worker::server::redact::{
    MARKER, MAX_LINE_CHARS, MAX_LINES, MAX_SECRETS, redact_logs,
};
use serde_json::json;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const LOGGING: &str = "falcone:host/logging@0.2.0";
const CONTEXT: &str = "falcone:host/invocation-context@0.2.0";
const PROGRESS: &str = "falcone:host/progress@0.2.0";

fn execute(cluster: &Cluster, spec: &Spec) -> Answer {
    post(cluster.execute(), "/v1/execute", &spec.bytes())
}

/// Starts an execution and returns its handle once its guest has started.
fn started(cluster: &Cluster, spec: &Spec) -> std::thread::JoinHandle<Answer> {
    let before = cluster.platform.probe.guest_started.count();
    let handle = spawn_post(cluster.execute(), "/v1/execute", spec.bytes());
    cluster.platform.probe.guest_started.wait_for(before + 1);
    handle
}

// --- the legacy envelope and the runtime identity ---------------------------------------------

pub fn a_wasm_revision_returns_the_legacy_envelope_with_its_runtime_identity() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-ok"));
    let spec = Spec::new(digest);
    let answer = execute(&cluster, &spec);
    assert_eq!(answer.status, 200);
    let body = answer.json();
    assert_eq!(body["status"], "success");
    assert_eq!(body["result"], json!({"statusCode": 200, "body": "ok"}));
    assert_eq!(body["logs"], json!([]));
    assert_eq!(body["activation_id"], spec.activation.as_str());
    assert_eq!(
        body["identity"],
        json!({
            "function_revision": "rev-local-1",
            "runtime_mode": "wasm",
            "artifact_digest": digest_text(&digest),
            "wasmtime_version": "49.0.5",
            "language_profile": {"name": "profile-local", "version": "1"},
            "interface_version": "0.2.0"
        })
    );
    // The published parsers accept the envelope against the request it answers.
    let request = falcone_serverless_worker::api::parse_request(&spec.bytes()).unwrap();
    assert!(falcone_serverless_worker::api::parse_result(&answer.body, &request).is_ok());
}

pub fn a_projection_naming_another_engine_is_refused_before_anything_runs() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-ok"));
    let mut spec = Spec::new(digest);
    spec.engine_version = "49.0.4".into();
    let answer = execute(&cluster, &spec);
    assert_eq!(answer.status, 400);
    assert_eq!(
        answer.failure(),
        ("invalid_request".into(), "engine_version_mismatch".into())
    );
    assert_eq!(cluster.platform.artifact_requests.count(), 0);
}

// --- refusals ---------------------------------------------------------------------------------

pub fn another_runtime_mode_is_refused_and_no_instance_is_created() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-ok"));
    for mode in ["oci", "image"] {
        let mut spec = Spec::new(digest);
        spec.runtime_mode = mode.into();
        let answer = execute(&cluster, &spec);
        assert_eq!(answer.status, 409, "{mode}");
        assert_eq!(
            answer.failure(),
            (
                "runtime_mode_not_supported".into(),
                "runtime_mode_not_wasm".into()
            ),
            "{mode}"
        );
    }
    assert_eq!(cluster.platform.artifact_requests.count(), 0);
    assert_eq!(cluster.platform.bind_entered.count(), 0);
    assert_eq!(
        cluster
            .platform
            .probe
            .host_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

pub fn a_malformed_request_is_a_400_and_runs_nothing() {
    let cluster = start(1);
    let answer = post(cluster.execute(), "/v1/execute", b"{\"version\":\"1\"}");
    assert_eq!(answer.status, 400);
    assert_eq!(
        answer.failure(),
        ("invalid_request".into(), "malformed_request".into())
    );
    assert_eq!(cluster.platform.artifact_requests.count(), 0);
}

pub fn an_oversized_action_body_is_refused_before_a_guest_runs() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-ok"));
    let mut spec = Spec::new(digest).imports(&[LOGGING]);
    spec.body_chars = 1_048_577;
    let answer = execute(&cluster, &spec);
    assert_eq!(answer.status, 413);
    assert_eq!(
        answer.failure(),
        ("request_too_large".into(), "body_too_large".into())
    );
    assert_eq!(
        cluster
            .platform
            .probe
            .host_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

// --- declared imports -------------------------------------------------------------------------

fn host_calls(cluster: &Cluster) -> usize {
    cluster
        .platform
        .probe
        .host_calls
        .load(std::sync::atomic::Ordering::SeqCst)
}

pub fn an_import_the_projection_does_not_declare_gets_422_before_guest_code_runs() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-ok"));
    // `imports: []` means none, and a non-empty list that lacks the import is no better.
    for imports in [vec![], vec![PROGRESS]] {
        let answer = execute(&cluster, &Spec::new(digest).imports(&imports));
        assert_eq!(answer.status, 422, "{imports:?}");
        assert_eq!(
            answer.failure(),
            ("undeclared_import".into(), "undeclared_import".into())
        );
    }
    assert_eq!(host_calls(&cluster), 0);
}

pub fn an_ungated_falcone_interface_is_refused_unless_declared() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-context"));
    let refused = execute(&cluster, &Spec::new(digest));
    assert_eq!(
        (refused.status, refused.failure().0.as_str()),
        (422, "undeclared_import")
    );
    let allowed = execute(&cluster, &Spec::new(digest).imports(&[CONTEXT]));
    assert_eq!(allowed.status, 200);
}

pub fn a_declared_import_that_the_worker_does_not_provide_is_still_refused() {
    let cluster = start(1);
    // A component that imports a WASI interface, which the worker never provides.
    let digest = cluster.platform.add(fixture("wasi-import"));
    let spec = Spec::new(digest).imports(&["wasi:cli/environment@0.2.0"]);
    let answer = execute(&cluster, &spec);
    assert_eq!(answer.status, 422);
    assert_eq!(answer.failure().0, "undeclared_import");
}

pub fn a_component_whose_imports_are_all_declared_and_allowed_runs() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-ok"));
    let answer = execute(&cluster, &Spec::new(digest).imports(&[LOGGING]));
    assert_eq!(answer.status, 200);
    assert_eq!(host_calls(&cluster), 1);
}

// --- budgets ----------------------------------------------------------------------------------

pub fn a_one_mebibyte_memory_budget_fails_the_four_mebibyte_fixture_and_enough_passes() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-ok"));
    let mut small = Spec::new(digest);
    small.memory_bytes = 1024 * 1024;
    let answer = execute(&cluster, &small);
    assert_eq!(answer.status, 500);
    assert_eq!(
        answer.failure(),
        ("execution_failed".into(), "memory_budget_exceeded".into())
    );
    let mut enough = Spec::new(digest);
    enough.memory_bytes = 8 * 1024 * 1024;
    assert_eq!(execute(&cluster, &enough).status, 200);
}

pub fn a_memory_budget_above_the_worker_ceiling_is_refused_before_compile() {
    let cluster = start_with(1, |config| {
        config.ceilings.max_memory_bytes = 16 * 1024 * 1024
    });
    let digest = cluster.platform.add(fixture("api-ok"));
    let mut spec = Spec::new(digest);
    spec.memory_bytes = 16 * 1024 * 1024 + 1;
    let answer = execute(&cluster, &spec);
    assert_eq!(answer.status, 400);
    assert_eq!(
        answer.failure(),
        (
            "invalid_request".into(),
            "budget_above_worker_ceiling".into()
        )
    );
    assert_eq!(cluster.platform.artifact_requests.count(), 0);
    spec.memory_bytes = 16 * 1024 * 1024;
    assert_eq!(execute(&cluster, &spec).status, 200);
}

pub fn a_small_fuel_budget_stops_a_looping_guest_well_before_the_timeout() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-spin"));
    let mut spec = Spec::new(digest).imports(&[LOGGING]);
    spec.fuel = 5_000_000;
    spec.timeout_ms = 30_000;
    let begun = Instant::now();
    let answer = execute(&cluster, &spec);
    assert_eq!(answer.status, 500);
    assert_eq!(
        answer.failure(),
        ("execution_failed".into(), "fuel_exhausted".into())
    );
    assert!(
        begun.elapsed() < Duration::from_secs(10),
        "{:?}",
        begun.elapsed()
    );
}

pub fn the_call_after_max_host_calls_is_refused_and_the_host_saw_exactly_n() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-four"));
    let mut spec = Spec::new(digest).imports(&[LOGGING]);
    spec.max_host_calls = 3;
    let answer = execute(&cluster, &spec);
    assert_eq!(answer.status, 500);
    assert_eq!(
        answer.failure(),
        (
            "execution_failed".into(),
            "host_call_budget_exceeded".into()
        )
    );
    assert_eq!(host_calls(&cluster), 3);
    // With room for all four the same guest succeeds.
    spec.activation = "act-local-four-ok".into();
    spec.max_host_calls = 4;
    assert_eq!(execute(&cluster, &spec).status, 200);
}

// --- admission order and listeners ------------------------------------------------------------

/// Runs `spin` guests until `count` are busy; returns their connections' handles and specs.
fn saturate(cluster: &Cluster, count: usize) -> Vec<(Spec, std::thread::JoinHandle<Answer>)> {
    let digest = cluster.platform.add(fixture("api-log-spin"));
    (0..count)
        .map(|_| {
            let spec = Spec::new(digest).imports(&[LOGGING]);
            let handle = started(cluster, &spec);
            (spec, handle)
        })
        .collect()
}

fn release(cluster: &Cluster, busy: Vec<(Spec, std::thread::JoinHandle<Answer>)>) {
    for (spec, handle) in busy {
        assert_eq!(cancel(cluster, &spec), (200, "acknowledged".into()));
        let answer = handle.join().unwrap();
        assert_eq!(
            (answer.status, answer.failure().0.as_str()),
            (499, "cancelled")
        );
    }
}

pub fn with_every_slot_busy_the_body_is_not_read_and_the_answer_is_busy() {
    let cluster = start(1);
    let busy = saturate(&cluster, 1);
    let mut stream = connect(cluster.execute());
    // The headers promise a megabyte; only a fragment of it is sent and the rest never is.
    stream.write_all(&head("/v1/execute", 1_000_000)).unwrap();
    stream.write_all(b"{\"version\":").unwrap();
    let answer = read_answer(&mut stream).expect("busy is answered without the body");
    assert_eq!(answer.status, 503);
    assert_eq!(answer.failure(), ("busy".into(), "no_capacity".into()));
    release(&cluster, busy);
}

pub fn with_every_slot_busy_a_non_wasm_request_with_its_body_buffered_still_gets_409() {
    let cluster = start(1);
    let busy = saturate(&cluster, 1);
    let digest = cluster.platform.add(fixture("api-ok"));
    let mut spec = Spec::new(digest);
    spec.runtime_mode = "oci".into();
    let refused = |answer: Answer| {
        assert_eq!(answer.status, 409);
        assert_eq!(
            answer.failure(),
            (
                "runtime_mode_not_supported".into(),
                "runtime_mode_not_wasm".into()
            )
        );
    };
    // Headers and the complete body in one segment: the body is already buffered.
    let mut stream = connect(cluster.execute());
    let body = spec.bytes();
    let mut message = head("/v1/execute", body.len());
    message.extend_from_slice(&body);
    stream.write_all(&message).unwrap();
    refused(read_answer(&mut stream).unwrap());

    // A complete request well past the head reader's first read: the body is queued on the
    // socket behind the headers, not in the head's buffer.
    spec.body_chars = 100_000;
    let body = spec.bytes();
    assert!(body.len() > 100_000);
    let mut stream = connect(cluster.execute());
    let mut message = head("/v1/execute", body.len());
    message.extend_from_slice(&body);
    stream.write_all(&message).unwrap();
    refused(read_answer(&mut stream).unwrap());

    // The same request delivered in fragments, each its own segment.
    let mut stream = connect(cluster.execute());
    stream.set_nodelay(true).unwrap();
    stream.write_all(&head("/v1/execute", body.len())).unwrap();
    for fragment in body.chunks(1500) {
        stream.write_all(fragment).unwrap();
    }
    refused(read_answer(&mut stream).unwrap());

    // A wasm request in the same position is merely busy.
    let wasm = Spec::new(digest);
    assert_eq!(execute(&cluster, &wasm).status, 503);
    release(&cluster, busy);
}

pub fn each_listener_refuses_the_other_listeners_operations() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-ok"));
    let spec = Spec::new(digest);
    assert_eq!(
        post(cluster.control(), "/v1/execute", &spec.bytes()).status,
        404
    );
    assert_eq!(
        post(cluster.execute(), "/v1/cancel", &spec.cancel()).status,
        404
    );
    assert_eq!(post(cluster.execute(), "/v1/update", b"{}").status, 404);
    assert_eq!(post(cluster.execute(), "/v1/elsewhere", b"{}").status, 404);
    assert_eq!(post(cluster.control(), "/v1/elsewhere", b"{}").status, 404);
    // Nothing ran for any of them.
    assert_eq!(cluster.platform.artifact_requests.count(), 0);
    assert_eq!(execute(&cluster, &spec).status, 200);
}

/// Opens `count` connections that send part of a request and then nothing: even ones stall in
/// the headers, odd ones in the body.
fn stall(
    address: std::net::SocketAddr,
    count: usize,
    path: &str,
    body_promised: usize,
) -> Vec<std::net::TcpStream> {
    (0..count)
        .map(|index| {
            let mut stream = connect(address);
            if index % 2 == 0 {
                // Stalled in the headers.
                stream.write_all(b"POST /v1/exec").unwrap();
            } else {
                // Stalled in the body.
                stream.write_all(&head(path, body_promised)).unwrap();
                stream.write_all(b"{").unwrap();
            }
            stream
        })
        .collect()
}

/// Waits, on the server's own events, until `headers` connections of the listener are
/// waiting in the headers and `bodies` in a body.
fn wait_stalled(
    waiting: &falcone_serverless_worker::server::testing::Waiting,
    headers: usize,
    bodies: usize,
) {
    assert!(
        waiting.in_headers.wait_at_least(headers, SAFETY),
        "the server never saw {headers} connections stalled in the headers"
    );
    assert!(
        waiting.in_body.wait_at_least(bodies, SAFETY),
        "the server never saw {bodies} connections stalled in a body"
    );
}

pub fn a_cancel_succeeds_with_all_slots_busy_and_the_execute_listener_full_of_stalled_connections()
{
    let concurrency = 2;
    let cluster = start(concurrency);
    let admission = falcone_serverless_worker::server::http::ADMISSION_CONNECTIONS;
    let waiting = &cluster.server.observed().execute;
    // While slots are free, every admission connection of the execute listener is taken by a
    // stalled one: half stalled in their headers, half in their bodies. The server reports
    // each of them before anything else happens.
    let stalled = stall(cluster.execute(), admission, "/v1/execute", 1_000);
    wait_stalled(waiting, admission / 2, admission / 2);
    // The rest of the listener's connections run the blocked guests: every slot is busy.
    let busy = saturate(&cluster, concurrency);
    // The stalled connections are all still held when the extra execute arrives.
    assert_eq!(
        (waiting.in_headers.get(), waiting.in_body.get()),
        (admission / 2, admission / 2)
    );
    // One more execute gets 503 busy: the listener has no connection left for it.
    let digest = cluster.platform.add(fixture("api-ok"));
    let more = execute(&cluster, &Spec::new(digest));
    assert_eq!(more.status, 503);
    assert_eq!(more.failure(), ("busy".into(), "no_capacity".into()));
    // A cancel on the control listener is acknowledged and takes effect within the documented
    // deadline: the cancel waits at most `CANCEL_WAIT` for the execution to end.
    let begun = Instant::now();
    let mut busy = busy;
    let (spec, handle) = busy.remove(0);
    assert_eq!(cancel(&cluster, &spec), (200, "acknowledged".into()));
    let answer = handle.join().unwrap();
    assert_eq!(
        (answer.status, answer.failure().0.as_str()),
        (499, "cancelled")
    );
    assert!(begun.elapsed() < falcone_serverless_worker::server::CANCEL_WAIT);
    drop(stalled);
    release(&cluster, busy);
}

pub fn a_full_control_listener_does_not_touch_running_executions_and_recovers_after_its_deadlines()
{
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-ok"));
    let gate = Gate::new();
    *cluster.platform.probe.block.lock().unwrap() = Some(gate.clone());
    let spec = Spec::new(digest).imports(&[LOGGING]);
    let running = started(&cluster, &spec);
    let limits = falcone_serverless_worker::server::ListenerLimits::control();
    let stalled = stall(
        cluster.control(),
        limits.max_connections,
        "/v1/cancel",
        1_000,
    );
    // The control listener is full: its connection limit answers busy at once.
    let refused = post(cluster.control(), "/v1/cancel", &spec.cancel());
    assert_eq!(refused.status, 503);
    // The running execution is unaffected: it finishes when its host call returns.
    gate.open();
    assert_eq!(running.join().unwrap().status, 200);
    // The stalled connections are closed at their deadlines, and then control recovers.
    let begun = Instant::now();
    loop {
        let mut probe = connect(cluster.control());
        let _ = probe.write_all(&head("/v1/cancel", spec.cancel().len()));
        let _ = probe.write_all(&spec.cancel());
        if read_answer(&mut probe).is_some_and(|answer| answer.status == 200) {
            break;
        }
        assert!(
            begun.elapsed() < SAFETY,
            "the control listener never recovered"
        );
    }
    assert!(
        begun.elapsed() < limits.header_deadline + limits.body_deadline + Duration::from_secs(2)
    );
    drop(stalled);
}

pub fn a_cancel_of_a_guest_blocked_in_a_mutating_host_call_is_uncertain() {
    let cluster = start(1);
    let gate = Gate::new();
    *cluster.platform.probe.block.lock().unwrap() = Some(gate.clone());
    let digest = cluster.platform.add(fixture("api-publish"));
    let mut spec = Spec::new(digest).imports(&["falcone:host/events@0.2.0"]);
    spec.topics = vec!["orders".into()];
    let running = started(&cluster, &spec);
    // The publish is blocked inside the host and cannot be interrupted there: the guest
    // is released, but the cancellation is not known to have taken effect.
    assert_eq!(cancel(&cluster, &spec), (200, "uncertain".into()));
    let answer = running.join().unwrap();
    assert_eq!(
        answer.failure(),
        ("execution_failed".into(), "uncertain_side_effects".into())
    );
    gate.open();
}

pub fn a_cancel_of_an_execution_that_is_not_running_is_uncertain() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-ok"));
    assert_eq!(
        cancel(&cluster, &Spec::new(digest)),
        (200, "uncertain".into())
    );
}

pub fn an_update_is_accepted_for_a_running_execution_and_refused_otherwise() {
    let cluster = start(1);
    let busy = saturate(&cluster, 1);
    let (spec, _) = &busy[0];
    let update = serde_json::to_vec(&json!({
        "version": "1", "type": "update",
        "activation_id": spec.activation, "attempt_id": spec.attempt(),
        "sequence": 3, "update": {"kind": "heartbeat"}
    }))
    .unwrap();
    assert_eq!(post(cluster.control(), "/v1/update", &update).status, 204);
    let other = serde_json::to_vec(&json!({
        "version": "1", "type": "update",
        "activation_id": "act-nobody", "attempt_id": "att-nobody",
        "sequence": 1, "update": {"kind": "heartbeat"}
    }))
    .unwrap();
    assert_eq!(post(cluster.control(), "/v1/update", &other).status, 404);
    assert_eq!(post(cluster.control(), "/v1/update", b"{}").status, 400);
    release(&cluster, busy);
}

// --- deadlines --------------------------------------------------------------------------------

/// How much later than its limit a timed-out execution may answer: scheduling and the epoch
/// tick, not an algorithm. The limit itself is far below the other two in every case.
const TOLERANCE: Duration = Duration::from_millis(1000);
const SHORT: Duration = Duration::from_millis(600);

fn assert_timeout(answer: &Answer, spec: &Spec, elapsed: Duration) {
    assert_eq!(
        answer.status,
        504,
        "{}",
        String::from_utf8_lossy(&answer.body[..answer.body.len().min(300)])
    );
    let body = answer.json();
    assert_eq!(body["status"], "failure");
    assert_eq!(body["class"], "non_retryable");
    assert_eq!(body["result"]["code"], "timeout");
    assert_eq!(body["activation_id"], spec.activation.as_str());
    assert_eq!(body["identity"]["wasmtime_version"], "49.0.5");
    assert!(elapsed >= SHORT - Duration::from_millis(100), "{elapsed:?}");
    assert!(elapsed < SHORT + TOLERANCE, "{elapsed:?}");
}

/// A guest that started and never returns, against `spec`: the answer, and how long it took.
fn spin(cluster: &Cluster, spec: &Spec) -> (Answer, Duration) {
    let begun = Instant::now();
    let handle = started(cluster, spec);
    let answer = handle.join().unwrap();
    (answer, begun.elapsed())
}

/// A looping guest's spec. The artifact is compiled first (by a run that fuel stops at once),
/// so that a short limit measures the deadline and not a cold compile.
fn spinner(cluster: &Cluster) -> Spec {
    let spec = Spec::new(cluster.platform.add(fixture("api-log-spin"))).imports(&[LOGGING]);
    let mut warm = spec.clone();
    warm.activation = format!("{}-warm", spec.activation);
    warm.fuel = 1_000_000;
    assert_eq!(execute(cluster, &warm).failure().1, "fuel_exhausted");
    spec
}

fn recovered(cluster: &Cluster) {
    let digest = cluster.platform.add(fixture("api-ok"));
    assert_eq!(execute(cluster, &Spec::new(digest)).status, 200);
}

pub fn a_request_deadline_shorter_than_the_projection_and_the_worker_limit_wins() {
    let cluster = start(1);
    let mut spec = spinner(&cluster);
    spec.deadline_in = SHORT;
    let (answer, elapsed) = spin(&cluster, &spec);
    assert_timeout(&answer, &spec, elapsed);
    recovered(&cluster);
}

pub fn a_projection_limit_shorter_than_the_request_and_the_worker_limit_wins() {
    let cluster = start(1);
    let mut spec = spinner(&cluster);
    spec.timeout_ms = SHORT.as_millis() as u64;
    let (answer, elapsed) = spin(&cluster, &spec);
    assert_timeout(&answer, &spec, elapsed);
    recovered(&cluster);
}

pub fn a_worker_wall_clock_limit_shorter_than_both_wins() {
    let cluster = start_with(1, |config| {
        config.ceilings.max_wall_clock_ms = SHORT.as_millis() as u64
    });
    let spec = spinner(&cluster);
    let (answer, elapsed) = spin(&cluster, &spec);
    assert_timeout(&answer, &spec, elapsed);
    recovered(&cluster);
}

pub fn setup_that_crosses_until_times_out_and_the_guest_never_starts() {
    let cluster = start(1);
    let gate = Gate::new();
    *cluster.platform.bind_gate.lock().unwrap() = Some(gate.clone());
    let digest = cluster.platform.add(fixture("api-log-ok"));
    let mut spec = Spec::new(digest).imports(&[LOGGING]);
    spec.deadline_in = SHORT;
    let begun = Instant::now();
    let handle = spawn_post(cluster.execute(), "/v1/execute", spec.bytes());
    // The bind stage was reached, and it ignores both the deadline and the cancellation.
    cluster.platform.bind_entered.wait_for(1);
    let answer = handle.join().unwrap();
    assert_timeout(&answer, &spec, begun.elapsed());
    assert_eq!(cluster.platform.probe.guest_started.count(), 0);
    assert_eq!(host_calls(&cluster), 0);
    // The slot was not held by the blocked bind.
    *cluster.platform.bind_gate.lock().unwrap() = None;
    recovered(&cluster);
    gate.open();
}

pub fn a_cancel_during_a_blocked_bind_ends_the_execution_and_frees_the_slot() {
    let cluster = start(1);
    let gate = Gate::new();
    *cluster.platform.bind_gate.lock().unwrap() = Some(gate.clone());
    let digest = cluster.platform.add(fixture("api-log-ok"));
    let spec = Spec::new(digest).imports(&[LOGGING]);
    let handle = spawn_post(cluster.execute(), "/v1/execute", spec.bytes());
    cluster.platform.bind_entered.wait_for(1);
    assert_eq!(cancel(&cluster, &spec), (200, "acknowledged".into()));
    let answer = handle.join().unwrap();
    assert_eq!(
        (answer.status, answer.failure().0.as_str()),
        (499, "cancelled")
    );
    assert_eq!(cluster.platform.probe.guest_started.count(), 0);
    *cluster.platform.bind_gate.lock().unwrap() = None;
    recovered(&cluster);
    gate.open();
}

/// A stage that fails while its execution is already cancelled or expired is `cancelled` or
/// `timeout`, whatever the stage reports: the stop condition is checked before the outcome of
/// every helper, failures included.
fn stopped_stage(
    interference: Interference,
    prepare: impl FnOnce(&Cluster, &mut Spec),
    stage: impl Fn(&Cluster, Option<Interference>),
) {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-ok"));
    let mut spec = Spec::new(digest).imports(&[LOGGING]);
    spec.deadline_in = SHORT;
    prepare(&cluster, &mut spec);
    stage(&cluster, Some(interference));
    let begun = Instant::now();
    let answer = execute(&cluster, &spec);
    match interference {
        Interference::Cancel => assert_eq!(
            (answer.status, answer.failure().0.as_str()),
            (499, "cancelled")
        ),
        Interference::Expire => assert_timeout(&answer, &spec, begun.elapsed()),
    }
    // No guest started and the host saw nothing.
    assert_eq!(cluster.platform.probe.guest_started.count(), 0);
    assert_eq!(host_calls(&cluster), 0);
    stage(&cluster, None);
    recovered(&cluster);
}

fn artifact_stage(cluster: &Cluster, interference: Option<Interference>) {
    *cluster.platform.artifact_interference.lock().unwrap() = interference;
}

fn bind_stage(cluster: &Cluster, interference: Option<Interference>) {
    *cluster.platform.bind_interference.lock().unwrap() = interference;
}

/// An artifact the platform cannot provide.
fn unavailable_artifact(_: &Cluster, spec: &mut Spec) {
    spec.digest = digest(b"never added to the platform");
}

/// An artifact the platform provides that the validator rejects.
fn rejected_artifact(cluster: &Cluster, spec: &mut Spec) {
    spec.digest = cluster.platform.add(b"not a component".to_vec());
}

pub fn a_cancel_that_lands_as_the_artifact_is_unavailable_is_cancelled() {
    stopped_stage(Interference::Cancel, unavailable_artifact, artifact_stage);
}

pub fn a_cancel_that_lands_as_the_artifact_is_rejected_is_cancelled() {
    stopped_stage(Interference::Cancel, rejected_artifact, artifact_stage);
}

pub fn a_cancel_that_lands_as_the_bind_fails_is_cancelled() {
    stopped_stage(Interference::Cancel, |_, _| {}, bind_stage);
}

pub fn a_deadline_that_passes_as_the_artifact_is_unavailable_is_a_timeout() {
    stopped_stage(Interference::Expire, unavailable_artifact, artifact_stage);
}

pub fn a_deadline_that_passes_as_the_artifact_is_rejected_is_a_timeout() {
    stopped_stage(Interference::Expire, rejected_artifact, artifact_stage);
}

pub fn a_deadline_that_passes_as_the_bind_fails_is_a_timeout() {
    stopped_stage(Interference::Expire, |_, _| {}, bind_stage);
}

pub fn a_host_call_blocking_near_expiry_ends_at_until_and_a_later_call_never_reaches_the_host() {
    let cluster = start(1);
    let gate = Gate::new();
    *cluster.platform.probe.block.lock().unwrap() = Some(gate.clone());
    let digest = cluster.platform.add(fixture("api-log-four"));
    let mut spec = Spec::new(digest).imports(&[LOGGING]);
    spec.deadline_in = SHORT;
    // The second call would also exceed max_host_calls; the deadline takes precedence.
    spec.max_host_calls = 1;
    let begun = Instant::now();
    let handle = spawn_post(cluster.execute(), "/v1/execute", spec.bytes());
    cluster.platform.probe.guest_started.wait_for(1);
    let answer = handle.join().unwrap();
    // The blocked call ended at `until`, not at `until` plus the 30 s host-call timeout.
    assert_timeout(&answer, &spec, begun.elapsed());
    // The second entry did start after `until`: the server saw it refused, and it never
    // reached the host. It is the deadline that ended the execution, though the same entry
    // would also have exceeded `max_host_calls`.
    assert_eq!(
        falcone_serverless_worker::server::testing::late_entries(&spec.attempt()),
        1
    );
    assert_eq!(host_calls(&cluster), 1);
    gate.open();
}

pub fn a_blocked_host_call_that_ends_before_until_leaves_the_budget_outcome_alone() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-four"));
    let mut spec = Spec::new(digest).imports(&[LOGGING]);
    spec.max_host_calls = 2;
    let answer = execute(&cluster, &spec);
    assert_eq!(
        answer.failure(),
        (
            "execution_failed".into(),
            "host_call_budget_exceeded".into()
        )
    );
    assert_eq!(host_calls(&cluster), 2);
}

// --- artifact validation under the deadline and cancellation ----------------------------------

/// A valid component with one custom section of `payload` bytes. The tag makes the digest
/// unique to one test: validation counters are keyed by digest.
fn padded(tag: &str, payload: usize) -> Vec<u8> {
    let mut bytes = fixture("api-ok");
    let name = tag.as_bytes();
    let size = 1 + name.len() + payload;
    bytes.push(0);
    // LEB128 of the section size.
    let mut rest = size;
    loop {
        let byte = (rest & 0x7f) as u8;
        rest >>= 7;
        if rest == 0 {
            bytes.push(byte);
            break;
        }
        bytes.push(byte | 0x80);
    }
    bytes.push(name.len() as u8);
    bytes.extend_from_slice(name);
    bytes.resize(bytes.len() + payload, 0);
    bytes
}

/// Close to the 64 MiB input bound.
fn near_limit_artifact(tag: &str) -> Vec<u8> {
    padded(tag, 63 * 1024 * 1024)
}

fn wait_validations(digest: &[u8; 32], finished: usize) {
    use falcone_serverless_worker::server::testing::validations;
    let begun = Instant::now();
    while validations(digest).1 < finished {
        assert!(begun.elapsed() < SAFETY, "validation never finished");
        std::thread::yield_now();
    }
}

pub fn a_cancel_during_cold_validation_frees_the_slot_within_the_termination_budget() {
    use falcone_serverless_worker::server::testing::{delay_validation, validations};
    let cluster = start(1);
    let digest = cluster
        .platform
        .add(near_limit_artifact("cancel-during-validation"));
    // The validator is slow: the execution must leave it rather than wait for it.
    delay_validation(digest, Duration::from_secs(4));
    let spec = Spec::new(digest);
    let handle = spawn_post(cluster.execute(), "/v1/execute", spec.bytes());
    let begun = Instant::now();
    while validations(&digest).0 == 0 {
        assert!(begun.elapsed() < SAFETY, "validation never started");
        std::thread::yield_now();
    }
    let cancelled_at = Instant::now();
    assert_eq!(cancel(&cluster, &spec), (200, "acknowledged".into()));
    let answer = handle.join().unwrap();
    assert!(
        cancelled_at.elapsed() < Duration::from_millis(500),
        "{:?}",
        cancelled_at.elapsed()
    );
    assert_eq!(
        (answer.status, answer.failure().0.as_str()),
        (499, "cancelled")
    );
    // The slot is free: a following execute is admitted while the abandoned validator
    // is still running.
    assert_eq!(validations(&digest).1, 0);
    recovered(&cluster);
    wait_validations(&digest, 1);
    // The artifact was validated exactly once.
    assert_eq!(validations(&digest), (1, 1));
}

pub fn a_short_timeout_during_cold_validation_times_out_within_the_termination_budget() {
    use falcone_serverless_worker::server::testing::{delay_validation, validations};
    let cluster = start(1);
    let digest = cluster
        .platform
        .add(near_limit_artifact("timeout-during-validation"));
    delay_validation(digest, Duration::from_secs(4));
    let mut spec = Spec::new(digest);
    spec.timeout_ms = 300;
    let handle = spawn_post(cluster.execute(), "/v1/execute", spec.bytes());
    let begun = Instant::now();
    while validations(&digest).0 == 0 {
        assert!(begun.elapsed() < SAFETY, "validation never started");
        std::thread::yield_now();
    }
    // Validation has started, so the 300 ms limit is already running; the execution leaves
    // the validator at the limit and the answer follows within the termination budget.
    let started = Instant::now();
    let answer = handle.join().unwrap();
    assert_eq!(answer.status, 504);
    assert_eq!(answer.failure().0, "timeout");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    recovered(&cluster);
    wait_validations(&digest, 1);
    assert_eq!(validations(&digest), (1, 1));
}

pub fn a_cold_artifact_is_validated_once_per_execution() {
    use falcone_serverless_worker::server::testing::validations;
    let cluster = start(1);
    let digest = cluster.platform.add(padded("validated-once", 1024));
    assert_eq!(execute(&cluster, &Spec::new(digest)).status, 200);
    assert_eq!(validations(&digest), (1, 1));
    assert_eq!(execute(&cluster, &Spec::new(digest)).status, 200);
    assert_eq!(validations(&digest), (2, 2));
}

// --- credential redaction through the service -------------------------------------------------

pub fn the_credential_is_redacted_from_the_logs_of_a_result() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-ok"));
    let mut spec = Spec::new(digest).imports(&[LOGGING]);
    // The guest logs "hello"; the credential contains that string.
    spec.tenant = "hello".into();
    let answer = execute(&cluster, &spec);
    assert_eq!(answer.status, 200);
    let logs = answer.json()["logs"].clone();
    assert_eq!(logs, json!(["Info: <redacted>"]));
    assert!(!String::from_utf8_lossy(&answer.body).contains("hello"));
}

pub fn the_credential_is_absent_from_guest_memory() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-probe"));
    // The guest scans all of its linear memory for a string of the credential.
    let answer = execute(&cluster, &Spec::new(digest));
    assert_eq!(answer.status, 200);
    assert_eq!(answer.json()["result"]["body"], "absent");
    // The probe sees what the guest is given: the same string in the parameters is found.
    let mut leaky = Spec::new(digest);
    leaky.note = Some("issuer-canary-local".into());
    assert_eq!(
        execute(&cluster, &leaky).json()["result"]["body"],
        "present"
    );
}

pub fn a_maximum_size_log_result_validates_and_is_bounded() {
    let cluster = start(1);
    let digest = cluster.platform.add(fixture("api-log-big"));
    let spec = Spec::new(digest).imports(&[LOGGING]);
    let answer = execute(&cluster, &spec);
    assert_eq!(answer.status, 200);
    let request = falcone_serverless_worker::api::parse_request(&spec.bytes()).unwrap();
    // `parse_result` enforces at most 1000 lines of at most 8192 characters.
    assert!(falcone_serverless_worker::api::parse_result(&answer.body, &request).is_ok());
    let logs = answer.json()["logs"].clone();
    assert_eq!(logs.as_array().unwrap().len(), 1000);
    assert!(
        logs.as_array()
            .unwrap()
            .iter()
            .all(|line| line.as_str().unwrap().chars().count() <= 8192)
    );
}

// --- HTTP ceilings and transfer deadlines -----------------------------------------------------

/// After a refused or closed connection, a normal execute and a cancel both succeed: no
/// connection slot, execution slot or control capacity leaked.
fn slots_recovered(cluster: &Cluster) {
    recovered(cluster);
    let digest = cluster.platform.add(fixture("api-ok"));
    assert_eq!(
        cancel(cluster, &Spec::new(digest)),
        (200, "uncertain".into())
    );
}

/// Reads until the peer closes or the safety timeout passes; the time it took.
fn closed_after(stream: &mut std::net::TcpStream) -> Duration {
    let begun = Instant::now();
    let mut sink = [0u8; 1024];
    loop {
        match std::io::Read::read(stream, &mut sink) {
            Ok(0) | Err(_) => return begun.elapsed(),
            Ok(_) => {}
        }
    }
}

pub fn oversized_headers_are_refused_without_running_a_guest() {
    let cluster = start(1);
    let mut stream = connect(cluster.execute());
    let mut request = b"POST /v1/execute HTTP/1.1\r\n".to_vec();
    while request.len() < 20 * 1024 {
        request.extend_from_slice(b"X-Padding: 0123456789012345678901234567890123456789\r\n");
    }
    request.extend_from_slice(b"Content-Length: 0\r\n\r\n");
    let _ = stream.write_all(&request);
    // Refused with the documented status, or closed.
    if let Some(answer) = read_answer(&mut stream) {
        assert_eq!(answer.status, 431);
    }
    assert_eq!(cluster.platform.artifact_requests.count(), 0);
    slots_recovered(&cluster);
}

pub fn an_oversized_request_is_refused_before_its_body_is_read() {
    let cluster = start(1);
    let mut stream = connect(cluster.execute());
    // 8 MiB is the ceiling; ask for one byte more and send none of it.
    stream
        .write_all(&head("/v1/execute", 8 * 1024 * 1024 + 1))
        .unwrap();
    let answer = read_answer(&mut stream).expect("refused");
    assert_eq!(answer.status, 413);
    assert_eq!(
        answer.failure(),
        ("request_too_large".into(), "body_too_large".into())
    );
    assert_eq!(cluster.platform.artifact_requests.count(), 0);
    slots_recovered(&cluster);
}

pub fn an_oversized_control_message_is_refused_on_the_control_listener() {
    let cluster = start(1);
    let mut stream = connect(cluster.control());
    stream
        .write_all(&head("/v1/cancel", 64 * 1024 + 1))
        .unwrap();
    let answer = read_answer(&mut stream).expect("refused");
    assert_eq!(answer.status, 413);
    slots_recovered(&cluster);
}

pub fn an_idle_connection_is_closed_within_the_header_deadline() {
    let cluster = start(1);
    let limits = falcone_serverless_worker::server::ListenerLimits::execute(1);
    let mut stream = connect(cluster.execute());
    let elapsed = closed_after(&mut stream);
    assert!(
        elapsed >= limits.header_deadline - Duration::from_millis(200),
        "{elapsed:?}"
    );
    assert!(
        elapsed < limits.header_deadline + Duration::from_secs(2),
        "{elapsed:?}"
    );
    slots_recovered(&cluster);
}

pub fn a_header_drip_is_closed_at_the_absolute_header_deadline() {
    let cluster = start(1);
    let limits = falcone_serverless_worker::server::ListenerLimits::execute(1);
    let mut stream = connect(cluster.execute());
    let begun = Instant::now();
    let mut writer = stream.try_clone().unwrap();
    // One byte every 200 ms would keep a per-read timeout alive forever.
    let drip = std::thread::spawn(move || {
        for byte in b"POST /v1/execute HTTP/1.1\r\nX-Slow: 1"
            .iter()
            .cycle()
            .take(200)
        {
            if writer.write_all(&[*byte]).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    let elapsed = closed_after(&mut stream);
    assert!(
        begun.elapsed() >= limits.header_deadline - Duration::from_millis(200),
        "{elapsed:?}"
    );
    assert!(
        elapsed < limits.header_deadline + Duration::from_secs(2),
        "{elapsed:?}"
    );
    drip.join().unwrap();
    slots_recovered(&cluster);
}

pub fn a_stalled_body_is_closed_at_the_absolute_body_deadline() {
    let cluster = start(1);
    let limits = falcone_serverless_worker::server::ListenerLimits::execute(1);
    let mut stream = connect(cluster.execute());
    stream.write_all(&head("/v1/execute", 1000)).unwrap();
    stream.write_all(b"{\"version\":").unwrap();
    let elapsed = closed_after(&mut stream);
    assert!(
        elapsed >= limits.body_deadline - Duration::from_millis(500),
        "{elapsed:?}"
    );
    assert!(
        elapsed < limits.body_deadline + Duration::from_secs(2),
        "{elapsed:?}"
    );
    assert_eq!(cluster.platform.artifact_requests.count(), 0);
    slots_recovered(&cluster);
}

pub fn a_slow_response_reader_is_cut_off_at_the_absolute_response_deadline() {
    let limits = falcone_serverless_worker::server::ListenerLimits::execute(1);
    // One connection in all, so that a reader that holds it is visible.
    let cluster = start_with(1, |config| config.execute.max_connections = 1);
    let digest = cluster.platform.add(fixture("api-log-big"));
    let spec = Spec::new(digest).imports(&[LOGGING]);
    let mut stream = connect(cluster.execute());
    stream
        .write_all(&head("/v1/execute", spec.bytes().len()))
        .unwrap();
    stream.write_all(&spec.bytes()).unwrap();
    // The response (tens of megabytes) begins when the execution ends. Read one byte at a
    // time, far slower than the deadline allows, until the first byte arrives.
    let mut byte = [0u8; 1];
    std::io::Read::read_exact(&mut stream, &mut byte).unwrap();
    let began = Instant::now();
    // While the reader dawdles it holds the listener's only connection.
    assert_eq!(
        post(cluster.execute(), "/v1/execute", &spec.bytes()).status,
        503
    );
    // Its connection is released at the absolute deadline, and then a normal execute works.
    let released = loop {
        let digest = cluster.platform.add(fixture("api-ok"));
        let mut probe = connect(cluster.execute());
        let ok = Spec::new(digest);
        let _ = probe.write_all(&head("/v1/execute", ok.bytes().len()));
        let _ = probe.write_all(&ok.bytes());
        if read_answer(&mut probe).is_some_and(|answer| answer.status == 200) {
            break began.elapsed();
        }
        // Read a byte, slowly, so that the reader never catches up.
        let _ = std::io::Read::read(&mut stream, &mut byte);
        assert!(
            began.elapsed() < SAFETY,
            "the slow reader was never cut off"
        );
    };
    assert!(
        released >= limits.response_deadline - Duration::from_secs(1),
        "{released:?}"
    );
    assert!(
        released < limits.response_deadline + Duration::from_secs(3),
        "{released:?}"
    );
    // A cancel succeeds too.
    assert_eq!(
        cancel(&cluster, &Spec::new(digest)),
        (200, "uncertain".into())
    );
}

// --- redaction --------------------------------------------------------------------------------

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

pub fn redaction_overlapping_secrets_leave_no_fragment_of_either() {
    let out = redact_logs(strings(&["xabcdefx"]), &strings(&["abcd", "cdef"]));
    assert_eq!(out, ["x<redacted>x"]);
    let out = redact_logs(strings(&["abcdefgh"]), &strings(&["abcdef", "cdefgh"]));
    assert_eq!(out, ["<redacted>"]);
}

pub fn redaction_secrets_that_share_a_prefix_are_fully_redacted() {
    let out = redact_logs(
        strings(&["key-secret-1 key key-secre"]),
        &strings(&["key", "key-secret"]),
    );
    assert_eq!(out, ["<redacted>-1 <redacted> <redacted>-secre"]);
}

pub fn redaction_a_secret_inside_another_is_covered_once() {
    let out = redact_logs(strings(&["a bcd e"]), &strings(&["bcd", "c"]));
    assert_eq!(out, ["a <redacted> e"]);
}

pub fn redaction_lines_without_secrets_and_logs_without_lines_pass_through() {
    assert_eq!(
        redact_logs(strings(&["plain"]), &strings(&["other"])),
        ["plain"]
    );
    assert_eq!(redact_logs(strings(&["plain"]), &[]), ["plain"]);
    assert!(redact_logs(Vec::new(), &strings(&["secret"])).is_empty());
}

pub fn redaction_each_line_is_cut_before_redaction_and_the_output_is_cut_again() {
    // The secret after the cut is never matched, and a line is never longer than the limit.
    let long = format!("{}{}", "x".repeat(MAX_LINE_CHARS), "secret");
    let out = redact_logs(vec![long], &strings(&["secret"]));
    assert_eq!(out[0].chars().count(), MAX_LINE_CHARS);
    assert!(!out[0].contains(MARKER));
    // One-character secrets grow a line tenfold; the result is cut back to the limit.
    let out = redact_logs(vec!["a".repeat(MAX_LINE_CHARS)], &strings(&["a"]));
    assert_eq!(out[0].chars().count(), MAX_LINE_CHARS);
    assert!(out[0].starts_with(MARKER));
    // Multi-byte characters count as characters.
    let out = redact_logs(vec!["é".repeat(MAX_LINE_CHARS + 5)], &[]);
    assert_eq!(out[0].chars().count(), MAX_LINE_CHARS);
}

pub fn redaction_at_most_one_thousand_lines_are_returned() {
    let lines = vec!["line".to_owned(); MAX_LINES + 10];
    assert_eq!(redact_logs(lines, &strings(&["secret"])).len(), MAX_LINES);
}

pub fn redaction_the_maximum_number_of_secrets_and_a_thousand_maximum_length_lines_finish_in_time()
{
    let secrets: Vec<String> = (0..MAX_SECRETS).map(|i| format!("k{i:05}")).collect();
    // Each line is full of secrets, so every line has about a thousand matches.
    let line: String = (0..1200)
        .map(|i| format!("k{:05} ", (i * 37) % MAX_SECRETS))
        .collect::<String>()
        .chars()
        .take(MAX_LINE_CHARS)
        .collect();
    let lines = vec![line; MAX_LINES];
    let begun = Instant::now();
    let out = redact_logs(lines, &secrets);
    let elapsed = begun.elapsed();
    // The documented bound is one second in a release build; a debug build is not timed
    // against it.
    let bound = if cfg!(debug_assertions) {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(1)
    };
    assert!(elapsed < bound, "{elapsed:?}");
    assert_eq!(out.len(), MAX_LINES);
    assert!(
        out.iter()
            .all(|line| line.chars().count() <= MAX_LINE_CHARS)
    );
    assert!(
        out.iter()
            .all(|line| !line.contains("k0") && line.contains(MARKER))
    );
}

pub fn redaction_past_the_secret_budget_the_result_is_only_the_marker() {
    let secrets: Vec<String> = (0..=MAX_SECRETS).map(|i| format!("k{i:05}")).collect();
    let out = redact_logs(strings(&["k00001 is a secret", "other"]), &secrets);
    assert_eq!(out, [MARKER]);
    let big = vec!["s".repeat(1024 * 1024 + 1)];
    assert_eq!(redact_logs(strings(&["s"]), &big), [MARKER]);
}

pub fn redaction_past_the_match_budget_the_result_is_only_the_marker_and_quickly() {
    // Prefix-sharing secrets of every length: every position of 'a's matches ~1000 of them.
    let secrets: Vec<String> = (1..=1000).map(|n| "a".repeat(n)).collect();
    let lines = vec!["a".repeat(MAX_LINE_CHARS); MAX_LINES];
    let begun = Instant::now();
    let out = redact_logs(lines, &secrets);
    assert_eq!(out, [MARKER]);
    let bound = if cfg!(debug_assertions) {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(1)
    };
    assert!(begun.elapsed() < bound, "{:?}", begun.elapsed());
}

// --- the host layer under the deadline --------------------------------------------------------

fn host_loaded(name: &str) -> (falcone_serverless_worker::cache::Loaded, std::path::PathBuf) {
    let directory = std::env::temp_dir().join(format!("host-layer-{}-{name}", std::process::id()));
    let mut cache = Cache::new(&directory, compiler_path()).unwrap();
    let limits = CompileLimits {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    };
    let loaded = cache
        .compile(
            CONTRACT_VERSION,
            &fixture(name),
            limits,
            &AtomicBool::new(false),
        )
        .unwrap();
    (loaded, directory)
}

pub fn host_the_handler_is_not_invoked_when_until_passes_after_instantiation() {
    let (loaded, directory) = host_loaded("api-log-ok");
    let probe = Arc::new(Probe::default());
    let host = host::Host::new(
        host::Context::default(),
        Arc::new(ProbeBindings(probe.clone())),
    );
    let until = Instant::now() + Duration::from_secs(1);
    let prepared = host::Prepared::with_until(&loaded, host, until)
        .unwrap_or_else(|execution| panic!("instantiation failed: {:?}", execution.result));
    // Instantiation is done; hold the guest back until `until` has passed.
    while Instant::now() < until {
        std::thread::yield_now();
    }
    let before = host::handler_invocations();
    let execution = prepared.call(b"{}");
    assert_eq!(execution.outcome, InvocationOutcome::GuestInterrupted);
    // The guest never started: the handler was not invoked, and the host saw no call.
    assert_eq!(host::handler_invocations(), before);
    assert_eq!(probe.host_calls.load(Ordering::SeqCst), 0);
    let _ = std::fs::remove_dir_all(directory);
}

pub fn host_a_host_counts_every_entry_against_max_host_calls() {
    let (loaded, directory) = host_loaded("api-log-four");
    let probe = Arc::new(Probe::default());
    let mut host = host::Host::new(
        host::Context::default(),
        Arc::new(ProbeBindings(probe.clone())),
    );
    host.set_budgets(u64::MAX, 2);
    let until = Instant::now() + Duration::from_secs(30);
    let execution = host::execute_until(&loaded, host, b"{}", until);
    assert_eq!(execution.outcome, InvocationOutcome::HostCallBudget);
    assert_eq!(
        execution.result.unwrap_err().code,
        "host_call_budget_exceeded"
    );
    assert_eq!(probe.host_calls.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(directory);
}

/// Every case, by name, for the pod runner and the registration check.
pub const CASES: &[(&str, fn())] = &[
    (
        "a_wasm_revision_returns_the_legacy_envelope_with_its_runtime_identity",
        a_wasm_revision_returns_the_legacy_envelope_with_its_runtime_identity,
    ),
    (
        "a_projection_naming_another_engine_is_refused_before_anything_runs",
        a_projection_naming_another_engine_is_refused_before_anything_runs,
    ),
    (
        "another_runtime_mode_is_refused_and_no_instance_is_created",
        another_runtime_mode_is_refused_and_no_instance_is_created,
    ),
    (
        "a_malformed_request_is_a_400_and_runs_nothing",
        a_malformed_request_is_a_400_and_runs_nothing,
    ),
    (
        "an_oversized_action_body_is_refused_before_a_guest_runs",
        an_oversized_action_body_is_refused_before_a_guest_runs,
    ),
    (
        "an_import_the_projection_does_not_declare_gets_422_before_guest_code_runs",
        an_import_the_projection_does_not_declare_gets_422_before_guest_code_runs,
    ),
    (
        "an_ungated_falcone_interface_is_refused_unless_declared",
        an_ungated_falcone_interface_is_refused_unless_declared,
    ),
    (
        "a_declared_import_that_the_worker_does_not_provide_is_still_refused",
        a_declared_import_that_the_worker_does_not_provide_is_still_refused,
    ),
    (
        "a_component_whose_imports_are_all_declared_and_allowed_runs",
        a_component_whose_imports_are_all_declared_and_allowed_runs,
    ),
    (
        "a_one_mebibyte_memory_budget_fails_the_four_mebibyte_fixture_and_enough_passes",
        a_one_mebibyte_memory_budget_fails_the_four_mebibyte_fixture_and_enough_passes,
    ),
    (
        "a_memory_budget_above_the_worker_ceiling_is_refused_before_compile",
        a_memory_budget_above_the_worker_ceiling_is_refused_before_compile,
    ),
    (
        "a_small_fuel_budget_stops_a_looping_guest_well_before_the_timeout",
        a_small_fuel_budget_stops_a_looping_guest_well_before_the_timeout,
    ),
    (
        "the_call_after_max_host_calls_is_refused_and_the_host_saw_exactly_n",
        the_call_after_max_host_calls_is_refused_and_the_host_saw_exactly_n,
    ),
    (
        "with_every_slot_busy_the_body_is_not_read_and_the_answer_is_busy",
        with_every_slot_busy_the_body_is_not_read_and_the_answer_is_busy,
    ),
    (
        "with_every_slot_busy_a_non_wasm_request_with_its_body_buffered_still_gets_409",
        with_every_slot_busy_a_non_wasm_request_with_its_body_buffered_still_gets_409,
    ),
    (
        "each_listener_refuses_the_other_listeners_operations",
        each_listener_refuses_the_other_listeners_operations,
    ),
    (
        "a_cancel_succeeds_with_all_slots_busy_and_the_execute_listener_full_of_stalled_connections",
        a_cancel_succeeds_with_all_slots_busy_and_the_execute_listener_full_of_stalled_connections,
    ),
    (
        "a_full_control_listener_does_not_touch_running_executions_and_recovers_after_its_deadlines",
        a_full_control_listener_does_not_touch_running_executions_and_recovers_after_its_deadlines,
    ),
    (
        "a_cancel_of_a_guest_blocked_in_a_mutating_host_call_is_uncertain",
        a_cancel_of_a_guest_blocked_in_a_mutating_host_call_is_uncertain,
    ),
    (
        "a_cancel_of_an_execution_that_is_not_running_is_uncertain",
        a_cancel_of_an_execution_that_is_not_running_is_uncertain,
    ),
    (
        "an_update_is_accepted_for_a_running_execution_and_refused_otherwise",
        an_update_is_accepted_for_a_running_execution_and_refused_otherwise,
    ),
    (
        "a_request_deadline_shorter_than_the_projection_and_the_worker_limit_wins",
        a_request_deadline_shorter_than_the_projection_and_the_worker_limit_wins,
    ),
    (
        "a_projection_limit_shorter_than_the_request_and_the_worker_limit_wins",
        a_projection_limit_shorter_than_the_request_and_the_worker_limit_wins,
    ),
    (
        "a_worker_wall_clock_limit_shorter_than_both_wins",
        a_worker_wall_clock_limit_shorter_than_both_wins,
    ),
    (
        "setup_that_crosses_until_times_out_and_the_guest_never_starts",
        setup_that_crosses_until_times_out_and_the_guest_never_starts,
    ),
    (
        "a_cancel_during_a_blocked_bind_ends_the_execution_and_frees_the_slot",
        a_cancel_during_a_blocked_bind_ends_the_execution_and_frees_the_slot,
    ),
    (
        "a_cancel_that_lands_as_the_artifact_is_unavailable_is_cancelled",
        a_cancel_that_lands_as_the_artifact_is_unavailable_is_cancelled,
    ),
    (
        "a_cancel_that_lands_as_the_artifact_is_rejected_is_cancelled",
        a_cancel_that_lands_as_the_artifact_is_rejected_is_cancelled,
    ),
    (
        "a_cancel_that_lands_as_the_bind_fails_is_cancelled",
        a_cancel_that_lands_as_the_bind_fails_is_cancelled,
    ),
    (
        "a_deadline_that_passes_as_the_artifact_is_unavailable_is_a_timeout",
        a_deadline_that_passes_as_the_artifact_is_unavailable_is_a_timeout,
    ),
    (
        "a_deadline_that_passes_as_the_artifact_is_rejected_is_a_timeout",
        a_deadline_that_passes_as_the_artifact_is_rejected_is_a_timeout,
    ),
    (
        "a_deadline_that_passes_as_the_bind_fails_is_a_timeout",
        a_deadline_that_passes_as_the_bind_fails_is_a_timeout,
    ),
    (
        "a_host_call_blocking_near_expiry_ends_at_until_and_a_later_call_never_reaches_the_host",
        a_host_call_blocking_near_expiry_ends_at_until_and_a_later_call_never_reaches_the_host,
    ),
    (
        "a_blocked_host_call_that_ends_before_until_leaves_the_budget_outcome_alone",
        a_blocked_host_call_that_ends_before_until_leaves_the_budget_outcome_alone,
    ),
    (
        "a_cancel_during_cold_validation_frees_the_slot_within_the_termination_budget",
        a_cancel_during_cold_validation_frees_the_slot_within_the_termination_budget,
    ),
    (
        "a_short_timeout_during_cold_validation_times_out_within_the_termination_budget",
        a_short_timeout_during_cold_validation_times_out_within_the_termination_budget,
    ),
    (
        "a_cold_artifact_is_validated_once_per_execution",
        a_cold_artifact_is_validated_once_per_execution,
    ),
    (
        "the_credential_is_redacted_from_the_logs_of_a_result",
        the_credential_is_redacted_from_the_logs_of_a_result,
    ),
    (
        "the_credential_is_absent_from_guest_memory",
        the_credential_is_absent_from_guest_memory,
    ),
    (
        "a_maximum_size_log_result_validates_and_is_bounded",
        a_maximum_size_log_result_validates_and_is_bounded,
    ),
    (
        "oversized_headers_are_refused_without_running_a_guest",
        oversized_headers_are_refused_without_running_a_guest,
    ),
    (
        "an_oversized_request_is_refused_before_its_body_is_read",
        an_oversized_request_is_refused_before_its_body_is_read,
    ),
    (
        "an_oversized_control_message_is_refused_on_the_control_listener",
        an_oversized_control_message_is_refused_on_the_control_listener,
    ),
    (
        "an_idle_connection_is_closed_within_the_header_deadline",
        an_idle_connection_is_closed_within_the_header_deadline,
    ),
    (
        "a_header_drip_is_closed_at_the_absolute_header_deadline",
        a_header_drip_is_closed_at_the_absolute_header_deadline,
    ),
    (
        "a_stalled_body_is_closed_at_the_absolute_body_deadline",
        a_stalled_body_is_closed_at_the_absolute_body_deadline,
    ),
    (
        "a_slow_response_reader_is_cut_off_at_the_absolute_response_deadline",
        a_slow_response_reader_is_cut_off_at_the_absolute_response_deadline,
    ),
    (
        "redaction_overlapping_secrets_leave_no_fragment_of_either",
        redaction_overlapping_secrets_leave_no_fragment_of_either,
    ),
    (
        "redaction_secrets_that_share_a_prefix_are_fully_redacted",
        redaction_secrets_that_share_a_prefix_are_fully_redacted,
    ),
    (
        "redaction_a_secret_inside_another_is_covered_once",
        redaction_a_secret_inside_another_is_covered_once,
    ),
    (
        "redaction_lines_without_secrets_and_logs_without_lines_pass_through",
        redaction_lines_without_secrets_and_logs_without_lines_pass_through,
    ),
    (
        "redaction_each_line_is_cut_before_redaction_and_the_output_is_cut_again",
        redaction_each_line_is_cut_before_redaction_and_the_output_is_cut_again,
    ),
    (
        "redaction_at_most_one_thousand_lines_are_returned",
        redaction_at_most_one_thousand_lines_are_returned,
    ),
    (
        "redaction_the_maximum_number_of_secrets_and_a_thousand_maximum_length_lines_finish_in_time",
        redaction_the_maximum_number_of_secrets_and_a_thousand_maximum_length_lines_finish_in_time,
    ),
    (
        "redaction_past_the_secret_budget_the_result_is_only_the_marker",
        redaction_past_the_secret_budget_the_result_is_only_the_marker,
    ),
    (
        "redaction_past_the_match_budget_the_result_is_only_the_marker_and_quickly",
        redaction_past_the_match_budget_the_result_is_only_the_marker_and_quickly,
    ),
    (
        "host_the_handler_is_not_invoked_when_until_passes_after_instantiation",
        host_the_handler_is_not_invoked_when_until_passes_after_instantiation,
    ),
    (
        "host_a_host_counts_every_entry_against_max_host_calls",
        host_a_host_counts_every_entry_against_max_host_calls,
    ),
];
