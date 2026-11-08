//! The guest execute path: call the exported `falcone:host/handler@0.2.0` `handle` of a
//! trusted, already compiled component, under the host's limits.
//!
//! The component arrives as a `Component`, which only trusted compilation (the supervised
//! compiler and the authenticated cache) mints for tenant input; no tenant-supplied
//! precompiled artifact is ever loaded here. Every failure is a typed code and an optional bounded message, and a
//! failure never carries partial output.
use super::Host;
use super::outcome::{InvocationOutcome, classify};
use super::runtime::instantiate_component;
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
/// cut to `MESSAGE_CHARS` with control characters replaced.
#[derive(Debug, PartialEq, Eq)]
pub struct ExecuteError {
    pub code: &'static str,
    pub message: Option<String>,
}

impl ExecuteError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            message: None,
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
///    uncertain effects, memory limit, cancellation, then the deadline, and only then
///    the buffer ceiling and the guest's own result.
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
    let base = classify(host, outcome.as_ref().map(|_| ()));
    match base {
        InvocationOutcome::Uncertain => return Err(ExecuteError::new("uncertain_effect")),
        InvocationOutcome::GuestMemoryLimit => {
            return Err(ExecuteError::new("guest_memory_limit"));
        }
        _ => {}
    }
    // Cancellation first: a guest cancelled during its last host call is cancelled,
    // whether it then returned a value, a handler error, or trapped on the interrupt.
    if host.cancelled_host_call() || host.cancel_requested() {
        return Err(ExecuteError::new("cancelled"));
    }
    // Then the deadline, so an expired invocation never reports an application error.
    if base == InvocationOutcome::GuestInterrupted
        || host
            .guest_deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(ExecuteError::new("timeout"));
    }
    if base == InvocationOutcome::BufferLimit {
        return Err(ExecuteError::new("buffer_limit_exceeded"));
    }
    match outcome {
        Err(_) => Err(ExecuteError::new("guest_trapped")),
        Ok((Ok(result),)) if result.len() <= host.input_limit() => Ok(result),
        Ok((Ok(_),)) => Err(ExecuteError::new("buffer_limit_exceeded")),
        Ok((Err(HandlerError::InvalidInput(message)),)) => {
            Err(ExecuteError::guest("invalid_input", message))
        }
        Ok((Err(HandlerError::Failed(message)),)) => {
            Err(ExecuteError::guest("handler_failed", message))
        }
    }
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
