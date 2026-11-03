//! The capability linker: instantiates a guest component against only the interfaces that the
//! verified grants cover (plus the authority-free baseline interfaces).
//!
//! Instantiation fails before any guest code runs when the component imports anything else,
//! WASI filesystem, sockets and environment included, or a `falcone:host` interface that no
//! grant covers. The error names the import. Nothing is linked as a fallback.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use wasmtime as engine;

use engine::component::{
    Component, ComponentType, Instance, Lift, Linker, Lower, Resource, ResourceTable, ResourceType,
    WasmList,
};
use engine::{
    AsContextMut, Config, Engine, Store, StoreContextMut, StoreLimits, StoreLimitsBuilder,
};

use super::HostError;
use super::bindings::{
    CancelToken, Discard, EventBinding, LogSink, ProgressSink, StorageBinding, WorkflowBinding,
};
use super::call::run_bounded;
use super::egress::{Connector, HttpRequest, Resolver, TcpConnector, fetch};
use super::grant::{self, GrantSet, ImportVerdict};
use super::limits::{
    HANDLE_FRAME_BYTES, HTTP_REQUEST_FRAME_BYTES, HostLimits, RECORD_FRAME_BYTES, U64_FRAME_BYTES,
    pair_bytes, result_bytes, string_bytes,
};

/// Guest linear memory is bounded for the host side of the ABI; profile budgets are RUN-01's.
const GUEST_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const NAME_BYTES: usize = 256;

/// The bindings behind the interfaces. A granted interface without its binding fails
/// instantiation; there is no fallback.
#[derive(Clone)]
pub struct Capabilities {
    pub storage: Option<Arc<dyn StorageBinding>>,
    pub events: Option<Arc<dyn EventBinding>>,
    pub workflows: Option<Arc<dyn WorkflowBinding>>,
    /// The resolver behind the HTTP gate.
    pub resolver: Option<Arc<dyn Resolver>>,
    /// Opens connections to addresses the gate validated.
    pub connector: Arc<dyn Connector>,
    pub logs: Arc<dyn LogSink>,
    pub progress: Arc<dyn ProgressSink>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            storage: None,
            events: None,
            workflows: None,
            resolver: None,
            connector: Arc::new(TcpConnector),
            logs: Arc::new(Discard),
            progress: Arc::new(Discard),
        }
    }
}

/// Read-only identifiers from verified invocation context.
#[derive(Clone, Debug, Default)]
pub struct InvocationContext {
    pub invocation_key: String,
    pub attempt_id: String,
    pub idempotency_key: String,
}

#[derive(Debug)]
pub enum GateError {
    /// The component imports something that is not a `falcone:host@0.1.0` interface.
    UndeclaredImport(String),
    /// A declared interface that no verified grant covers.
    UngrantedImport(String),
    /// A grant covers the interface but the embedder supplied no binding for it.
    MissingBinding(&'static str),
    Compile(String),
    Instantiate(String),
    Call(String),
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndeclaredImport(name) => write!(f, "undeclared import `{name}`"),
            Self::UngrantedImport(name) => write!(f, "import `{name}` is not covered by a grant"),
            Self::MissingBinding(name) => write!(f, "no binding for granted interface `{name}`"),
            Self::Compile(reason) => write!(f, "compile failed: {reason}"),
            Self::Instantiate(reason) => write!(f, "instantiation failed: {reason}"),
            Self::Call(reason) => write!(f, "call failed: {reason}"),
        }
    }
}

impl std::error::Error for GateError {}

#[derive(ComponentType, Lift, Lower, Clone, Copy)]
#[component(variant)]
enum WireError {
    #[component(name = "denied")]
    Denied,
    #[component(name = "limit-exceeded")]
    LimitExceeded,
    #[component(name = "timeout")]
    Timeout,
    #[component(name = "cancelled")]
    Cancelled,
}

