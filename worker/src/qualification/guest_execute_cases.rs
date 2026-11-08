//! Guest execute cases, run in-process by `cargo test` (fast feedback) and again by the
//! qualification runner's `--guest-execute` flag in the restricted cluster pod. Each case
//! is one security-relevant criterion of the `falcone:host@0.2.0` handler export.
use crate::handler_fixtures as fixtures;
use crate::host::execute::{ExecuteError, execute};
use crate::host::{Bindings, Context, Host, HostError, Operation, Reply};
use crate::limits::Limits;
use engine::component::Component;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use wasmtime as engine;

const TOLERANCE: Duration = Duration::from_secs(2);
const CEILING: usize = 1_048_576;

/// Every case, by the name the cluster pod reports it under.
pub const CASES: [(&str, fn()); 10] = [
    (
        "successful_handler_call",
        successful_handler_call_returns_exactly_the_params,
    ),
    (
        "size_ceilings",
        params_and_result_ceilings_are_enforced_before_any_output,
    ),
    (
        "timeout_isolation",
        a_looping_guest_times_out_while_another_store_completes,
    ),
    (
        "cancellation_precedence",
        cancellation_wins_on_success_and_on_every_handler_error,
    ),
    (
        "deadline_precedence",
        the_deadline_wins_on_success_and_on_every_handler_error,
    ),
    (
        "host_call_timeout_is_not_cancellation",
        a_host_call_timeout_is_not_a_cancellation,
    ),
    (
        "oversized_guest_data",
        oversized_results_and_error_payloads_are_refused_unread,
    ),
    ("bounded_errors", every_failure_maps_to_a_bounded_error),
    (
        "signature_rejection",
        handler_gate_rejects_before_any_host_call,
    ),
    (
        "limits_are_configurable",
        configured_ceilings_replace_the_defaults,
    ),
];

/// A binding that counts every call and can run a hook inside the call, which is how a
/// case cancels, or outlives the deadline, "during the guest's last host call".
#[derive(Default)]
struct Recording {
    calls: AtomicUsize,
    during_call: Mutex<Option<Box<dyn Fn() + Send>>>,
}
impl Recording {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
    fn on_call(&self, hook: impl Fn() + Send + 'static) {
        *self.during_call.lock().unwrap() = Some(Box::new(hook));
    }
}
impl Bindings for Recording {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        assert!(matches!(operation, Operation::Log { .. }));
        self.calls.fetch_add(1, Ordering::AcqRel);
        if let Some(hook) = self.during_call.lock().unwrap().as_ref() {
            hook();
        }
        Ok(Reply::Empty)
    }
}

fn shared_engine() -> engine::Engine {
    let mut config = engine::Config::new();
    // The trusted cache's configuration: fuel on, epoch interruption on.
    config.consume_fuel(true).epoch_interruption(true);
    engine::Engine::new(&config).unwrap()
}

fn component(engine: &engine::Engine, wasm: &[u8]) -> Component {
    Component::new(engine, wasm).unwrap()
}

fn limits(guest_deadline_ms: u64, host_call_ms: u64, to_host: usize, to_guest: usize) -> Limits {
    Limits::parse(
        &serde_json::json!({
            "version": 1, "compiler_cpu_seconds": 120,
            "compiler_memory_bytes": 4294967296_u64, "guest_memory_bytes": 268435456,
            "guest_to_host_bytes": to_host, "host_to_guest_bytes": to_guest,
            "guest_deadline_ms": guest_deadline_ms, "host_call_deadline_ms": host_call_ms,
            "termination_budget_ms": 500
        })
        .to_string(),
    )
    .unwrap()
}

fn host(binding: Arc<Recording>, limits: Limits) -> Host {
    Host::with_limits(Context::default(), binding, limits)
}

fn run(component: &Component, params: &[u8]) -> Result<Vec<u8>, ExecuteError> {
    execute(
        component,
        host(Arc::new(Recording::default()), Limits::compatibility()),
        params,
    )
}

fn code(result: Result<Vec<u8>, ExecuteError>) -> &'static str {
    result.expect_err("the invocation must fail").code
}

fn mode(byte: u8, rest: &[u8]) -> Vec<u8> {
    [&[byte][..], rest].concat()
}

#[cfg_attr(test, test)]
pub fn successful_handler_call_returns_exactly_the_params() {
    let engine = shared_engine();
    let echo = component(&engine, fixtures::ECHO);
    for params in [
        b"".to_vec(),
        b"{\"legacy\":\"envelope\"}".to_vec(),
        (0..=255).collect::<Vec<u8>>(),
    ] {
        assert_eq!(run(&echo, &params).unwrap(), params);
    }
    // The same component can serve a second, independent invocation.
    assert_eq!(run(&echo, b"again").unwrap(), b"again");
}

