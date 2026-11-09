//! Shared in-process and restricted-pod handler qualification cases.
use crate::host::{self, Bindings, Context, Host, HostError, Operation, Reply};
use std::sync::Arc;

struct Empty;
impl Bindings for Empty {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Err(HostError::Denied)
    }
}

pub(super) fn engine() -> wasmtime::Engine {
    let mut config = wasmtime::Config::new();
    config.epoch_interruption(true);
    wasmtime::Engine::new(&config).unwrap()
}
fn fixture() -> Vec<u8> {
    // Wasmtime accepts WAT, but the artifact gate deliberately accepts only binary.
    // The binary is generated from handler.wat and checked by the integration test.
    include_bytes!("execute_fixtures/handler.wasm").to_vec()
}

#[cfg_attr(test, test)]
pub fn echo() {
    let execution = host::execute(
        &engine(),
        &fixture(),
        Host::new(Context::default(), Arc::new(Empty)),
        b"{\"hello\":true}",
    );
    assert_eq!(execution.result.unwrap(), b"{\"hello\":true}");
    assert_eq!(execution.outcome, host::outcome::InvocationOutcome::Success);
}

pub(super) fn limits(limit: usize, deadline: u64) -> crate::limits::Limits {
    let mut json = serde_json::json!({"version":1});
    for field in crate::limits::Field::ALL {
        json[field.name()] = crate::limits::Limits::compatibility().value(field).into();
    }
    json["guest_to_host_bytes"] = limit.into();
    json["host_to_guest_bytes"] = limit.into();
    json["guest_deadline_ms"] = deadline.into();
    crate::limits::Limits::parse(&json.to_string()).unwrap()
}
pub(super) fn context() -> Context {
    Context {
        grants: host::Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    }
}
/// Modes of the adversarial WAT fixture's parameter envelope. Values are ABI
/// literals shared with its independently checked binary, not capability IDs.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Mode {
    Result = 0,
    InvalidInput = 1,
    Failed = 2,
    Trap = 4,
    MutationSuccess = 5,
    MutationError = 6,
    MutationTrap = 7,
    MutationLoop = 8,
    LogSuccess = 9,
    LogError = 10,
    LogLoop = 11,
    LogFields = 12,
    OversizedPublish = 13,
}
pub(super) fn params(mode: Mode, size: usize) -> [u8; 8] {
    let mut params = [0; 8];
    params[0] = mode as u8;
    params[4..].copy_from_slice(&(size as u32).to_le_bytes());
    params
}
pub(super) fn adversarial() -> &'static [u8] {
    include_bytes!("execute_fixtures/adversarial.wasm")
}

#[cfg_attr(test, test)]
pub fn configured_output_limit() {
    let engine = engine();
    for mode in [Mode::Result, Mode::InvalidInput, Mode::Failed] {
        for limit in [128, host::BUFFER_BYTES] {
            let host = || Host::with_limits(context(), Arc::new(Empty), limits(limit, 30000));
            let at = host::execute(&engine, adversarial(), host(), &params(mode, limit));
            if mode == Mode::Result {
                assert_eq!(at.result.unwrap(), vec![b'x'; limit]);
            } else {
                let error = at.result.unwrap_err();
                assert_eq!(
                    error.code,
                    if mode == Mode::InvalidInput {
                        "invalid_input"
                    } else {
                        "handler_failed"
                    }
                );
                assert_eq!(error.message.unwrap().len(), limit);
            }
            let over = host::execute(&engine, adversarial(), host(), &params(mode, limit + 1));
            let error = over.result.unwrap_err();
            assert_eq!(error.code, "buffer_limit_exceeded");
            assert_eq!(error.message, None);
        }
    }
}

#[cfg_attr(test, test)]
pub fn oversized_params_never_call_guest() {
    // The trap would win if handle were entered.
    let execution = host::execute(
        &engine(),
        adversarial(),
        Host::with_limits(context(), Arc::new(Empty), limits(128, 30000)),
        &[Mode::Trap as u8; 129],
    );
    assert_eq!(execution.result.unwrap_err().code, "buffer_limit_exceeded");
    let at = host::execute(
        &engine(),
        &fixture(),
        Host::with_limits(Context::default(), Arc::new(Empty), limits(128, 30000)),
        &[b'x'; 128],
    );
    assert_eq!(at.result.unwrap(), [b'x'; 128]);
}