impl From<HostError> for WireError {
    fn from(error: HostError) -> Self {
        match error {
            HostError::Denied => Self::Denied,
            HostError::LimitExceeded => Self::LimitExceeded,
            HostError::Timeout => Self::Timeout,
            HostError::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(ComponentType, Lift, Lower)]
#[component(record)]
struct Header {
    #[component(name = "name")]
    name: String,
    #[component(name = "value")]
    value: String,
}

#[derive(ComponentType, Lift, Lower)]
#[component(record)]
struct Response {
    #[component(name = "status")]
    status: u16,
    #[component(name = "headers")]
    headers: Vec<Header>,
    #[component(name = "body")]
    body: Vec<u8>,
}

#[derive(ComponentType, Lift, Lower)]
#[component(record)]
struct Field {
    #[component(name = "name")]
    name: String,
    #[component(name = "value")]
    value: String,
}

#[derive(ComponentType, Lift, Lower, Clone, Copy)]
#[component(enum)]
#[repr(u8)]
enum Level {
    #[component(name = "debug")]
    Debug,
    #[component(name = "info")]
    Info,
    #[component(name = "warn")]
    Warn,
    #[component(name = "error")]
    Error,
}

impl Level {
    fn name(self) -> &'static str {
        [
            (Self::Debug, "debug"),
            (Self::Info, "info"),
            (Self::Warn, "warn"),
            (Self::Error, "error"),
        ]
        .into_iter()
        .find(|(level, _)| *level as u8 == self as u8)
        .map_or("error", |(_, name)| name)
    }
}

struct Object(String);
struct Checkpoint(String);
struct Execution {
    target: String,
    id: String,
}

/// Per-invocation host state: the verified grants, the bindings and the handle table.
pub struct HostState {
    grants: Arc<GrantSet>,
    caps: Arc<Capabilities>,
    limits: HostLimits,
    context: InvocationContext,
    cancel: CancelToken,
    live: Arc<AtomicUsize>,
    table: ResourceTable,
    store_limits: StoreLimits,
}

type Wire<T> = (Result<T, WireError>,);

fn wire<T>(result: Result<T, HostError>) -> engine::Result<Wire<T>> {
    Ok((result.map_err(WireError::from),))
}

impl HostState {
    fn bounded<T, F>(&self, call: F) -> Result<T, HostError>
    where
        T: Send + 'static,
        F: FnOnce(&super::bindings::CallControl) -> Result<T, HostError> + Send + 'static,
    {
        run_bounded(&self.live, &self.cancel, self.limits.call_timeout(), call)
    }

    fn open_object(&mut self, key: String) -> Result<Resource<Object>, HostError> {
        self.limits.check_request(&[string_bytes(key.len())])?;
        self.grants.authorize_storage_key(&key)?;
        self.table
            .push(Object(key))
            .map_err(|_| HostError::LimitExceeded)
    }

    fn open_checkpoint(&mut self, key: String) -> Result<Resource<Checkpoint>, HostError> {
        self.limits.check_request(&[string_bytes(key.len())])?;
        self.grants.authorize_storage_key(&key)?;
        self.table
            .push(Checkpoint(key))
            .map_err(|_| HostError::LimitExceeded)
    }

    fn storage_read(&mut self, key: String) -> Result<Vec<u8>, HostError> {
        // Authorization is re-derived from the grant at use, not trusted from the handle.
        self.grants.authorize_storage_key(&key)?;
        let binding = self.caps.storage.clone().ok_or(HostError::Denied)?;
        let data = self.bounded(move |c| binding.read(&key, c).map_err(|e| e.into_host(c)))?;
        self.limits.check_response(result_bytes(data.len()))?;
        Ok(data)
    }

    fn storage_write(&mut self, key: String, data: Vec<u8>) -> Result<(), HostError> {
        self.limits
            .check_request(&[HANDLE_FRAME_BYTES, string_bytes(data.len())])?;
        self.grants.authorize_storage_key(&key)?;
        let binding = self.caps.storage.clone().ok_or(HostError::Denied)?;
        self.bounded(move |c| binding.write(&key, &data, c).map_err(|e| e.into_host(c)))
    }

    fn publish(&mut self, topic: String, payload: Vec<u8>) -> Result<(), HostError> {
        self.limits
            .check_request(&[string_bytes(topic.len()), string_bytes(payload.len())])?;
        self.grants.authorize_event_topic(&topic)?;
        let binding = self.caps.events.clone().ok_or(HostError::Denied)?;
        self.bounded(move |c| {
            binding
                .publish(&topic, &payload, c)
                .map_err(|e| e.into_host(c))
        })
    }

    fn start_workflow(
        &mut self,
        target: String,
        input: Vec<u8>,
    ) -> Result<Resource<Execution>, HostError> {
        self.limits
            .check_request(&[string_bytes(target.len()), string_bytes(input.len())])?;
        self.grants.authorize_workflow_target(&target)?;
        let binding = self.caps.workflows.clone().ok_or(HostError::Denied)?;
        let name = target.clone();
        let id =
            self.bounded(move |c| binding.start(&name, &input, c).map_err(|e| e.into_host(c)))?;
        self.limits.check_response(id.len())?;
        self.table
            .push(Execution { target, id })
            .map_err(|_| HostError::LimitExceeded)
    }