#[cfg_attr(test, test)]
pub fn params_and_result_ceilings_are_enforced_before_any_output() {
    let engine = shared_engine();
    let echo = component(&engine, fixtures::ECHO);
    let exact = vec![7; CEILING];
    assert_eq!(run(&echo, &exact).unwrap(), exact);
    let over = vec![7; CEILING + 1];
    assert_eq!(code(run(&echo, &over)), "buffer_limit_exceeded");

    let modes = component(&engine, fixtures::MODES);
    let sized = |bytes: usize| mode(b's', &(bytes as u32).to_le_bytes());
    assert_eq!(run(&modes, &sized(CEILING)).unwrap().len(), CEILING);
    assert_eq!(
        code(run(&modes, &sized(CEILING + 1))),
        "buffer_limit_exceeded"
    );
}

#[cfg_attr(test, test)]
pub fn configured_ceilings_replace_the_defaults() {
    let engine = shared_engine();
    let echo = component(&engine, fixtures::ECHO);
    let small = limits(30_000, 30_000, 128, 64);
    let invoke =
        |params: &[u8]| execute(&echo, host(Arc::new(Recording::default()), small), params);
    // Params are host-to-guest (64); the echoed result is guest-to-host (128).
    assert_eq!(invoke(&[1; 64]).unwrap(), vec![1; 64]);
    assert_eq!(code(invoke(&[1; 65])), "buffer_limit_exceeded");
    let modes = component(&engine, fixtures::MODES);
    let sized = |bytes: u32| mode(b's', &bytes.to_le_bytes());
    let invoke =
        |params: &[u8]| execute(&modes, host(Arc::new(Recording::default()), small), params);
    assert_eq!(invoke(&sized(128)).unwrap().len(), 128);
    assert_eq!(code(invoke(&sized(129))), "buffer_limit_exceeded");
}

#[cfg_attr(test, test)]
pub fn a_looping_guest_times_out_while_another_store_completes() {
    let engine = shared_engine();
    let modes = component(&engine, fixtures::MODES);
    // The neighbour is inside a host call that outlives the looping guest's deadline.
    let neighbour_binding = Arc::new(Recording::default());
    neighbour_binding.on_call(|| std::thread::sleep(Duration::from_millis(600)));
    let entered = neighbour_binding.clone();
    std::thread::scope(|scope| {
        let neighbour = scope.spawn(|| {
            execute(
                &modes,
                host(neighbour_binding.clone(), Limits::compatibility()),
                &mode(b'k', b""),
            )
        });
        while entered.calls() == 0 {
            std::thread::sleep(Duration::from_millis(2));
        }
        let start = Instant::now();
        let looping = execute(
            &modes,
            host(
                Arc::new(Recording::default()),
                limits(100, 50, CEILING, CEILING),
            ),
            &mode(b'l', b""),
        );
        let elapsed = start.elapsed();
        assert_eq!(code(looping), "timeout");
        assert!(elapsed >= Duration::from_millis(100), "{elapsed:?}");
        assert!(
            elapsed <= Duration::from_millis(100) + TOLERANCE,
            "{elapsed:?}"
        );
        assert_eq!(neighbour.join().unwrap().unwrap(), b"done");
    });
}

type Control = (u8, &'static str, Option<&'static str>);
/// Mode byte, the code with no interference, and its guest message when it has one.
const OUTCOMES: [Control; 3] = [
    (b'k', "", None),
    (b'm', "handler_failed", Some("boom")),
    (b'n', "invalid_input", Some("bad input")),
];

fn controlled(
    params: u8,
    binding: Arc<Recording>,
    limits: Limits,
) -> Result<Vec<u8>, ExecuteError> {
    let engine = shared_engine();
    execute(
        &component(&engine, fixtures::MODES),
        host(binding, limits),
        &mode(params, b""),
    )
}

#[cfg_attr(test, test)]
pub fn cancellation_wins_on_success_and_on_every_handler_error() {
    for (params, plain, message) in OUTCOMES {
        // Control: without a cancellation each outcome keeps its own code.
        let result = controlled(
            params,
            Arc::new(Recording::default()),
            Limits::compatibility(),
        );
        match message {
            None => assert_eq!(result.unwrap(), b"done"),
            Some(message) => {
                let error = result.unwrap_err();
                assert_eq!(
                    (error.code, error.message.as_deref()),
                    (plain, Some(message))
                );
            }
        }
        // The cancellation lands during the guest's last (and only) host call.
        let engine = shared_engine();
        let binding = Arc::new(Recording::default());
        let host = host(binding.clone(), Limits::compatibility());
        let cancel = host.cancellation();
        binding.on_call(move || cancel.store(true, Ordering::Release));
        let result = execute(
            &component(&engine, fixtures::MODES),
            host,
            &mode(params, b""),
        );
        let error = result.expect_err("a cancelled invocation never succeeds");
        assert_eq!(error.code, "cancelled", "mode {}", params as char);
        assert_eq!(error.message, None);
    }
}

#[cfg_attr(test, test)]
pub fn the_deadline_wins_on_success_and_on_every_handler_error() {
    for (params, _, _) in OUTCOMES {
        // The host call returns successfully, but only after the guest deadline.
        let binding = Arc::new(Recording::default());
        binding.on_call(|| std::thread::sleep(Duration::from_millis(300)));
        let result = controlled(params, binding, limits(100, 1_000, CEILING, CEILING));
        let error = result.expect_err("an invocation past its deadline never succeeds");
        assert_eq!(error.code, "timeout", "mode {}", params as char);
        assert_eq!(error.message, None);
    }
}