struct Cancel(std::sync::Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>);
impl Bindings for Cancel {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(Reply::Empty)
    }
}
fn cancelled_completion(mode: Mode) {
    let binding = Arc::new(Cancel(std::sync::Mutex::new(None)));
    let host = Host::new(context(), binding.clone());
    *binding.0.lock().unwrap() = Some(host.cancellation());
    let result = host::execute(&engine(), adversarial(), host, &params(mode, 0));
    assert_eq!(result.result.unwrap_err().code, "cancelled");
    assert_eq!(result.outcome, host::outcome::InvocationOutcome::Cancelled);
}
#[cfg_attr(test, test)]
pub fn cancellation_on_success() {
    cancelled_completion(Mode::LogSuccess);
}
#[cfg_attr(test, test)]
pub fn cancellation_on_error() {
    cancelled_completion(Mode::LogError);
}

struct Delay;
impl Bindings for Delay {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        std::thread::sleep(std::time::Duration::from_millis(250));
        Ok(Reply::Empty)
    }
}
fn deadline_completion(mode: Mode) {
    let execution = host::execute(
        &engine(),
        adversarial(),
        Host::with_limits(context(), Arc::new(Delay), limits(128, 200)),
        &params(mode, 0),
    );
    assert_eq!(execution.result.unwrap_err().code, "deadline_exceeded");
}
#[cfg_attr(test, test)]
pub fn deadline_on_success() {
    deadline_completion(Mode::LogSuccess);
}
#[cfg_attr(test, test)]
pub fn deadline_on_error() {
    deadline_completion(Mode::LogError);
}