    fn signal_workflow(
        &mut self,
        handle: &Resource<Execution>,
        name: String,
        payload: Vec<u8>,
    ) -> Result<(), HostError> {
        self.limits.check_request(&[
            HANDLE_FRAME_BYTES,
            string_bytes(name.len()),
            string_bytes(payload.len()),
        ])?;
        let execution = self.table.get(handle).map_err(|_| HostError::Denied)?;
        let (target, id) = (execution.target.clone(), execution.id.clone());
        self.grants.authorize_workflow_target(&target)?;
        let binding = self.caps.workflows.clone().ok_or(HostError::Denied)?;
        self.bounded(move |c| {
            binding
                .signal(&target, &id, &name, &payload, c)
                .map_err(|e| e.into_host(c))
        })
    }

    fn http(&mut self, request: HttpRequest) -> Result<Response, HostError> {
        let resolver = self.caps.resolver.clone().ok_or(HostError::Denied)?;
        let connector = Arc::clone(&self.caps.connector);
        let grants = Arc::clone(&self.grants);
        let limits = self.limits;
        let response = self.bounded(move |c| {
            fetch(
                resolver.as_ref(),
                connector.as_ref(),
                &grants,
                &limits,
                request,
                c,
            )
        })?;
        Ok(Response {
            status: response.status,
            headers: response
                .headers
                .into_iter()
                .map(|(name, value)| Header { name, value })
                .collect(),
            body: response.body,
        })
    }

    /// `fields` were lifted under `lift_pairs`, so their aggregate is already inside the ceiling.
    fn log(&mut self, level: Level, message: String, fields: Vec<Field>) -> Result<(), HostError> {
        self.limits
            .check_request(&[message.len(), LOG_FRAME_BYTES, log_field_bytes(&fields)?])?;
        let fields: Vec<(String, String)> = fields
            .into_iter()
            .map(|f| {
                let value = if sensitive(&f.name) {
                    "<REDACTED>".to_owned()
                } else {
                    f.value
                };
                (f.name, value)
            })
            .collect();
        let logs = Arc::clone(&self.caps.logs);
        let level = level.name();
        self.bounded(move |_| {
            logs.log(level, &message, &fields);
            Ok(())
        })
    }

    fn report_progress(
        &mut self,
        completed: u64,
        total: u64,
        message: String,
    ) -> Result<(), HostError> {
        self.limits
            .check_request(&[2 * U64_FRAME_BYTES, string_bytes(message.len())])?;
        let progress = Arc::clone(&self.caps.progress);
        self.bounded(move |_| {
            progress.report(completed, total, &message);
            Ok(())
        })
    }

    fn report_checkpoint(&mut self, handle: &Resource<Checkpoint>) -> Result<(), HostError> {
        let key = self
            .table
            .get(handle)
            .map_err(|_| HostError::Denied)?
            .0
            .clone();
        let progress = Arc::clone(&self.caps.progress);
        self.bounded(move |_| {
            progress.checkpoint(&key);
            Ok(())
        })
    }
}

/// Canonical-ABI bytes of the call outside the message text and the field strings: severity,
/// the message pointer and length, and the field list pointer and length.
const LOG_FRAME_BYTES: usize = 20;

/// Serialized size of every field: its record plus both strings. Overflow is over the limit.
fn log_field_bytes(fields: &[Field]) -> Result<usize, HostError> {
    fields
        .iter()
        .try_fold(0usize, |total, f| {
            total.checked_add(pair_bytes(f.name.len(), f.value.len()))
        })
        .ok_or(HostError::LimitExceeded)
}

/// Lifts a list of `name`/`value` records lazily. The list length is known from the call frame,
/// so a request whose records alone exceed the ceiling (with `fixed` bytes of the rest of the
/// call) is refused before any element is copied, and lifting stops at the first element that
/// takes the aggregate over it. Elements may alias one large guest string, so nothing is
/// allocated for the remainder once the ceiling is crossed.
fn lift_pairs<T: Lift + 'static>(
    mut cx: StoreContextMut<HostState>,
    fixed: usize,
    list: &WasmList<T>,
    lengths: fn(&T) -> (usize, usize),
) -> Result<Vec<T>, HostError> {
    let limits = cx.data().limits;
    let records = list
        .len()
        .checked_mul(RECORD_FRAME_BYTES)
        .ok_or(HostError::LimitExceeded)?;
    limits.check_request(&[fixed, records])?;
    let mut lifted = Vec::new();
    let mut strings = 0usize;
    for item in list.iter(&mut cx).map_err(|_| HostError::Denied)? {
        let item = item.map_err(|_| HostError::Denied)?;
        let (name, value) = lengths(&item);
        strings = strings
            .checked_add(name)
            .and_then(|total| total.checked_add(value))
            .ok_or(HostError::LimitExceeded)?;
        limits.check_request(&[fixed, records, strings])?;
        lifted.push(item);
    }
    Ok(lifted)
}

