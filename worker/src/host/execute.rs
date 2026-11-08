//! The execute path: one `handle` call on a validated, compiled component.
//!
//! Params and result are bounded by the host-to-guest and guest-to-host ceilings. The
//! guest deadline and cancellation are enforced on the epoch tick, so they also stop a
//! guest that makes no host calls. Every outcome, success or error, goes through the one
//! completion check in [`complete`]: cancellation first, then the deadline. The caller
//! gets a bounded code or the whole result, never partial output and never guest text.

use super::{BUFFER_BYTES, Host, instantiate_component};
use crate::artifact::validate::{HANDLER_EXPORT, validate};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wasmtime::component::types::{ComponentItem, Type};
use wasmtime::component::{Component, ComponentType, Lift, WasmList, WasmStr};
use wasmtime::{Config, Engine, Trap, UpdateDeadline};

/// How often the engine epoch advances. Deadline and cancellation are noticed within a tick.
pub const TICK: Duration = Duration::from_millis(10);

/// Raw qualification sources above this size are refused before validation.
const SOURCE_BYTES: usize = 64 * 1024 * 1024;

pub const LIMIT_EXCEEDED: &str = "limit_exceeded";
pub const TIMEOUT: &str = "timeout";
pub const CANCELLED: &str = "cancelled";
pub const TRAP: &str = "trap";
pub const INVALID_INPUT: &str = "invalid_input";
pub const HANDLER_FAILED: &str = "handler_failed";
pub const MISSING_HANDLER_EXPORT: &str = "missing_handler_export";
pub const INSTANTIATE_FAILED: &str = "instantiate_failed";
pub const COMPILE_FAILED: &str = "compile_failed";
pub const INTERNAL: &str = "internal";

/// A stable, bounded failure code. It never carries guest-provided text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}

impl std::error::Error for Error {}

const fn error(code: &'static str) -> Error {
    Error { code }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub guest_deadline: Duration,
}

/// The engine configuration the execute path needs: fuel metering (the trusted compiler
/// builds with it) and epoch interruption for the deadline and cancellation.
pub fn engine() -> Result<Engine, Error> {
    let mut config = Config::new();
    config.consume_fuel(true).epoch_interruption(true);
    Engine::new(&config).map_err(|_| error(INTERNAL))
}

/// `variant handler-error { invalid-input(string), failed(string) }`. The text is never read.
#[derive(ComponentType, Lift)]
#[component(variant)]
enum HandlerError {
    #[component(name = "invalid-input")]
    InvalidInput(WasmStr),
    #[component(name = "failed")]
    Failed(WasmStr),
}

