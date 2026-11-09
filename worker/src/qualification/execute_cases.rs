//! Shared public execution cases, run in-process and by the restricted cluster runner.
use falcone_serverless_worker::host::{self, Bindings, Context, Host, HostError, Operation, Reply};
use std::sync::Arc;
use wasmtime::{Config, Engine};

const ECHO: &[u8] = include_bytes!("execute_fixtures/echo.wasm");
struct Deny;
impl Bindings for Deny {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Err(HostError::Denied)
    }
}
fn engine() -> Engine {
    let mut config = Config::new();
    config.epoch_interruption(true);
    Engine::new(&config).unwrap()
}
fn host() -> Host {
    Host::new(Context::default(), Arc::new(Deny))
}

#[cfg_attr(test, test)]
pub fn successful_handler_echoes_the_params() {
    let engine = engine();
    let params = br#"{"hello":"world"}"#;
    assert_eq!(
        host::execute(&engine, ECHO, params, host()).unwrap(),
        params
    );
}

use falcone_serverless_worker::limits::Limits;
use std::sync::atomic::{AtomicUsize, Ordering};
const ACTIONS: &[u8] = include_bytes!("execute_fixtures/actions.wasm");
fn limits(limit: usize, deadline: u64) -> Limits {
    Limits::parse(&format!(r#"{{"version":1,"compiler_cpu_seconds":120,"compiler_memory_bytes":4294967296,"guest_memory_bytes":268435456,"guest_to_host_bytes":{limit},"host_to_guest_bytes":{limit},"guest_deadline_ms":{deadline},"host_call_deadline_ms":30000,"termination_budget_ms":500}}"#)).unwrap()
}
#[derive(Default)]
struct Count(AtomicUsize);
impl Bindings for Count {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Reply::Empty)
    }
}
fn command(len: u32, outcome: u8, action: u8) -> Vec<u8> {
    let mut params = len.to_le_bytes().to_vec();
    params.extend_from_slice(&[outcome, action]);
    params
}

#[cfg_attr(test, test)]
pub fn params_are_bounded_before_guest_calls_at_small_and_default_limits() {
    let _case = crate::capability_support::serial_case();
    let engine = engine();
    for limit in [128, host::BUFFER_BYTES] {
        let bindings = Arc::new(Count::default());
        let host = Host::with_limits(Context::default(), bindings.clone(), limits(limit, 30000));
        let mut params = command(0, 0, 1);
        params.resize(limit + 1, 0);
        assert_eq!(
            host::execute(&engine, ACTIONS, &params, host)
                .unwrap_err()
                .code,
            "buffer_limit_exceeded"
        );
        assert_eq!(bindings.0.load(Ordering::SeqCst), 0);
        let params = vec![42; limit];
        let host = Host::with_limits(Context::default(), Arc::new(Deny), limits(limit, 30000));
        assert_eq!(host::execute(&engine, ECHO, &params, host).unwrap(), params);
    }
}

#[cfg_attr(test, test)]
pub fn version_two_host_capability_is_callable() {
    let _case = crate::capability_support::serial_case();
    let bindings = Arc::new(Count::default());
    let host = Host::new(Context::default(), bindings.clone());
    assert_eq!(
        host::execute(&engine(), ACTIONS, &command(3, 0, 1), host).unwrap(),
        b"aaa"
    );
    assert_eq!(bindings.0.load(Ordering::SeqCst), 1);
}

#[cfg_attr(test, test)]
pub fn utf8_result_and_error_payloads_obey_configured_limits() {
    let _case = crate::capability_support::serial_case();
    let engine = engine();
    for limit in [128, host::BUFFER_BYTES] {
        for kind in [0, 1, 2] {
            for len in [limit, limit + 1] {
                let host =
                    Host::with_limits(Context::default(), Arc::new(Deny), limits(limit, 30000));
                let result = host::execute(&engine, ACTIONS, &command(len as u32, kind, 0), host);
                if len > limit {
                    assert_eq!(result.unwrap_err().code, "buffer_limit_exceeded");
                } else if kind == 0 {
                    assert_eq!(result.unwrap(), vec![b'a'; len]);
                } else {
                    let error = result.unwrap_err();
                    assert_eq!(
                        error.code,
                        if kind == 1 {
                            "invalid_input"
                        } else {
                            "handler_failed"
                        }
                    );
                    assert_eq!(error.message.unwrap(), "a".repeat(len));
                }
            }
        }
    }
}

