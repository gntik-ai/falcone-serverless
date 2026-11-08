//! The guest execute path: call the exported `falcone:host/handler@0.2.0` `handle` of a
//! trusted, already compiled component, under the host's limits.
//!
//! The component arrives as a `Component`, which only trusted compilation (the supervised
//! compiler and the authenticated cache) mints for tenant input; no tenant-supplied
//! precompiled artifact is ever loaded here. Every failure is a typed code and an optional bounded message, and a
//! failure never carries partial output.
use super::Host;
use super::runtime::{BUFFER_BYTES, Bytes, instantiate_component};
use std::time::Instant;
use wasmtime::Engine;
use wasmtime::component::types::{ComponentFunc, ComponentItem, Type};
use wasmtime::component::{Component, ComponentType, Lift};

pub const HANDLER_EXPORT: &str = "falcone:host/handler@0.2.0";

/// Guest-supplied text is echoed only up to this many characters.
pub const MESSAGE_CHARS: usize = 256;

/// `variant handler-error { invalid-input(string), failed(string) }`.
#[derive(Debug, ComponentType, Lift)]
#[component(variant)]
enum HandlerError {
    #[component(name = "invalid-input")]
    InvalidInput(String),
    #[component(name = "failed")]
    Failed(String),
}

/// A failed invocation. `code` is stable; `message` is guest text, only for handler errors,
/// cut to `MESSAGE_CHARS` with control characters replaced. `uncertain_effect` is set
/// whenever a detached mutating host call may still commit, whatever `code` says.
#[derive(Debug, PartialEq, Eq)]
pub struct ExecuteError {
    pub code: &'static str,
    pub message: Option<String>,
    pub uncertain_effect: bool,
}

impl ExecuteError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            message: None,
            uncertain_effect: false,
        }
    }

    fn guest(code: &'static str, message: String) -> Self {
        let message = message
            .chars()
            .take(MESSAGE_CHARS)
            .map(|c| if c.is_control() { '?' } else { c })
            .collect();
        Self {
            code,
            message: Some(message),
            uncertain_effect: false,
        }
    }
}

/// Call `handle(params)` on `component`.
///
/// Order, none of it skippable:
/// 1. The handler export and its exact signature are checked on the component's static
///    type, before the linker instantiates it, so no start function can reach a host
///    capability when they are wrong.
/// 2. Params over the host-to-guest ceiling are refused before any guest code runs.
/// 3. The guest runs under the store's epoch deadline and cancellation flag.
/// 4. Every outcome, including handler errors, passes the same completion checks:
///    cancellation first, then the deadline, and only then the outcome is mapped
///    (uncertain effects, memory limit, buffer ceiling, the guest's own result).
///    The store's hostcall fuel is the transfer ceiling, so Wasmtime refuses to lift a
///    result or error text over it before copying a byte into host memory.
///
/// The engine must have epoch interruption enabled, which the trusted cache's does.
pub fn execute(component: &Component, host: Host, params: &[u8]) -> Result<Vec<u8>, ExecuteError> {
    let engine = component.engine();
    check_handler(component, engine)?;
    if !engine.get_epoch_interruption() {
        return Err(ExecuteError::new("engine_unsupported"));
    }
    if params.len() > host.output_limit() {
        return Err(ExecuteError::new("buffer_limit_exceeded"));
    }
    let (mut store, instance) = instantiate_component(engine, component, host)
        .map_err(|_| ExecuteError::new("instantiation_failed"))?;
    // Wasmtime charges every list and string it lifts against this fuel before it copies
    // them, so a guest cannot make the host allocate more than the transfer ceiling for
    // its result, an error text, or an argument of a host call.
    store.set_hostcall_fuel(BUFFER_BYTES);
    let outcome = instance.and_then(|instance| {
        let export = component
            .get_export_index(None, HANDLER_EXPORT)
            .and_then(|handler| component.get_export_index(Some(&handler), "handle"))
            .ok_or_else(|| wasmtime::Error::msg("missing handler export"))?;
        let handle = instance
            .get_typed_func::<(Vec<u8>,), (Result<Vec<u8>, HandlerError>,)>(&mut store, &export)?;
        handle.call(&mut store, (params.to_vec(),))
    });
    // The store, and with it the epoch ticker thread, is released on return.
    finish(store.data(), outcome)
}