#[cfg_attr(test, test)]
pub fn expired_prepared_invocation() {
    let prepared = host::Prepared::new(
        &engine(),
        &fixture(),
        Host::with_limits(Context::default(), Arc::new(Empty), limits(128, 200)),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert_eq!(
        prepared.call(&[0; 129]).result.unwrap_err().code,
        "deadline_exceeded"
    );
}

struct Count(std::sync::atomic::AtomicUsize);
impl Bindings for Count {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Reply::Empty)
    }
}
fn rejected_before_start(bytes: &[u8], code: &str) {
    let binding = Arc::new(Count(std::sync::atomic::AtomicUsize::new(0)));
    let result = host::execute(
        &engine(),
        bytes,
        Host::new(context(), binding.clone()),
        &params(Mode::Result, 0),
    );
    assert_eq!(result.result.unwrap_err().code, code);
    assert_eq!(binding.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}
#[cfg_attr(test, test)]
pub fn invalid_export_before_host_call() {
    rejected_before_start(
        include_bytes!("execute_fixtures/invalid-start.wasm"),
        "invalid_handler_signature",
    );
}
#[cfg_attr(test, test)]
pub fn wrong_signature_before_host_call() {
    rejected_before_start(
        include_bytes!("execute_fixtures/wrong-signature.wasm"),
        "invalid_handler_signature",
    );
}
#[cfg_attr(test, test)]
pub fn utf16_before_host_call() {
    rejected_before_start(
        include_bytes!("execute_fixtures/utf16-start.wasm"),
        "unsupported_string_encoding",
    );
}
#[cfg_attr(test, test)]
pub fn latin1_before_host_call() {
    rejected_before_start(
        include_bytes!("execute_fixtures/latin1-start.wasm"),
        "unsupported_string_encoding",
    );
}
#[cfg_attr(test, test)]
pub fn utf16_lower_before_host_call() {
    rejected_before_start(
        include_bytes!("execute_fixtures/utf16-lower.wasm"),
        "unsupported_string_encoding",
    );
}
#[cfg_attr(test, test)]
pub fn forbidden_imports() {
    rejected_before_start(
        include_bytes!("execute_fixtures/wasi-import.wasm"),
        "undeclared_import",
    );
    let mut old = adversarial().to_vec();
    for i in 0..old.len().saturating_sub(5) {
        if old[i..].starts_with(b"@0.2.0") {
            old[i + 3] = b'1';
        }
    }
    rejected_before_start(&old, "undeclared_import");
}
#[cfg_attr(test, test)]
pub fn missing_export() {
    rejected_before_start(b"\0asm\x0d\0\x01\0", "missing_handler_export");
}
#[cfg_attr(test, test)]
pub fn different_export() {
    let mut bytes = fixture();
    // Preserve the otherwise valid handler's types and binary section lengths.
    let name = b"falcone:host/handler@0.2.0";
    let offset = bytes.windows(name.len()).position(|s| s == name).unwrap();
    bytes[offset..offset + name.len()].copy_from_slice(b"falcone:host/another@0.2.0");
    rejected_before_start(&bytes, "missing_handler_export");
}

struct Timeout {
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl Bindings for Timeout {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        Err(HostError::Timeout)
    }
}
fn uncertain(mode: Mode, cancel: bool, size: usize) {
    let started = std::time::Instant::now();
    let mut host = Host::with_limits(
        context(),
        Arc::new(Empty),
        limits(
            128,
            if mode == Mode::MutationLoop && !cancel {
                200
            } else {
                30000
            },
        ),
    );
    let flag = host.cancellation();
    host.bindings = Arc::new(Timeout {
        cancel: cancel.then_some(flag),
    });
    let result = host::execute(&engine(), adversarial(), host, &params(mode, size));
    if cancel {
        // The 30 s deadline cannot explain this loop's completion. With the
        // former 200 ms budget, deadline interruption could mask broken cancel.
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }
    assert_eq!(result.outcome, host::outcome::InvocationOutcome::Uncertain);
    assert_eq!(result.result.unwrap_err().code, "uncertain");
}
#[cfg_attr(test, test)]
pub fn uncertainty_on_success() {
    uncertain(Mode::MutationSuccess, false, 0);
}
#[cfg_attr(test, test)]
pub fn uncertainty_on_error() {
    uncertain(Mode::MutationError, false, 0);
}
#[cfg_attr(test, test)]
pub fn uncertainty_on_trap() {
    uncertain(Mode::MutationTrap, false, 0);
}
#[cfg_attr(test, test)]
pub fn uncertainty_on_cancellation() {
    uncertain(Mode::MutationLoop, true, 0);
}
#[cfg_attr(test, test)]
pub fn uncertainty_on_size_limit() {
    uncertain(Mode::MutationSuccess, false, 129);
}
#[cfg_attr(test, test)]
pub fn non_mutating_timeout() {
    let result = host::execute(
        &engine(),
        adversarial(),
        Host::new(context(), Arc::new(Timeout { cancel: None })),
        &params(Mode::LogSuccess, 0),
    );
    assert_eq!(result.outcome, host::outcome::InvocationOutcome::Cancelled);
    assert_eq!(result.result.unwrap_err().code, "cancelled");
}

struct Ready(std::sync::mpsc::SyncSender<()>);
impl Bindings for Ready {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        self.0.send(()).unwrap();
        Ok(Reply::Empty)
    }
}
#[cfg_attr(test, test)]
pub fn timeout_isolation() {
    let engine = engine();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let blocked = host::Prepared::new(
        &engine,
        adversarial(),
        Host::with_limits(context(), Arc::new(Ready(send)), limits(128, 500)),
    )
    .unwrap();
    let other = host::Prepared::new(
        &engine,
        &fixture(),
        Host::new(Context::default(), Arc::new(Empty)),
    )
    .unwrap();
    let start = std::time::Instant::now();
    std::thread::scope(|scope| {
        let running = scope.spawn(|| blocked.call(&params(Mode::LogLoop, 0)));
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(other.call(b"isolated").result.unwrap(), b"isolated");
        assert_eq!(
            running.join().unwrap().result.unwrap_err().code,
            "deadline_exceeded"
        );
    });
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}