/// Same spelling rule as the manifest's property denylist: case, `_`, `-` and `.` ignored.
fn sensitive(name: &str) -> bool {
    let key: String = name
        .chars()
        .filter(|c| !matches!(c, '_' | '-' | '.'))
        .flat_map(char::to_lowercase)
        .collect();
    [
        "value",
        "plaintext",
        "secret",
        "secretvalue",
        "clientsecret",
        "password",
        "passwd",
        "token",
        "authorization",
        "apikey",
        "credential",
        "credentials",
        "privatekey",
        "accesskey",
    ]
    .contains(&key.as_str())
        && key != "value"
}

fn destroy<T: 'static>(mut cx: StoreContextMut<HostState>, rep: u32) -> engine::Result<()> {
    cx.data_mut().table.delete(Resource::<T>::new_own(rep))?;
    Ok(())
}

fn link_cancellation(linker: &mut Linker<HostState>) -> engine::Result<()> {
    let mut instance = linker.instance("falcone:host/cancellation@0.1.0")?;
    instance.func_wrap("is-cancelled", |cx: StoreContextMut<HostState>, (): ()| {
        Ok((cx.data().cancel.is_cancelled(),))
    })
}

fn link_invocation_context(linker: &mut Linker<HostState>) -> engine::Result<()> {
    let mut instance = linker.instance("falcone:host/invocation-context@0.1.0")?;
    instance.func_wrap(
        "invocation-key",
        |cx: StoreContextMut<HostState>, (): ()| Ok((cx.data().context.invocation_key.clone(),)),
    )?;
    instance.func_wrap("attempt-id", |cx: StoreContextMut<HostState>, (): ()| {
        Ok((cx.data().context.attempt_id.clone(),))
    })?;
    instance.func_wrap(
        "idempotency-key",
        |cx: StoreContextMut<HostState>, (): ()| Ok((cx.data().context.idempotency_key.clone(),)),
    )
}

fn link_logging(linker: &mut Linker<HostState>) -> engine::Result<()> {
    let mut instance = linker.instance("falcone:host/logging@0.1.0")?;
    instance.func_wrap(
        "log",
        |mut cx: StoreContextMut<HostState>,
         (level, message, fields): (Level, String, WasmList<Field>)| {
            let fixed = message.len().saturating_add(LOG_FRAME_BYTES);
            let lengths = |f: &Field| (f.name.len(), f.value.len());
            let fields = match lift_pairs(cx.as_context_mut(), fixed, &fields, lengths) {
                Ok(fields) => fields,
                Err(error) => return wire(Err(error)),
            };
            wire(cx.data_mut().log(level, message, fields))
        },
    )
}

fn link_progress(linker: &mut Linker<HostState>) -> engine::Result<()> {
    let mut instance = linker.instance("falcone:host/progress@0.1.0")?;
    instance.func_wrap(
        "report",
        |mut cx: StoreContextMut<HostState>, (completed, total, message): (u64, u64, String)| {
            wire(cx.data_mut().report_progress(completed, total, message))
        },
    )?;
    instance.func_wrap(
        "report-checkpoint",
        |mut cx: StoreContextMut<HostState>, (handle,): (Resource<Checkpoint>,)| {
            wire(cx.data_mut().report_checkpoint(&handle))
        },
    )
}