#[cfg_attr(test, test)]
pub fn execute_allocation_probe() {
    let Ok(probe) = std::env::var("FS_EXECUTE_ALLOCATION") else {
        return;
    };
    let parts: Vec<usize> = probe.split(':').map(|part| part.parse().unwrap()).collect();
    let [limit, len, kind] = parts[..] else {
        panic!("allocation probe config")
    };
    let engine = engine();
    let host = Host::with_limits(Context::default(), Arc::new(Deny), limits(limit, 30000));
    let mut prepared = host::Prepared::new(&engine, ACTIONS, host).unwrap();
    prepared.call(&command(0, 0, 0)).unwrap();
    let params = command(len as u32, kind as u8, 0);
    let before = crate::counting_alloc::reset_peak();
    let result = prepared.call(&params);
    let growth = crate::counting_alloc::peak().saturating_sub(before);
    assert!(
        growth <= 256 * 1024,
        "return lifting allocated {growth} bytes"
    );
    assert_eq!(result.unwrap_err().code, "buffer_limit_exceeded");
    println!("execute-allocation-probe passed: {probe} {growth}");
}

#[cfg_attr(test, test)]
pub fn oversized_results_and_errors_are_rejected_before_host_allocation() {
    let _case = crate::capability_support::serial_case();
    for limit in [128, host::BUFFER_BYTES] {
        for len in [limit + 1, if limit == 128 { 1_048_576 } else { 2_097_152 }] {
            for kind in [0, 1, 2] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["execute_allocation_probe", "--nocapture"])
                    .env("FS_EXECUTE_ALLOCATION", format!("{limit}:{len}:{kind}"))
                    .output()
                    .unwrap();
                assert!(
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout)
                            .contains("execute-allocation-probe passed:"),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}

use std::sync::{Mutex, atomic::AtomicBool};
use std::time::{Duration, Instant};
struct Completion {
    cancellation: Mutex<Option<Arc<AtomicBool>>>,
    delay: Duration,
}
impl Bindings for Completion {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        std::thread::sleep(self.delay);
        if let Some(cancel) = self.cancellation.lock().unwrap().as_ref() {
            cancel.store(true, Ordering::Release);
        }
        Ok(Reply::Empty)
    }
}
fn completion(kind: u8, cancel: bool, expire: bool) {
    let engine = engine();
    let bindings = Arc::new(Completion {
        cancellation: Mutex::new(None),
        delay: if expire {
            Duration::from_millis(100)
        } else {
            Duration::ZERO
        },
    });
    let host = Host::with_limits(
        Context::default(),
        bindings.clone(),
        limits(128, if expire { 50 } else { 30000 }),
    );
    if cancel && expire {
        let flag = host.cancellation();
        let mut prepared = host::Prepared::new(&engine, ACTIONS, host).unwrap();
        std::thread::sleep(Duration::from_millis(70));
        flag.store(true, Ordering::Release);
        assert_eq!(
            prepared.call(&command(3, kind, 1)).unwrap_err().code,
            "cancelled"
        );
        return;
    }
    if cancel {
        *bindings.cancellation.lock().unwrap() = Some(host.cancellation());
    }
    let result = host::execute(&engine, ACTIONS, &command(3, kind, 1), host);
    assert_eq!(
        result.unwrap_err().code,
        if cancel { "cancelled" } else { "guest_timeout" }
    );
}
#[cfg_attr(test, test)]
pub fn cancellation_precedes_success() {
    let _case = crate::capability_support::serial_case();
    completion(0, true, false);
    completion(0, true, true);
}
#[cfg_attr(test, test)]
pub fn cancellation_precedes_handler_error() {
    let _case = crate::capability_support::serial_case();
    completion(2, true, false);
    completion(2, true, true);
}
#[cfg_attr(test, test)]
pub fn deadline_precedes_success() {
    let _case = crate::capability_support::serial_case();
    completion(0, false, true);
}
#[cfg_attr(test, test)]
pub fn deadline_precedes_handler_error() {
    let _case = crate::capability_support::serial_case();
    completion(2, false, true);
}

