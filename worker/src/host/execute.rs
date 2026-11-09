//! Validated UTF-8 handler execution. No tenant native cache input or raw ABI lifting.
use super::{
    Host, HostError,
    outcome::{self, InvocationOutcome},
    runtime,
};
use crate::artifact::validate::{HANDLER_EXPORT, validate};
use std::time::Instant;
use wasmtime::component::{Component, ComponentType, Lift, TypedFunc};
use wasmtime::{CallHook, Engine, Store};

#[derive(ComponentType, Lift)]
#[component(variant)]
enum HandlerError {
    #[component(name = "invalid-input")]
    InvalidInput(String),
    #[component(name = "failed")]
    Failed(String),
}
type Handler = TypedFunc<(&'static [u8],), (Result<Vec<u8>, HandlerError>,)>;

#[derive(Debug, PartialEq, Eq)]
pub struct ExecuteError {
    pub code: &'static str,
    pub message: Option<String>,
}
#[derive(Debug)]
pub struct Execution {
    pub outcome: InvocationOutcome,
    pub result: Result<Vec<u8>, ExecuteError>,
}
fn error(code: &'static str) -> ExecuteError {
    ExecuteError {
        code,
        message: None,
    }
}

/// One invocation, retaining the host through instantiation and completion.
pub struct Prepared {
    store: Store<Host>,
    handler: Handler,
    deadline: Instant,
}
impl Prepared {
    pub fn new(engine: &Engine, bytes: &[u8], host: Host) -> Result<Self, Execution> {
        let deadline = Instant::now() + host.guest_duration();
        let mut store = Store::new(engine, host);
        let result = (|| {
            validate(bytes, 64 * 1024 * 1024).map_err(|e| error(e.code))?;
            if !engine.get_epoch_interruption() {
                return Err(error("epoch_interruption_required"));
            }
            let component = Component::new(engine, bytes).map_err(|_| error("compile_failed"))?;
            let instance = runtime::instantiate_in_store(engine, &component, &mut store, deadline)
                .map_err(|_| error("instantiation_failed"))?;
            let (_, handler) = instance
                .get_export(&mut store, None, HANDLER_EXPORT)
                .ok_or_else(|| error("missing_handler_export"))?;
            let (_, handle) = instance
                .get_export(&mut store, Some(&handler), "handle")
                .ok_or_else(|| error("invalid_handler_signature"))?;
            instance
                .get_typed_func(&mut store, handle)
                .map_err(|_| error("invalid_handler_signature"))
        })();
        match result {
            Ok(handler) => {
                let import_fuel = store.hostcall_fuel();
                store.call_hook(move |mut store, hook| {
                    // In 49.0.5 ReturningFromWasm runs before the component
                    // result's LiftContext is created. Bound that eager copy,
                    // including errors, before automatic post-return cleanup.
                    // Restore the runtime's lazy-cursor allowance whenever
                    // entering guest code (including realloc and post-return)
                    // or a host import, preserving the imports' WIT errors.
                    let fuel = match hook {
                        CallHook::ReturningFromWasm => store.data().input_limit(),
                        _ => import_fuel,
                    };
                    store.set_hostcall_fuel(fuel);
                    Ok(())
                });
                Ok(Self {
                    store,
                    handler,
                    deadline,
                })
            }
            Err(e) => Err(finish(store.data(), deadline, Err(e), None)),
        }
    }
    pub fn call(mut self, params: &[u8]) -> Execution {
        if params.len() > self.store.data().output_limit() {
            self.store
                .data()
                .record(&Err::<(), _>(HostError::LimitExceeded));
            return finish(
                self.store.data(),
                self.deadline,
                Err(error("buffer_limit_exceeded")),
                None,
            );
        }
        match self.handler.call(&mut self.store, (params,)) {
            Ok((Ok(bytes),)) => finish(self.store.data(), self.deadline, Ok(bytes), None),
            Ok((Err(handler_error),)) => {
                let (code, message) = match handler_error {
                    HandlerError::InvalidInput(message) => ("invalid_input", message),
                    HandlerError::Failed(message) => ("handler_failed", message),
                };
                finish(
                    self.store.data(),
                    self.deadline,
                    Err(ExecuteError {
                        code,
                        message: Some(message),
                    }),
                    None,
                )
            }
            Err(trap) => {
                // 49.0.5 keeps HostcallFuelExhausted private. Match its exact,
                // pinned engine diagnostic; never classify arbitrary guest text.
                if trap.root_cause().to_string()
                    == "too much data is being copied between the host and the guest: fuel allocated for hostcalls has been exhausted"
                {
                    self.store
                        .data()
                        .record(&Err::<(), _>(HostError::LimitExceeded));
                }
                finish(
                    self.store.data(),
                    self.deadline,
                    Err(error("handler_trapped")),
                    Some(&trap),
                )
            }
        }
    }
}
fn finish(
    host: &Host,
    deadline: Instant,
    result: Result<Vec<u8>, ExecuteError>,
    trap: Option<&wasmtime::Error>,
) -> Execution {
    let failed = wasmtime::Error::msg("handler_failed");
    let cancelled = host
        .cancellation()
        .load(std::sync::atomic::Ordering::Acquire);
    if cancelled {
        host.record(&Err::<(), _>(HostError::Cancelled));
    }
    let expired = wasmtime::Error::new(wasmtime::Trap::Interrupt);
    let outcome = outcome::classify(
        host,
        if cancelled {
            Err(&failed)
        } else if Instant::now() >= deadline {
            Err(&expired)
        } else if result.is_ok() {
            Ok(())
        } else {
            Err(trap.unwrap_or(&failed))
        },
    );
    let result = match outcome {
        InvocationOutcome::Success | InvocationOutcome::Failed => result,
        InvocationOutcome::Uncertain => Err(error("uncertain")),
        InvocationOutcome::GuestInterrupted => Err(error("deadline_exceeded")),
        InvocationOutcome::GuestMemoryLimit => Err(error("guest_memory_limit_exceeded")),
        InvocationOutcome::BufferLimit => Err(error("buffer_limit_exceeded")),
        InvocationOutcome::Cancelled => Err(error("cancelled")),
    };
    Execution { outcome, result }
}
pub fn execute(engine: &Engine, bytes: &[u8], host: Host, params: &[u8]) -> Execution {
    if params.len() > host.output_limit() {
        host.record(&Err::<(), _>(HostError::LimitExceeded));
        return finish(
            &host,
            Instant::now() + host.guest_duration(),
            Err(error("buffer_limit_exceeded")),
            None,
        );
    }
    match Prepared::new(engine, bytes, host) {
        Ok(prepared) => prepared.call(params),
        Err(result) => result,
    }
}
