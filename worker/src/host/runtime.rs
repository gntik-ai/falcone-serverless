//! Lazy canonical-ABI arguments: list lengths are checked before element reads,
//! and strings are checked after `to_str`, before copying. UTF-8 borrows guest
//! memory. UTF-16 and Latin-1 decoding allocates inside Wasmtime; that allocation
//! is bounded by the guest linear memory capped by the store limiter. UTF-8
//! validation still takes CPU proportional to the guest range. No raw ABI lifting.
use super::{Bindings, Context, HostError, Level, Operation, Pair, Reply, import_allowed};
use std::collections::BTreeMap;
use std::sync::{
    Arc, Condvar, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use wasmtime::component::{
    Component, ComponentType, Instance, Lift, Linker, LinkerInstance, Resource, ResourceType,
    WasmList, WasmStr,
};
use wasmtime::{Engine, Store, StoreContextMut, StoreLimits, StoreLimitsBuilder};

pub const BUFFER_BYTES: usize = 1_048_576;
pub const GUEST_MEMORY_BYTES: usize = 256 * 1024 * 1024;
// Retained storage metadata is outside guest linear memory. With at most 1024
// live handles, this bounds retained keys to 1 MiB, independently of call count.
const STORAGE_KEY_BYTES: usize = 1024;
// ADR 0007's qualification concurrency is 20. Reserve four additional helpers
// for detached calls, but bound *all* running helpers, including binding drops.
const HELPER_THREADS: usize = 20 + 4;
const STALLED_PER_BINDING: usize = 4;

#[derive(Default)]
struct BudgetState {
    live: usize,
    stalled: BTreeMap<usize, usize>,
}
#[derive(Default)]
struct ThreadBudget {
    state: Mutex<BudgetState>,
    available: Condvar,
}
#[derive(Default)]
struct CallState {
    detached: bool,
    finished: bool,
}
struct CallTicket {
    budget: &'static ThreadBudget,
    binding_id: usize,
    // Keep the allocation identity stable even during the binding's destructor.
    _binding: Weak<dyn Bindings>,
    state: Mutex<CallState>,
}
struct Permit(Arc<CallTicket>);

impl CallTicket {
    fn detach(&self) {
        // Always lock budget before call state, as in Permit::drop.
        let mut budget = self
            .budget
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut call = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !call.finished && !call.detached {
            call.detached = true;
            *budget.stalled.entry(self.binding_id).or_default() += 1;
            self.budget.available.notify_all();
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        let ticket = &self.0;
        let mut budget = ticket
            .budget
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut call = ticket
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        call.finished = true;
        budget.live -= 1;
        if call.detached {
            let remaining = budget
                .stalled
                .get_mut(&ticket.binding_id)
                .expect("detached call is registered");
            *remaining -= 1;
            if *remaining == 0 {
                budget.stalled.remove(&ticket.binding_id);
            }
        }
        ticket.budget.available.notify_all();
    }
}
fn permit(
    binding: &Arc<dyn Bindings>,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Permit, HostError> {
    // One budget for this worker process, independent of guest/store lifetime.
    static BUDGET: OnceLock<ThreadBudget> = OnceLock::new();
    let budget = BUDGET.get_or_init(ThreadBudget::default);
    let binding_id = Arc::as_ptr(binding) as *const () as usize;
    let mut state = budget
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(HostError::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HostError::Timeout);
        }
        if state.stalled.get(&binding_id).copied().unwrap_or(0) >= STALLED_PER_BINDING
            || state.stalled.values().sum::<usize>() >= HELPER_THREADS
        {
            return Err(HostError::LimitExceeded);
        }
        if state.live < HELPER_THREADS {
            state.live += 1;
            return Ok(Permit(Arc::new(CallTicket {
                budget,
                binding_id,
                _binding: Arc::downgrade(binding),
                state: Mutex::new(CallState::default()),
            })));
        }
        // Healthy contention waits within the call deadline instead of returning
        // a spurious limit error. Cancellation is independent of fuel/epochs.
        state = budget
            .available
            .wait_timeout(state, remaining.min(Duration::from_millis(2)))
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
    }
}

// Struct fields drop in declaration order, on return and during unwinding.
// The closure owns this entire value, so all binding captures retire before
// its permit, including a binding destructor that blocks after a panic.
struct Helper {
    binding: Arc<dyn Bindings>,
    context: Context,
    cancelled: Arc<AtomicBool>,
    operation: Option<Operation>,
    send: mpsc::SyncSender<Result<Reply, HostError>>,
    deadline: Instant,
    _permit: Permit,
}
impl Helper {
    fn run(mut self) {
        let result = match self.operation.take().expect("helper operation") {
            Operation::Http(request) => super::net::request(
                self.binding.as_ref(),
                &self.context,
                request,
                self.deadline,
                self.cancelled.clone(),
            )
            .map(Reply::Http),
            operation => self.binding.call(&self.context, operation),
        };
        let _ = self.send.send(result);
    }
}

#[derive(Default)]
struct Bytes(usize);
impl Bytes {
    fn add(&mut self, bytes: usize) -> Result<(), HostError> {
        if !super::aggregate_within_limit(&[self.0, bytes]) {
            return Err(HostError::LimitExceeded);
        }
        self.0 += bytes;
        Ok(())
    }
}

fn empty(reply: Reply) -> Result<(), HostError> {
    match reply {
        Reply::Empty => Ok(()),
        _ => Err(HostError::Denied),
    }
}

struct Object;
struct Checkpoint;
struct Execution;

fn string(
    value: &WasmStr,
    store: &StoreContextMut<'_, Host>,
    bytes: &mut Bytes,
) -> Result<String, HostError> {
    bytes.add(4)?;
    string_content(value, store, bytes)
}

fn string_content(
    value: &WasmStr,
    store: &StoreContextMut<'_, Host>,
    bytes: &mut Bytes,
) -> Result<String, HostError> {
    let value = value.to_str(store).map_err(|_| HostError::Denied)?;
    bytes.add(value.len())?;
    Ok(value.into_owned())
}

fn data(
    value: &WasmList<u8>,
    store: &mut StoreContextMut<'_, Host>,
    bytes: &mut Bytes,
) -> Result<Vec<u8>, HostError> {
    bytes.add(4)?;
    bytes.add(value.len())?;
    Ok(value.as_le_slice(store).to_vec())
}

#[derive(ComponentType, Lift)]
#[component(record)]
struct LazyPair {
    name: WasmStr,
    value: WasmStr,
}

fn pairs(
    value: &WasmList<LazyPair>,
    store: &mut StoreContextMut<'_, Host>,
    bytes: &mut Bytes,
) -> Result<Vec<Pair>, HostError> {
    bytes.add(4)?;
    bytes.add(value.len().checked_mul(8).ok_or(HostError::LimitExceeded)?)?;
    let mut result = Vec::new();
    for index in 0..value.len() {
        let pair = value
            .get(&mut *store, index)
            .ok_or(HostError::Denied)?
            .map_err(|_| HostError::Denied)?;
        let name = string_content(&pair.name, store, bytes)?;
        let value = string_content(&pair.value, store, bytes)?;
        result.push(Pair { name, value });
    }
    Ok(result)
}

// Validate the whole aggregate before making any UTF-8 host copy. In particular,
// a large valid prefix of fields cannot allocate before a later oversized field.
fn preflight(
    store: &mut StoreContextMut<'_, Host>,
    strings: &[&WasmStr],
    lists: &[&WasmList<u8>],
    fields: Option<&WasmList<LazyPair>>,
    overhead: usize,
) -> Result<(), HostError> {
    let mut bytes = Bytes::default();
    bytes.add(overhead)?;
    for list in lists {
        bytes.add(4)?;
        bytes.add(list.len())?;
    }
    if let Some(fields) = fields {
        bytes.add(4)?;
        bytes.add(
            fields
                .len()
                .checked_mul(8)
                .ok_or(HostError::LimitExceeded)?,
        )?;
    }
    for string in strings {
        let value = string.to_str(&*store).map_err(|_| HostError::Denied)?;
        bytes.add(4)?;
        bytes.add(value.len())?;
    }
    if let Some(fields) = fields {
        for index in 0..fields.len() {
            let pair = fields
                .get(&mut *store, index)
                .ok_or(HostError::Denied)?
                .map_err(|_| HostError::Denied)?;
            for string in [&pair.name, &pair.value] {
                let value = string.to_str(&*store).map_err(|_| HostError::Denied)?;
                bytes.add(value.len())?;
            }
        }
    }
    Ok(())
}

pub struct Host {
    pub context: Context,
    pub bindings: Arc<dyn Bindings>,
    limits: StoreLimits,
    call_timeout: Duration,
    cancelled: Arc<AtomicBool>,
    keys: BTreeMap<u32, (bool, String)>,
    next_handle: u32,
    executions: BTreeMap<u32, (String, String)>,
}

impl Host {
    pub fn new(context: Context, bindings: Arc<dyn Bindings>) -> Self {
        Self {
            context,
            bindings,
            limits: StoreLimitsBuilder::new()
                .memory_size(GUEST_MEMORY_BYTES)
                .memories(1)
                .tables(4)
                .build(),
            call_timeout: Duration::from_secs(30),
            cancelled: Arc::new(AtomicBool::new(false)),
            keys: BTreeMap::new(),
            next_handle: 0,
            executions: BTreeMap::new(),
        }
    }

    pub fn set_call_timeout(&mut self, timeout: Duration) {
        self.call_timeout = timeout.min(Duration::from_secs(30));
    }

    pub fn cancellation(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    fn handle(&mut self) -> Result<u32, HostError> {
        if self.keys.len() + self.executions.len() >= 1024 {
            return Err(HostError::LimitExceeded);
        }
        let handle = self.next_handle;
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .ok_or(HostError::LimitExceeded)?;
        Ok(handle)
    }

    fn key(&self, rep: u32, checkpoint: bool) -> Result<String, HostError> {
        self.keys
            .get(&rep)
            .filter(|(kind, key)| *kind == checkpoint && self.context.grants.allows_key(key))
            .map(|(_, key)| key.clone())
            .ok_or(HostError::Denied)
    }

    fn call(&self, operation: Operation) -> Result<Reply, HostError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(HostError::Cancelled);
        }
        let deadline = Instant::now() + self.call_timeout;
        let permit = permit(&self.bindings, deadline, &self.cancelled)?;
        let ticket = permit.0.clone();
        let (send, receive) = mpsc::sync_channel(1);
        let helper = Helper {
            binding: self.bindings.clone(),
            context: self.context.clone(),
            cancelled: self.cancelled.clone(),
            operation: Some(operation),
            send,
            deadline,
            _permit: permit,
        };
        std::thread::Builder::new()
            .name("capability-binding".into())
            .spawn(move || helper.run())
            .map_err(|_| HostError::LimitExceeded)?;
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                ticket.detach();
                return Err(HostError::Cancelled);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                ticket.detach();
                return Err(HostError::Timeout);
            }
            match receive.recv_timeout(remaining.min(Duration::from_millis(2))) {
                Ok(reply) => return reply,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(HostError::Denied),
            }
        }
    }
}

