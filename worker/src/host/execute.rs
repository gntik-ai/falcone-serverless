//! Validated UTF-8 handler execution. Compilation here is qualification-only;
//! the runtime adapter must use the supervised compiler and trusted cache.
use super::{Host, runtime};
use crate::artifact::validate::{HANDLER_EXPORT, validate};
use wasmtime::component::{Component, ComponentType, Lift, TypedFunc};
use wasmtime::{Engine, Store};

#[derive(Debug, PartialEq, Eq)]
pub struct ExecutionError {
    pub code: &'static str,
    pub message: Option<String>,
}
impl ExecutionError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            message: None,
        }
    }
}

#[derive(ComponentType, Lift)]
#[component(variant)]
enum HandlerError {
    #[component(name = "invalid-input")]
    InvalidInput(String),
    #[component(name = "failed")]
    Failed(String),
}

type Handle = TypedFunc<(Vec<u8>,), (Result<Vec<u8>, HandlerError>,)>;
pub struct Prepared {
    store: Store<Host>,
    handle: Handle,
}
impl Prepared {
    /// Validates before Component::new, linking or any guest start function.
    pub fn new(engine: &Engine, bytes: &[u8], host: Host) -> Result<Self, ExecutionError> {
        if let Some(code) = host.completion_error() {
            return Err(ExecutionError::new(code));
        }
        let validated =
            validate(bytes, 64 * 1024 * 1024).map_err(|error| ExecutionError::new(error.code))?;
        if !engine.get_epoch_interruption() {
            return Err(ExecutionError::new("guest_deadline_unavailable"));
        }
        let component = Component::new(engine, validated.bytes())
            .map_err(|_| ExecutionError::new("compile_failed"))?;
        let (mut store, instance) =
            runtime::instantiate_component(engine, component, host, Some(u64::MAX))
                .map_err(|_| ExecutionError::new("instantiation_failed"))?;
        let index = instance
            .get_export_index(&mut store, None, HANDLER_EXPORT)
            .and_then(|instance_index| {
                instance.get_export_index(&mut store, Some(&instance_index), "handle")
            })
            .ok_or_else(|| ExecutionError::new("invalid_handler_signature"))?;
        let handle = instance
            .get_typed_func::<(Vec<u8>,), (Result<Vec<u8>, HandlerError>,)>(&mut store, index)
            .map_err(|_| ExecutionError::new("invalid_handler_signature"))?;
        Ok(Self { store, handle })
    }

    fn completion(&self) -> Result<(), ExecutionError> {
        match self.store.data().completion_error() {
            Some(code) => Err(ExecutionError::new(code)),
            None => Ok(()),
        }
    }

    pub fn call(&mut self, params: &[u8]) -> Result<Vec<u8>, ExecutionError> {
        self.completion()?;
        if params.len() > self.store.data().output_limit() {
            return Err(ExecutionError::new("buffer_limit_exceeded"));
        }
        // Wasmtime 49 charges list/string bytes before allocating a lifted
        // Vec/String. Validation guarantees UTF-8, so encoded == decoded bytes.
        // This must be the configured guest-to-host limit, including for errors.
        self.store
            .set_hostcall_fuel(self.store.data().input_limit());
        let result = self.handle.call(&mut self.store, (params.to_vec(),));
        // Both success and error paths, including traps/post-return, must pass
        // the same completion gate. Never publish output before this point.
        self.completion()?;
        let (result,) = result.map_err(|error| call_error(&error))?;
        match result {
            Ok(bytes) => Ok(bytes),
            Err(error) => {
                let (code, message) = match error {
                    HandlerError::InvalidInput(message) => ("invalid_input", message),
                    HandlerError::Failed(message) => ("handler_failed", message),
                };
                Err(ExecutionError {
                    code,
                    message: Some(message),
                })
            }
        }
    }
}

/// A single qualification invocation; never accepts native/precompiled artifacts.
pub fn execute(
    engine: &Engine,
    bytes: &[u8],
    params: &[u8],
    host: Host,
) -> Result<Vec<u8>, ExecutionError> {
    if params.len() > host.output_limit() {
        return Err(ExecutionError::new("buffer_limit_exceeded"));
    }
    Prepared::new(engine, bytes, host)?.call(params)
}

// The pinned engine keeps HostcallFuelExhausted private. Match its exact,
// engine-owned root diagnostic; never expose trap text or artifact bytes.
fn call_error(error: &wasmtime::Error) -> ExecutionError {
    const FUEL_EXHAUSTED: &str = "too much data is being copied between the host and the guest: fuel allocated for hostcalls has been exhausted";
    ExecutionError::new(if error.root_cause().to_string() == FUEL_EXHAUSTED {
        "buffer_limit_exceeded"
    } else {
        "guest_trap"
    })
}