fn finish(
    host: &Host,
    outcome: wasmtime::Result<(Result<Vec<u8>, HandlerError>,)>,
) -> Result<Vec<u8>, ExecuteError> {
    let uncertain = host.uncertain_host_call();
    let fail = |code: &'static str| ExecuteError {
        uncertain_effect: uncertain,
        ..ExecuteError::new(code)
    };
    // Cancellation first: a guest cancelled during its last host call is cancelled,
    // whether it then returned a value, a handler error, or trapped on the interrupt.
    // Only a requested cancellation counts: a host call that hit its own deadline also
    // marks the call as unacknowledged, but nobody asked for the invocation to stop.
    if host.cancel_requested() {
        return Err(fail("cancelled"));
    }
    // Then the deadline, so an expired invocation never reports an application error.
    let interrupted = outcome.as_ref().err().is_some_and(|error| {
        matches!(
            error.downcast_ref::<wasmtime::Trap>(),
            Some(wasmtime::Trap::Interrupt)
        )
    });
    if interrupted
        || host
            .guest_deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(fail("timeout"));
    }
    // Only now is the outcome mapped. A detached mutating call may still commit, so
    // that fact is reported ahead of whatever the guest did next.
    if uncertain {
        return Err(fail("uncertain_effect"));
    }
    if host.memory_limited() {
        return Err(fail("guest_memory_limit"));
    }
    if host.buffer_limited() {
        return Err(fail("buffer_limit_exceeded"));
    }
    match outcome {
        Err(error) if over_transfer_ceiling(&error) => Err(fail("buffer_limit_exceeded")),
        Err(_) => Err(fail("guest_trapped")),
        Ok((Ok(result),)) => match Bytes::new(host.input_limit()).add(result.len()) {
            Ok(()) => Ok(result),
            Err(_) => Err(fail("buffer_limit_exceeded")),
        },
        Ok((Err(error),)) => {
            let (code, message) = match error {
                HandlerError::InvalidInput(message) => ("invalid_input", message),
                HandlerError::Failed(message) => ("handler_failed", message),
            };
            match Bytes::new(host.input_limit()).add(message.len()) {
                Ok(()) => Err(ExecuteError::guest(code, message)),
                Err(_) => Err(fail("buffer_limit_exceeded")),
            }
        }
    }
}

/// Wasmtime's own refusal to lift guest data over the store's hostcall fuel. It is a
/// plain error, not a trap, and is only recognisable by its message; a test pins it.
fn over_transfer_ceiling(error: &wasmtime::Error) -> bool {
    format!("{error:#}").contains("fuel allocated for hostcalls has been exhausted")
}

/// The static handler check on the compiled component's type. It mirrors the intake
/// gate in `artifact::validate`, which runs on the raw bytes before compilation; this one
/// keeps the execute entry point safe for any component it is handed.
fn check_handler(component: &Component, engine: &Engine) -> Result<(), ExecuteError> {
    let missing = || ExecuteError::new("missing_handler_export");
    let invalid = || ExecuteError::new("invalid_handler_signature");
    let ty = component.component_type();
    let mut exports = ty.exports(engine);
    let (1, Some((HANDLER_EXPORT, export))) = (exports.len(), exports.next()) else {
        return Err(missing());
    };
    let ComponentItem::ComponentInstance(instance) = export.ty else {
        return Err(missing());
    };
    let mut handle = None;
    for (name, item) in instance.exports(engine) {
        match (name, item.ty) {
            ("handle", ComponentItem::ComponentFunc(func)) => handle = Some(func),
            ("handler-error", ComponentItem::Type(_)) => {}
            _ => return Err(invalid()),
        }
    }
    if handle.is_some_and(|func| handle_signature(&func)) {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn list_of_u8(ty: &Type) -> bool {
    matches!(ty, Type::List(list) if matches!(list.ty(), Type::U8))
}

fn handle_signature(func: &ComponentFunc) -> bool {
    let params: Vec<_> = func.params().collect();
    let results: Vec<_> = func.results().collect();
    let ([("params", param)], [Type::Result(reply)]) = (&*params, &*results) else {
        return false;
    };
    let (Some(ok), Some(Type::Variant(error))) = (reply.ok(), reply.err()) else {
        return false;
    };
    let cases: Vec<_> = error.cases().collect();
    list_of_u8(param)
        && list_of_u8(&ok)
        && matches!(
            &*cases,
            [first, second]
                if first.name == "invalid-input"
                    && second.name == "failed"
                    && matches!(first.ty, Some(Type::String))
                    && matches!(second.ty, Some(Type::String))
        )
}