fn storage_resource<T: Send + 'static>(
    linker: &mut LinkerInstance<'_, Host>,
    names: [&str; 4],
    checkpoint: bool,
) -> wasmtime::Result<()> {
    linker.resource(names[0], ResourceType::host::<T>(), |mut store, rep| {
        store.data_mut().keys.remove(&rep);
        Ok(())
    })?;
    linker.func_wrap(
        names[1],
        move |mut store: StoreContextMut<'_, Host>, (key,): (WasmStr,)| {
            let result: Result<Resource<T>, HostError> = (|| {
                let key = key.to_str(&store).map_err(|_| HostError::Denied)?;
                if key.len() > STORAGE_KEY_BYTES {
                    return Err(HostError::LimitExceeded);
                }
                if !store.data().context.grants.allows_key(&key) {
                    return Err(HostError::Denied);
                }
                let key = key.into_owned();
                let rep = store.data_mut().handle()?;
                store.data_mut().keys.insert(rep, (checkpoint, key));
                Ok(Resource::new_own(rep))
            })();
            Ok((result,))
        },
    )?;
    linker.func_wrap(
        names[2],
        move |store: StoreContextMut<'_, Host>, (handle,): (Resource<T>,)| {
            let result: Result<Vec<u8>, HostError> = (|| {
                let key = store.data().key(handle.rep(), checkpoint)?;
                let Reply::Data(data) = store.data().call(Operation::Read { key, checkpoint })?
                else {
                    return Err(HostError::Denied);
                };
                let mut bytes = Bytes::default();
                bytes.add(4)?;
                bytes.add(data.len())?;
                Ok(data)
            })();
            Ok((result,))
        },
    )?;
    linker.func_wrap(
        names[3],
        move |mut store: StoreContextMut<'_, Host>,
              (handle, data_arg): (Resource<T>, WasmList<u8>)| {
            let result: Result<(), HostError> = (|| {
                preflight(&mut store, &[], &[&data_arg], None, 4)?;
                let key = store.data().key(handle.rep(), checkpoint)?;
                let mut bytes = Bytes::default();
                bytes.add(4)?;
                let data = data(&data_arg, &mut store, &mut bytes)?;
                empty(store.data().call(Operation::Write {
                    key,
                    checkpoint,
                    data,
                })?)
            })();
            Ok((result,))
        },
    )
}