fn rejection(bytes: &[u8], code: &str) {
    let bindings = Arc::new(Count::default());
    let host = Host::new(Context::default(), bindings.clone());
    assert_eq!(
        host::execute(&engine(), bytes, b"", host).unwrap_err().code,
        code
    );
    assert_eq!(bindings.0.load(Ordering::SeqCst), 0);
}
#[cfg_attr(test, test)]
pub fn handler_rejections_precede_guest_start_and_host_calls() {
    let _case = crate::capability_support::serial_case();
    for bytes in [
        include_bytes!("execute_fixtures/start-empty.wasm").as_slice(),
        include_bytes!("execute_fixtures/start-signature.wasm").as_slice(),
    ] {
        rejection(bytes, "invalid_handler_signature");
    }
}
#[cfg_attr(test, test)]
pub fn utf16_rejection_precedes_guest_start_and_host_calls() {
    let _case = crate::capability_support::serial_case();
    for bytes in [
        include_bytes!("execute_fixtures/utf16.wasm").as_slice(),
        include_bytes!("execute_fixtures/host-utf16.wasm").as_slice(),
    ] {
        rejection(bytes, "unsupported_string_encoding");
    }
}
#[cfg_attr(test, test)]
pub fn latin1_rejection_precedes_guest_start_and_host_calls() {
    let _case = crate::capability_support::serial_case();
    for bytes in [
        include_bytes!("execute_fixtures/latin1.wasm").as_slice(),
        include_bytes!("execute_fixtures/host-latin1.wasm").as_slice(),
    ] {
        rejection(bytes, "unsupported_string_encoding");
    }
}

#[cfg_attr(test, test)]
pub fn guest_deadline_interrupts_only_its_store_on_a_shared_engine() {
    let _case = crate::capability_support::serial_case();
    let mut config = Config::new();
    config.epoch_interruption(true).consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let mut slow = host::Prepared::new(
        &engine,
        ACTIONS,
        Host::with_limits(Context::default(), Arc::new(Deny), limits(128, 150)),
    )
    .unwrap();
    let healthy = host::Prepared::new(
        &engine,
        ECHO,
        Host::with_limits(Context::default(), Arc::new(Deny), limits(128, 2000)),
    )
    .unwrap();
    let running = Arc::new(AtomicBool::new(true));
    std::thread::scope(|scope| {
        let running_other = running.clone();
        let other = scope.spawn(move || {
            let mut healthy = healthy;
            let mut count = 0;
            while running_other.load(Ordering::Acquire) {
                assert_eq!(healthy.call(b"unaffected").unwrap(), b"unaffected");
                count += 1;
                std::thread::yield_now();
            }
            assert_eq!(healthy.call(b"still healthy").unwrap(), b"still healthy");
            count
        });
        let start = Instant::now();
        let result = slow.call(&command(0, 0, 2));
        running.store(false, Ordering::Release);
        let count = other.join().unwrap();
        assert_eq!(result.unwrap_err().code, "guest_timeout");
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(count > 0, "the other store must be running concurrently");
    });
}

#[cfg_attr(test, test)]
pub fn guest_deadline_also_bounds_a_blocked_host_call() {
    let _case = crate::capability_support::serial_case();
    let engine = engine();
    let binding = Arc::new(Completion {
        cancellation: Mutex::new(None),
        delay: Duration::from_millis(400),
    });
    let host = Host::with_limits(Context::default(), binding, limits(128, 50));
    let mut prepared = host::Prepared::new(&engine, ACTIONS, host).unwrap();
    let start = Instant::now();
    assert_eq!(
        prepared.call(&command(0, 0, 1)).unwrap_err().code,
        "guest_timeout"
    );
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "host call outlived guest deadline: {:?}",
        start.elapsed()
    );
}