#[cfg_attr(test, test)]
pub fn a_host_call_timeout_is_not_a_cancellation() {
    // The guest deadline (100 ms) falls while the host call (deadline 150 ms) is still
    // blocked for 300 ms: the host call times out, nobody cancels, and the invocation is
    // past its deadline, so every outcome reports timeout.
    for (params, _, _) in OUTCOMES {
        let binding = Arc::new(Recording::default());
        binding.on_call(|| std::thread::sleep(Duration::from_millis(300)));
        let result = controlled(params, binding, limits(100, 150, CEILING, CEILING));
        let error = result.expect_err("an invocation past its deadline never succeeds");
        assert_eq!(error.code, "timeout", "mode {}", params as char);
        assert_eq!(error.message, None);
    }
    // Control: with the guest deadline far away, the same host-call timeout is only an
    // error value the guest saw. It is not a cancellation, so the guest's own outcome
    // stands.
    for (params, plain, message) in OUTCOMES {
        let binding = Arc::new(Recording::default());
        binding.on_call(|| std::thread::sleep(Duration::from_millis(300)));
        let result = controlled(params, binding, limits(30_000, 50, CEILING, CEILING));
        match message {
            None => assert_eq!(result.unwrap(), b"done", "mode {}", params as char),
            Some(message) => {
                let error = result.unwrap_err();
                assert_eq!(
                    (error.code, error.message.as_deref()),
                    (plain, Some(message))
                );
            }
        }
    }
}

#[cfg_attr(test, test)]
pub fn oversized_results_and_error_payloads_are_refused_unread() {
    let engine = shared_engine();
    let modes = component(&engine, fixtures::MODES);
    let sized = |byte: u8, bytes: usize| mode(byte, &(bytes as u32).to_le_bytes());
    // The guest memory (2 MiB) holds a result and an error text well over the ceiling.
    let over = CEILING + CEILING / 2;
    assert_eq!(
        code(run(&modes, &sized(b's', over))),
        "buffer_limit_exceeded"
    );
    let error = run(&modes, &sized(b'G', over)).unwrap_err();
    assert_eq!((error.code, error.message), ("buffer_limit_exceeded", None));
    // An error text exactly at the ceiling is still the guest's handler error, cut to a
    // bounded echo, and one byte more is a limit failure.
    let error = run(&modes, &sized(b'G', CEILING)).unwrap_err();
    assert_eq!(error.code, "handler_failed");
    assert_eq!(error.message.unwrap().chars().count(), 256);
    let error = run(&modes, &sized(b'G', CEILING + 1)).unwrap_err();
    assert_eq!(error.code, "buffer_limit_exceeded");
}

#[cfg_attr(test, test)]
pub fn every_failure_maps_to_a_bounded_error() {
    let engine = shared_engine();
    let modes = component(&engine, fixtures::MODES);
    let failing = |params: &[u8]| run(&modes, params).unwrap_err();
    let error = failing(&mode(b'f', b""));
    assert_eq!(
        (error.code, error.message.as_deref()),
        ("handler_failed", Some("boom"))
    );
    let error = failing(&mode(b'i', b""));
    assert_eq!(
        (error.code, error.message.as_deref()),
        ("invalid_input", Some("bad input"))
    );
    // A trap carries no guest text at all.
    let error = failing(&mode(b't', b""));
    assert_eq!((error.code, error.message), ("guest_trapped", None));
    // A 1000-character guest message is cut to a bounded length.
    let error = failing(&mode(b'g', b""));
    assert_eq!(error.code, "handler_failed");
    let message = error.message.unwrap();
    assert_eq!(message.chars().count(), 256);
    assert!(message.chars().all(|c| c == 'x'));
}

#[cfg_attr(test, test)]
pub fn handler_gate_rejects_before_any_host_call() {
    let engine = shared_engine();
    // The control: the same start function does reach the host when the handler is valid.
    let binding = Arc::new(Recording::default());
    let valid = component(&engine, fixtures::START_LOGS_WITH_HANDLE);
    execute(&valid, host(binding.clone(), Limits::compatibility()), b"").unwrap();
    assert_eq!(
        binding.calls(),
        1,
        "the start function must call logging.log"
    );
    for (wasm, expected) in [
        (
            fixtures::START_LOGS_WITHOUT_HANDLE,
            "invalid_handler_signature",
        ),
        (fixtures::WRONG_RESULT, "invalid_handler_signature"),
        (b"\0asm\x0d\0\x01\0".as_slice(), "missing_handler_export"),
    ] {
        let binding = Arc::new(Recording::default());
        let result = execute(
            &component(&engine, wasm),
            host(binding.clone(), Limits::compatibility()),
            b"",
        );
        assert_eq!(code(result), expected);
        assert_eq!(binding.calls(), 0, "no guest code may run before the gate");
    }
    // A flag that never completes is irrelevant: rejection does not wait on a deadline.
    let _ = AtomicBool::new(false);
}