/// Qualification-only: compiles raw source in process, never native artifacts.
/// A runtime adapter must use the supervised trusted compiler for tenant input;
/// this fixture entry point does not enforce compilation CPU or memory budgets.
pub fn instantiate(
    engine: &Engine,
    source: impl AsRef<[u8]>,
    host: Host,
) -> wasmtime::Result<(Store<Host>, Instance)> {
    let mut context_bytes = Bytes::default();
    for value in [
        &host.context.invocation_key,
        &host.context.attempt_id,
        &host.context.idempotency_key,
    ] {
        context_bytes
            .add(4)
            .and_then(|()| context_bytes.add(value.len()))
            .map_err(|_| wasmtime::Error::msg("limit-exceeded: invocation context"))?;
    }
    let component = Component::new(engine, source)?;
    for (name, _) in component.component_type().imports(engine) {
        if !import_allowed(name, &host.context.grants) {
            return Err(wasmtime::Error::msg(format!("denied import: {name}")));
        }
    }
    let mut linker = Linker::new(engine);
    linker
        .instance("falcone:host/cancellation@0.1.0")?
        .func_wrap(
            "is-cancelled",
            |store: StoreContextMut<'_, Host>, (): ()| {
                Ok((store.data().cancelled.load(Ordering::Acquire),))
            },
        )?;
    let mut context = linker.instance("falcone:host/invocation-context@0.1.0")?;
    context.func_wrap(
        "invocation-key",
        |store: StoreContextMut<'_, Host>, (): ()| {
            Ok((store.data().context.invocation_key.clone(),))
        },
    )?;
    context.func_wrap("attempt-id", |store: StoreContextMut<'_, Host>, (): ()| {
        Ok((store.data().context.attempt_id.clone(),))
    })?;
    context.func_wrap(
        "idempotency-key",
        |store: StoreContextMut<'_, Host>, (): ()| {
            Ok((store.data().context.idempotency_key.clone(),))
        },
    )?;
    let mut progress = linker.instance("falcone:host/progress@0.1.0")?;
    progress.resource(
        "checkpoint",
        ResourceType::host::<Checkpoint>(),
        |mut store, rep| {
            store.data_mut().keys.remove(&rep);
            Ok(())
        },
    )?;
    progress.func_wrap(
        "report",
        |store: StoreContextMut<'_, Host>, (completed, total, message): (u64, u64, WasmStr)| {
            let result: Result<(), HostError> = (|| {
                let mut bytes = Bytes::default();
                bytes.add(16)?;
                let message = string(&message, &store, &mut bytes)?;
                empty(store.data().call(Operation::Progress {
                    completed,
                    total,
                    message,
                })?)
            })();
            Ok((result,))
        },
    )?;
    progress.func_wrap(
        "report-checkpoint",
        |store: StoreContextMut<'_, Host>, (handle,): (Resource<Checkpoint>,)| {
            let result: Result<(), HostError> = (|| {
                let key = store.data().key(handle.rep(), true)?;
                empty(store.data().call(Operation::ProgressCheckpoint { key })?)
            })();
            Ok((result,))
        },
    )?;
    if !host.context.grants.topics.is_empty() {
        linker.instance("falcone:host/events@0.1.0")?.func_wrap(
            "publish",
            |mut store: StoreContextMut<'_, Host>,
             (destination, payload): (WasmStr, WasmList<u8>)| {
                let result = (|| {
                    preflight(&mut store, &[&destination], &[&payload], None, 0)?;
                    let mut bytes = Bytes::default();
                    let destination = string(&destination, &store, &mut bytes)?;
                    // Check list length before reading or copying even one element.
                    let payload = data(&payload, &mut store, &mut bytes)?;
                    if !store
                        .data()
                        .context
                        .grants
                        .topics
                        .iter()
                        .any(|topic| topic == &destination && !topic.contains('*'))
                    {
                        return Err(HostError::Denied);
                    }
                    empty(store.data().call(Operation::Publish {
                        destination,
                        payload,
                    })?)
                })();
                Ok((result,))
            },
        )?;
    }
    linker.instance("falcone:host/logging@0.1.0")?.func_wrap(
        "log",
        |mut store: StoreContextMut<'_, Host>,
         (severity, message, fields): (Level, WasmStr, WasmList<LazyPair>)| {
            let result: Result<(), HostError> = (|| {
                preflight(&mut store, &[&message], &[], Some(&fields), 1)?;
                let mut bytes = Bytes::default();
                bytes.add(1)?;
                let message = string(&message, &store, &mut bytes)?;
                let mut fields = pairs(&fields, &mut store, &mut bytes)?;
                for field in &mut fields {
                    let name = field.name.to_ascii_lowercase();
                    if ["secret", "password", "token", "credential", "authorization"]
                        .iter()
                        .any(|word| name.contains(word))
                    {
                        field.value = "<REDACTED>".into();
                    }
                }
                empty(store.data().call(Operation::Log {
                    severity,
                    message,
                    fields,
                })?)
            })();
            Ok((result,))
        },
    )?;
    if !host.context.grants.prefixes.is_empty() {
        let mut storage = linker.instance("falcone:host/storage@0.1.0")?;
        storage_resource::<Object>(
            &mut storage,
            ["object", "open-object", "read-object", "write-object"],
            false,
        )?;
        storage_resource::<Checkpoint>(
            &mut storage,
            [
                "checkpoint",
                "open-checkpoint",
                "read-checkpoint",
                "write-checkpoint",
            ],
            true,
        )?;
    }
    if !host.context.grants.targets.is_empty() {
        let mut workflows = linker.instance("falcone:host/workflows@0.1.0")?;
        workflows.resource(
            "execution",
            ResourceType::host::<Execution>(),
            |mut store, rep| {
                store.data_mut().executions.remove(&rep);
                Ok(())
            },
        )?;
        workflows.func_wrap(
            "start",
            |mut store: StoreContextMut<'_, Host>, (workflow, input): (WasmStr, WasmList<u8>)| {
                let result: Result<Resource<Execution>, HostError> = (|| {
                    preflight(&mut store, &[&workflow], &[&input], None, 0)?;
                    let mut bytes = Bytes::default();
                    let workflow = string(&workflow, &store, &mut bytes)?;
                    let input = data(&input, &mut store, &mut bytes)?;
                    if !store
                        .data()
                        .context
                        .grants
                        .targets
                        .iter()
                        .any(|target| target == &workflow && !target.contains('*'))
                    {
                        return Err(HostError::Denied);
                    }
                    let Reply::Execution(execution) = store.data().call(Operation::Start {
                        workflow: workflow.clone(),
                        input,
                    })?
                    else {
                        return Err(HostError::Denied);
                    };
                    Bytes::default().add(execution.len())?;
                    let rep = store.data_mut().handle()?;
                    store
                        .data_mut()
                        .executions
                        .insert(rep, (workflow, execution));
                    Ok(Resource::new_own(rep))
                })();
                Ok((result,))
            },
        )?;
        workflows.func_wrap(
            "signal",
            |mut store: StoreContextMut<'_, Host>,
             (handle, name, payload): (Resource<Execution>, WasmStr, WasmList<u8>)| {
                let result: Result<(), HostError> = (|| {
                    preflight(&mut store, &[&name], &[&payload], None, 4)?;
                    let mut bytes = Bytes::default();
                    bytes.add(4)?;
                    let name = string(&name, &store, &mut bytes)?;
                    let payload = data(&payload, &mut store, &mut bytes)?;
                    let (workflow, execution) = store
                        .data()
                        .executions
                        .get(&handle.rep())
                        .ok_or(HostError::Denied)?;
                    if !store
                        .data()
                        .context
                        .grants
                        .targets
                        .iter()
                        .any(|target| target == workflow && !target.contains('*'))
                    {
                        return Err(HostError::Denied);
                    }
                    empty(store.data().call(Operation::Signal {
                        execution: execution.clone(),
                        name,
                        payload,
                    })?)
                })();
                Ok((result,))
            },
        )?;
    }
    if !host.context.grants.hosts.is_empty() {
        linker.instance("falcone:host/http@0.1.0")?.func_wrap(
            "request",
            |mut store: StoreContextMut<'_, Host>,
             (url, method, headers, body): (WasmStr, WasmStr, WasmList<LazyPair>, WasmList<u8>)| {
                let result: Result<super::net::Response, HostError> = (|| {
                    preflight(&mut store, &[&url, &method], &[&body], Some(&headers), 0)?;
                    let mut bytes = Bytes::default();
                    let url = string(&url, &store, &mut bytes)?;
                    let method = string(&method, &store, &mut bytes)?;
                    let headers = pairs(&headers, &mut store, &mut bytes)?;
                    let body = data(&body, &mut store, &mut bytes)?;
                    match store.data().call(Operation::Http(super::net::Request {
                        url, method, headers, body,
                    }))? {
                        Reply::Http(response) => Ok(response),
                        _ => Err(HostError::Denied),
                    }
                })();
                Ok((result,))
            },
        )?;
    }
    let mut store = Store::new(engine, host);
    store.limiter(|host| &mut host.limits);
    // Wasmtime charges lazy cursors as if eagerly lifted. Keep that finite
    // accounting bound tied to guest memory, while our 1 MiB checks below
    // reject before copying. This lets oversized inputs return the WIT error.
    store.set_hostcall_fuel(GUEST_MEMORY_BYTES * 16);
    let instance = linker.instantiate(&mut store, &component)?;
    Ok((store, instance))
}