struct CancelDeadline(Arc<std::sync::atomic::AtomicBool>);
impl Bindings for CancelDeadline {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        std::thread::sleep(std::time::Duration::from_millis(250));
        self.0.store(true, std::sync::atomic::Ordering::Release);
        Ok(Reply::Empty)
    }
}
fn cancellation_before_deadline(mode: Mode) {
    let mut host = Host::with_limits(context(), Arc::new(Empty), limits(128, 200));
    host.bindings = Arc::new(CancelDeadline(host.cancellation()));
    let prepared = host::Prepared::new(&engine(), adversarial(), host).unwrap();
    let result = prepared.call(&params(mode, 0));
    assert_eq!(result.result.unwrap_err().code, "cancelled");
}
#[cfg_attr(test, test)]
pub fn cancel_before_deadline_success() {
    cancellation_before_deadline(Mode::LogSuccess);
}
#[cfg_attr(test, test)]
pub fn cancel_before_deadline_error() {
    cancellation_before_deadline(Mode::LogError);
}

#[cfg_attr(test, test)]
pub fn uncertainty_on_deadline() {
    uncertain(Mode::MutationLoop, false, 0);
}

#[cfg_attr(test, test)]
pub fn output_survives_post_return_cleanup() {
    let result = host::execute(
        &engine(),
        include_bytes!("execute_fixtures/post-return.wasm"),
        Host::new(Context::default(), Arc::new(Empty)),
        b"owned before cleanup",
    );
    assert_eq!(result.result.unwrap(), b"owned before cleanup");
    let result = host::execute(
        &engine(),
        include_bytes!("execute_fixtures/error-post-return.wasm"),
        Host::new(context(), Arc::new(Empty)),
        &params(Mode::Failed, 16),
    );
    let error = result.result.unwrap_err();
    assert_eq!(error.code, "handler_failed");
    assert_eq!(error.message.as_deref(), Some("xxxxxxxxxxxxxxxx"));
}

struct ImportLog(std::sync::Mutex<Vec<(String, usize)>>);
impl Bindings for ImportLog {
    fn call(&self, _: &Context, op: Operation) -> Result<Reply, HostError> {
        match op {
            Operation::Log {
                message, fields, ..
            } => {
                self.0.lock().unwrap().push((message, fields.len()));
                Ok(Reply::Empty)
            }
            _ => panic!("oversized import must not reach the binding"),
        }
    }
}

#[cfg_attr(test, test)]
pub fn in_limit_import_fields_keep_their_existing_budget() {
    let binding = std::sync::Arc::new(ImportLog(std::sync::Mutex::new(Vec::new())));
    let result = host::execute(
        &engine(),
        adversarial(),
        host::Host::with_limits(context(), binding.clone(), limits(128, 30000)),
        &params(Mode::LogFields, 1),
    );
    assert_eq!(result.result.unwrap(), b"x");
    assert_eq!(*binding.0.lock().unwrap(), vec![("o".into(), 2)]);
}

#[cfg_attr(test, test)]
pub fn oversized_import_returns_wit_error_and_guest_continues() {
    for limit in [128, host::BUFFER_BYTES] {
        let binding = std::sync::Arc::new(ImportLog(std::sync::Mutex::new(Vec::new())));
        let result = host::execute(
            &engine(),
            adversarial(),
            host::Host::with_limits(context(), binding.clone(), limits(limit, 30000)),
            &params(Mode::OversizedPublish, limit + 1),
        );
        assert_eq!(result.result.unwrap_err().code, "buffer_limit_exceeded");
        assert_eq!(*binding.0.lock().unwrap(), vec![("orders".into(), 0)]);
    }
}

#[cfg_attr(test, test)]
pub fn async_utf16_before_host_call() {
    rejected_before_start(
        include_bytes!("execute_fixtures/async-utf16.wasm"),
        "unsupported_string_encoding",
    );
}
#[cfg_attr(test, test)]
pub fn async_latin1_before_host_call() {
    rejected_before_start(
        include_bytes!("execute_fixtures/async-latin1.wasm"),
        "unsupported_string_encoding",
    );
}