/// Why the epoch callback stopped a guest.
#[derive(Debug)]
struct Stopped(&'static str);

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Stopped {}

/// Cancellation and the deadline of one invocation.
#[derive(Clone)]
struct Watch {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl Watch {
    /// Cancellation first, then the deadline.
    fn reason(&self) -> Option<Error> {
        if self.cancelled.load(Ordering::Acquire) {
            Some(error(CANCELLED))
        } else if Instant::now() >= self.deadline {
            Some(error(TIMEOUT))
        } else {
            None
        }
    }
}

/// The single completion check for every outcome. A cancelled or expired invocation
/// reports that, whatever the guest returned, and its output is dropped.
fn complete(outcome: Result<Vec<u8>, Error>, watch: &Watch) -> Result<Vec<u8>, Error> {
    match watch.reason() {
        Some(stop) => Err(stop),
        None => outcome,
    }
}

struct Ticker {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Ticker {
    fn start(engine: Engine) -> Result<Self, Error> {
        let (stop, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("execute-epoch".into())
            .spawn(move || {
                while let Err(mpsc::RecvTimeoutError::Timeout) = receiver.recv_timeout(TICK) {
                    engine.increment_epoch();
                }
            })
            .map_err(|_| error(INTERNAL))?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs invocations on one engine, which any number of threads may share. Each call
/// has its own store and its own deadline; the one ticker advances the shared epoch.
pub struct Executor {
    engine: Engine,
    _ticker: Ticker,
}

impl Executor {
    /// `engine` must come from [`engine`] or the trusted compiler's cache, which use the
    /// same configuration; components passed to [`Executor::execute`] must belong to it.
    pub fn new(engine: &Engine) -> Result<Self, Error> {
        Ok(Self {
            engine: engine.clone(),
            _ticker: Ticker::start(engine.clone())?,
        })
    }

    /// Calls `handle` once. Returns the whole result, or one bounded code.
    pub fn execute(
        &self,
        component: &Component,
        mut host: Host,
        params: &[u8],
        limits: Limits,
    ) -> Result<Vec<u8>, Error> {
        let deadline = Instant::now()
            .checked_add(limits.guest_deadline)
            .ok_or(error(LIMIT_EXCEEDED))?;
        host.set_invocation_deadline(deadline);
        let watch = Watch {
            cancelled: host.cancellation(),
            deadline,
        };
        let outcome = self.invoke(component, host, params, &watch);
        complete(outcome, &watch)
    }

    /// Qualification only: validates the raw component first, then compiles it in
    /// process. A runtime adapter compiles through the supervised trusted compiler.
    /// Validation runs before anything is compiled or instantiated, so a start function
    /// of a rejected component never reaches the host.
    pub fn execute_source(
        &self,
        source: &[u8],
        host: Host,
        params: &[u8],
        limits: Limits,
    ) -> Result<Vec<u8>, Error> {
        let validated = validate(source, SOURCE_BYTES).map_err(|rejected| error(rejected.code))?;
        let component =
            Component::new(&self.engine, validated.bytes()).map_err(|_| error(COMPILE_FAILED))?;
        self.execute(&component, host, params, limits)
    }

    fn invoke(
        &self,
        component: &Component,
        host: Host,
        params: &[u8],
        watch: &Watch,
    ) -> Result<Vec<u8>, Error> {
        if let Some(stop) = watch.reason() {
            return Err(stop);
        }
        if params.len() > BUFFER_BYTES {
            return Err(error(LIMIT_EXCEEDED));
        }
        if !Engine::same(&self.engine, component.engine()) {
            return Err(error(INTERNAL));
        }
        // The type is known before instantiation: no start function runs for a component
        // that lacks the exact handler.
        if !handler_type_is_exact(&self.engine, component) {
            return Err(error(MISSING_HANDLER_EXPORT));
        }
        let epoch = watch.clone();
        let (mut store, instance) =
            instantiate_component(&self.engine, component, host, move |store| {
                store.set_fuel(u64::MAX)?;
                store.set_epoch_deadline(1);
                store.epoch_deadline_callback(move |_| match epoch.reason() {
                    Some(stop) => Err(wasmtime::Error::new(Stopped(stop.code))),
                    None => Ok(UpdateDeadline::Continue(1)),
                });
                Ok(())
            })
            .map_err(|failure| failure_code(&failure, INSTANTIATE_FAILED))?;
        let handle = instance
            .get_export_index(&mut store, None, HANDLER_EXPORT)
            .and_then(|handler| instance.get_export_index(&mut store, Some(&handler), "handle"))
            .ok_or(error(MISSING_HANDLER_EXPORT))?;
        let handle = instance
            .get_typed_func::<(Vec<u8>,), (Result<WasmList<u8>, HandlerError>,)>(
                &mut store, &handle,
            )
            .map_err(|_| error(MISSING_HANDLER_EXPORT))?;
        let (result,) = handle
            .call(&mut store, (params.to_vec(),))
            .map_err(|failure| failure_code(&failure, TRAP))?;
        match result {
            Ok(list) if list.len() > BUFFER_BYTES => Err(error(LIMIT_EXCEEDED)),
            Ok(list) => Ok(list.as_le_slice(&store).to_vec()),
            Err(HandlerError::InvalidInput(text)) => Err(without_text(INVALID_INPUT, text)),
            Err(HandlerError::Failed(text)) => Err(without_text(HANDLER_FAILED, text)),
        }
    }
}

/// The guest's message is dropped unread: only the code leaves the execute path.
fn without_text(code: &'static str, _text: WasmStr) -> Error {
    error(code)
}

fn failure_code(failure: &wasmtime::Error, otherwise: &'static str) -> Error {
    if let Some(Stopped(code)) = failure.downcast_ref::<Stopped>() {
        return error(code);
    }
    match failure.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => error(LIMIT_EXCEEDED),
        _ => error(otherwise),
    }
}

/// The component's only export is `falcone:host/handler@0.2.0`, an instance whose `handle`
/// is `func(params: list<u8>) -> result<list<u8>, variant { invalid-input(string),
/// failed(string) }>`. The same rule as the raw artifact validator, on the compiled type.
fn handler_type_is_exact(engine: &Engine, component: &Component) -> bool {
    let component = component.component_type();
    let mut exports = component.exports(engine);
    let (Some((name, export)), None) = (exports.next(), exports.next()) else {
        return false;
    };
    let ComponentItem::ComponentInstance(handler) = export.ty else {
        return false;
    };
    if name != HANDLER_EXPORT {
        return false;
    }
    let mut handle = false;
    for (name, item) in handler.exports(engine) {
        match (name, item.ty) {
            ("handle", ComponentItem::ComponentFunc(function)) => {
                let mut params = function.params();
                let mut results = function.results();
                handle = matches!(
                    (params.next(), params.next(), results.next(), results.next()),
                    (Some(("params", Type::List(bytes))), None, Some(Type::Result(outcome)), None)
                        if matches!(bytes.ty(), Type::U8)
                            && matches!(outcome.ok(), Some(Type::List(ok)) if matches!(ok.ty(), Type::U8))
                            && matches!(outcome.err(), Some(Type::Variant(error)) if variant_is_handler_error(&error))
                );
            }
            ("handler-error", ComponentItem::Type(Type::Variant(error))) => {
                if !variant_is_handler_error(&error) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    handle
}

fn variant_is_handler_error(variant: &wasmtime::component::types::Variant) -> bool {
    let mut cases = variant.cases();
    matches!(
        (cases.next(), cases.next(), cases.next()),
        (Some(first), Some(second), None)
            if first.name == "invalid-input"
                && matches!(first.ty, Some(Type::String))
                && second.name == "failed"
                && matches!(second.ty, Some(Type::String))
    )
}