fn link_storage(linker: &mut Linker<HostState>) -> engine::Result<()> {
    let mut instance = linker.instance(grant::STORAGE)?;
    instance.resource("object", ResourceType::host::<Object>(), destroy::<Object>)?;
    instance.resource(
        "checkpoint",
        ResourceType::host::<Checkpoint>(),
        destroy::<Checkpoint>,
    )?;
    instance.func_wrap(
        "open-object",
        |mut cx: StoreContextMut<HostState>, (key,): (String,)| {
            wire(cx.data_mut().open_object(key))
        },
    )?;
    instance.func_wrap(
        "read-object",
        |mut cx: StoreContextMut<HostState>, (handle,): (Resource<Object>,)| {
            let state = cx.data_mut();
            let key = state.table.get(&handle).map(|o| o.0.clone());
            wire(
                key.map_err(|_| HostError::Denied)
                    .and_then(|key| state.storage_read(key)),
            )
        },
    )?;
    instance.func_wrap(
        "write-object",
        |mut cx: StoreContextMut<HostState>, (handle, data): (Resource<Object>, Vec<u8>)| {
            let state = cx.data_mut();
            let key = state.table.get(&handle).map(|o| o.0.clone());
            wire(
                key.map_err(|_| HostError::Denied)
                    .and_then(|key| state.storage_write(key, data)),
            )
        },
    )?;
    instance.func_wrap(
        "open-checkpoint",
        |mut cx: StoreContextMut<HostState>, (key,): (String,)| {
            wire(cx.data_mut().open_checkpoint(key))
        },
    )?;
    instance.func_wrap(
        "read-checkpoint",
        |mut cx: StoreContextMut<HostState>, (handle,): (Resource<Checkpoint>,)| {
            let state = cx.data_mut();
            let key = state.table.get(&handle).map(|c| c.0.clone());
            wire(
                key.map_err(|_| HostError::Denied)
                    .and_then(|key| state.storage_read(key)),
            )
        },
    )?;
    instance.func_wrap(
        "write-checkpoint",
        |mut cx: StoreContextMut<HostState>, (handle, data): (Resource<Checkpoint>, Vec<u8>)| {
            let state = cx.data_mut();
            let key = state.table.get(&handle).map(|c| c.0.clone());
            wire(
                key.map_err(|_| HostError::Denied)
                    .and_then(|key| state.storage_write(key, data)),
            )
        },
    )
}

fn link_http(linker: &mut Linker<HostState>) -> engine::Result<()> {
    let mut instance = linker.instance(grant::HTTP)?;
    instance.func_wrap(
        "request",
        |mut cx: StoreContextMut<HostState>,
         (url, method, headers, body): (String, String, WasmList<Header>, Vec<u8>)| {
            let fixed = [url.len(), method.len(), body.len()]
                .into_iter()
                .fold(HTTP_REQUEST_FRAME_BYTES, usize::saturating_add);
            let lengths = |h: &Header| (h.name.len(), h.value.len());
            let headers = match lift_pairs(cx.as_context_mut(), fixed, &headers, lengths) {
                Ok(headers) => headers,
                Err(error) => return wire(Err(error)),
            };
            let request = HttpRequest {
                url,
                method,
                headers: headers.into_iter().map(|h| (h.name, h.value)).collect(),
                body,
            };
            wire(cx.data_mut().http(request))
        },
    )
}

fn link_events(linker: &mut Linker<HostState>) -> engine::Result<()> {
    let mut instance = linker.instance(grant::EVENTS)?;
    instance.func_wrap(
        "publish",
        |mut cx: StoreContextMut<HostState>, (topic, payload): (String, Vec<u8>)| {
            wire(cx.data_mut().publish(topic, payload))
        },
    )
}

fn link_workflows(linker: &mut Linker<HostState>) -> engine::Result<()> {
    let mut instance = linker.instance(grant::WORKFLOWS)?;
    instance.resource(
        "execution",
        ResourceType::host::<Execution>(),
        destroy::<Execution>,
    )?;
    instance.func_wrap(
        "start",
        |mut cx: StoreContextMut<HostState>, (target, input): (String, Vec<u8>)| {
            wire(cx.data_mut().start_workflow(target, input))
        },
    )?;
    instance.func_wrap(
        "signal",
        |mut cx: StoreContextMut<HostState>,
         (handle, name, payload): (Resource<Execution>, String, Vec<u8>)| {
            wire(cx.data_mut().signal_workflow(&handle, name, payload))
        },
    )
}

/// The engine, the verified grants and the bindings of one invocation.
pub struct Gate {
    engine: Engine,
    grants: Arc<GrantSet>,
    caps: Arc<Capabilities>,
    limits: HostLimits,
}

