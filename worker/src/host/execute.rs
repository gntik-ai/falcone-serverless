//! UTF-8 handler execution from verified trusted-compiler cache loads.
use super::{Host, runtime};
use crate::artifact::validate::HANDLER_EXPORT;
use crate::cache::Loaded;
use wasmtime::component::{ComponentType, Lift, TypedFunc};
use wasmtime::{CallHook, Store};

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
    import_fuel: usize,
}
impl Prepared {
    /// The cache validates raw components before bounded child compilation and
    /// verifies native output before loading. Execution never compiles raw Wasm.
    pub fn new(loaded: Loaded, host: Host) -> Result<Self, ExecutionError> {
        if let Some(code) = host.completion_error() {
            return Err(ExecutionError::new(code));
        }
        let component = loaded.into_component();
        let engine = component.engine().clone();
        if !engine.get_epoch_interruption() {
            return Err(ExecutionError::new("guest_deadline_unavailable"));
        }
        let (mut store, instance) =
            runtime::instantiate_component(&engine, component, host, Some(u64::MAX))
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
        let import_fuel = store.hostcall_fuel();
        store.call_hook(move |mut store, transition| {
            // A core export has returned, but Wasmtime has not yet lifted its
            // result. Bound that lift before Vec/String allocation. UTF-8 is
            // validated up front, so encoded bytes equal host allocation bytes.
            // On entry to guest code (including realloc/post-return) and host
            // imports, preserve the existing import allowance and WIT errors.
            let fuel = match transition {
                CallHook::ReturningFromWasm => store.data().input_limit(),
                _ => import_fuel,
            };
            store.set_hostcall_fuel(fuel);
            Ok(())
        });
        Ok(Self {
            store,
            handle,
            import_fuel,
        })
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
        // Restore import accounting before argument lowering, including on a
        // repeated call. The hook applies the configured limit only on return.
        self.store.set_hostcall_fuel(self.import_fuel);
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

/// A single invocation of a verified trusted-compiler cache load.
pub fn execute(loaded: Loaded, params: &[u8], host: Host) -> Result<Vec<u8>, ExecutionError> {
    if params.len() > host.output_limit() {
        return Err(ExecutionError::new("buffer_limit_exceeded"));
    }
    Prepared::new(loaded, host)?.call(params)
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