impl Gate {
    pub fn new(
        grants: GrantSet,
        caps: Capabilities,
        limits: HostLimits,
    ) -> Result<Self, GateError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config).map_err(|e| GateError::Compile(format!("{e:#}")))?;
        Ok(Self {
            engine,
            grants: Arc::new(grants),
            caps: Arc::new(caps),
            limits,
        })
    }

    /// Compiles a validated component binary. Text and native code are refused here: tenants
    /// supply neither, and compilation always starts from the component bytes.
    pub fn compile(&self, bytes: &[u8]) -> Result<Component, GateError> {
        if !bytes.starts_with(b"\0asm") {
            return Err(GateError::Compile("not a WebAssembly binary".into()));
        }
        Component::new(&self.engine, bytes).map_err(|e| GateError::Compile(format!("{e:#}")))
    }

    /// Compiles a trusted WAT fixture (qualification only; never fed from tenant input).
    pub fn compile_wat(&self, text: &str) -> Result<Component, GateError> {
        Component::new(&self.engine, text.as_bytes())
            .map_err(|e| GateError::Compile(format!("{e:#}")))
    }

    /// Checks every import against the grants, then links and instantiates. The guest start
    /// function, if any, runs only after every check has passed.
    pub fn instantiate(
        &self,
        component: &Component,
        context: InvocationContext,
    ) -> Result<Guest, GateError> {
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        let link =
            |result: engine::Result<()>| result.map_err(|e| GateError::Instantiate(e.to_string()));
        for (name, _) in component.component_type().imports(&self.engine) {
            match self.grants.classify_import(name) {
                ImportVerdict::Granted | ImportVerdict::Baseline => {}
                ImportVerdict::Ungranted => {
                    return Err(GateError::UngrantedImport(printable(name)));
                }
                ImportVerdict::Undeclared => {
                    return Err(GateError::UndeclaredImport(printable(name)));
                }
            }
        }
        link(link_cancellation(&mut linker))?;
        link(link_invocation_context(&mut linker))?;
        link(link_logging(&mut linker))?;
        link(link_progress(&mut linker))?;
        let caps = &self.caps;
        if self.grants.covers_interface(grant::STORAGE) {
            require(caps.storage.is_some(), grant::STORAGE)?;
            link(link_storage(&mut linker))?;
        }
        if self.grants.covers_interface(grant::HTTP) {
            require(caps.resolver.is_some(), grant::HTTP)?;
            link(link_http(&mut linker))?;
        }
        if self.grants.covers_interface(grant::EVENTS) {
            require(caps.events.is_some(), grant::EVENTS)?;
            link(link_events(&mut linker))?;
        }
        if self.grants.covers_interface(grant::WORKFLOWS) {
            require(caps.workflows.is_some(), grant::WORKFLOWS)?;
            link(link_workflows(&mut linker))?;
        }
        let cancel = CancelToken::default();
        let mut store = Store::new(
            &self.engine,
            HostState {
                grants: Arc::clone(&self.grants),
                caps: Arc::clone(&self.caps),
                limits: self.limits,
                context,
                cancel: cancel.clone(),
                live: Arc::new(AtomicUsize::new(0)),
                table: ResourceTable::new(),
                store_limits: StoreLimitsBuilder::new()
                    .memory_size(GUEST_MEMORY_BYTES)
                    .instances(8)
                    .build(),
            },
        );
        store.limiter(|state| &mut state.store_limits);
        let instance = linker
            .instantiate(&mut store, component)
            .map_err(|e| GateError::Instantiate(format!("{e:#}")))?;
        Ok(Guest {
            store,
            instance,
            cancel,
        })
    }
}

fn require(present: bool, interface: &'static str) -> Result<(), GateError> {
    present
        .then_some(())
        .ok_or(GateError::MissingBinding(interface))
}

/// Import names come from tenant bytes: bounded and free of control characters in errors.
fn printable(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(NAME_BYTES)
        .collect()
}

pub struct Guest {
    store: Store<HostState>,
    instance: Instance,
    cancel: CancelToken,
}

impl Guest {
    /// The embedder's cancellation handle for this invocation.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Calls an exported `func() -> u32`.
    pub fn call_u32(&mut self, export: &str) -> Result<u32, GateError> {
        let func = self
            .instance
            .get_typed_func::<(), (u32,)>(&mut self.store, export)
            .map_err(|e| GateError::Call(e.to_string()))?;
        func.call(&mut self.store, ())
            .map(|(value,)| value)
            .map_err(|e| GateError::Call(format!("{e:#}")))
    }
}
