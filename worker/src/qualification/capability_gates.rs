//! WAS-02.t4 capability-gate scenarios, shared by the in-process tests
//! (`tests/capability_gate_inprocess.rs`) and the pod qualification binary
//! (`capability-gate-qualify`). Each scenario drives WAT guests through the real gate with
//! mock bindings (the system boundary) and local listeners, and checks the outcome the guest
//! sees together with what the bindings and listeners observed.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use falcone_serverless_worker::compile_budget::{
    BudgetOutcome, CompileBudget, compile_within_budget,
};
use falcone_serverless_worker::host::HostError;
use falcone_serverless_worker::host::bindings::{
    BindingError, CallControl, EventBinding, LogSink, StorageBinding, WorkflowBinding,
};
use falcone_serverless_worker::host::egress::{Connector, Resolver, TcpConnector};
use falcone_serverless_worker::host::grant::GrantSet;
use falcone_serverless_worker::host::limits::HostLimits;
use falcone_serverless_worker::host::runtime::{
    Capabilities, Gate, GateError, Guest, InvocationContext,
};
use serde_json::{Value, json};

/// A stalled binding must return within its deadline plus this tolerance. Generous on purpose:
/// shared k3s nodes and CI runners schedule threads late.
pub const DEADLINE_TOLERANCE: Duration = Duration::from_millis(2000);

const STORAGE: &str = "falcone:host/storage@0.1.0";
const HTTP: &str = "falcone:host/http@0.1.0";
const EVENTS: &str = "falcone:host/events@0.1.0";
const WORKFLOWS: &str = "falcone:host/workflows@0.1.0";
const MAX: usize = 1_048_576;

/// A guest result that reports host-error `case` at `stage` (0x100 for the first call, 0x200
/// for the second; see the fixtures' header).
pub const fn host_error(stage: u32, case: HostError) -> u32 {
    0xE000_0000
        | stage
        | match case {
            HostError::Denied => 0,
            HostError::LimitExceeded => 1,
            HostError::Timeout => 2,
            HostError::Cancelled => 3,
        }
}

pub struct Fixtures {
    pub allowed: &'static str,
    pub heavy_function: &'static str,
    pub baseline_only: &'static str,
    pub imports_wasi_filesystem: &'static str,
    pub imports_wasi_sockets: &'static str,
    pub imports_wasi_environment: &'static str,
    pub ungranted_storage: &'static str,
    pub storage: &'static str,
    pub events: &'static str,
    pub workflows: &'static str,
    pub http: &'static str,
}

#[derive(Debug)]
pub enum Failure {
    /// The environment cannot run the scenario (no local sockets); never a verdict.
    NotRun(String),
    Failed(String),
}

pub type Verdict = Result<(), Failure>;

fn fail<T>(message: impl Into<String>) -> Result<T, Failure> {
    Err(Failure::Failed(message.into()))
}

fn ensure(condition: bool, message: impl FnOnce() -> String) -> Verdict {
    if condition { Ok(()) } else { fail(message()) }
}

fn equal<T: PartialEq + std::fmt::Debug>(actual: T, expected: T, what: &str) -> Verdict {
    ensure(actual == expected, || {
        format!("{what}: expected {expected:?}, got {actual:?}")
    })
}

// ---- mocks ------------------------------------------------------------------------------

#[derive(Default)]
pub struct MockLogs(Mutex<Vec<String>>);

impl MockLogs {
    pub fn messages(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl LogSink for MockLogs {
    fn log(&self, _level: &str, message: &str, _fields: &[(String, String)]) {
        self.0.lock().unwrap().push(message.to_owned());
    }
}

#[derive(Default)]
pub struct MockStorage {
    pub reads: AtomicUsize,
    pub writes: AtomicUsize,
    pub last_write_len: AtomicUsize,
    pub keys: Mutex<Vec<String>>,
    pub response: Vec<u8>,
    /// Stalls until `release` is set, ignoring its call control: the host must enforce the
    /// deadline and cancellation itself and never rely on the binding to stop.
    pub stall: bool,
    pub release: AtomicBool,
    pub stopped: AtomicBool,
}

impl MockStorage {
    pub fn calls(&self) -> usize {
        self.reads.load(Ordering::SeqCst) + self.writes.load(Ordering::SeqCst)
    }

    fn enter(&self, key: &str, control: &CallControl) -> Result<(), BindingError> {
        self.keys.lock().unwrap().push(key.to_owned());
        if self.stall {
            let _ = control;
            while !self.release.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(5));
            }
            self.stopped.store(true, Ordering::SeqCst);
            return Err(BindingError::Interrupted);
        }
        Ok(())
    }
}

impl StorageBinding for MockStorage {
    fn read(&self, key: &str, control: &CallControl) -> Result<Vec<u8>, BindingError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.enter(key, control)?;
        Ok(self.response.clone())
    }

    fn write(&self, key: &str, data: &[u8], control: &CallControl) -> Result<(), BindingError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.last_write_len.store(data.len(), Ordering::SeqCst);
        self.enter(key, control)
    }
}

#[derive(Default)]
pub struct MockEvents {
    pub publishes: AtomicUsize,
    pub topics: Mutex<Vec<String>>,
}

impl EventBinding for MockEvents {
    fn publish(
        &self,
        topic: &str,
        _payload: &[u8],
        _control: &CallControl,
    ) -> Result<(), BindingError> {
        self.publishes.fetch_add(1, Ordering::SeqCst);
        self.topics.lock().unwrap().push(topic.to_owned());
        Ok(())
    }
}

#[derive(Default)]
pub struct MockWorkflows {
    pub starts: AtomicUsize,
    pub signals: Mutex<Vec<(String, String, String)>>,
}

impl WorkflowBinding for MockWorkflows {
    fn start(
        &self,
        _target: &str,
        _input: &[u8],
        _control: &CallControl,
    ) -> Result<String, BindingError> {
        let n = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("exec-{n}"))
    }

    fn signal(
        &self,
        target: &str,
        execution: &str,
        name: &str,
        _payload: &[u8],
        _control: &CallControl,
    ) -> Result<(), BindingError> {
        self.signals.lock().unwrap().push((
            target.to_owned(),
            execution.to_owned(),
            name.to_owned(),
        ));
        Ok(())
    }
}

/// Answers per name from a queue; the last answer repeats. Records every lookup.
#[derive(Default)]
pub struct StubResolver {
    answers: Mutex<HashMap<String, VecDeque<Vec<IpAddr>>>>,
    pub lookups: Mutex<Vec<String>>,
}

impl StubResolver {
    pub fn with(name: &str, answers: &[&[&str]]) -> Self {
        let resolver = Self::default();
        resolver.add(name, answers);
        resolver
    }

    pub fn add(&self, name: &str, answers: &[&[&str]]) {
        let queue = answers
            .iter()
            .map(|answer| answer.iter().map(|ip| ip.parse().unwrap()).collect())
            .collect();
        self.answers.lock().unwrap().insert(name.to_owned(), queue);
    }

    pub fn lookup_count(&self) -> usize {
        self.lookups.lock().unwrap().len()
    }
}

impl Resolver for StubResolver {
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        self.lookups.lock().unwrap().push(host.to_owned());
        let mut answers = self.answers.lock().unwrap();
        let queue = answers
            .get_mut(host)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such name"))?;
        Ok(if queue.len() > 1 {
            queue.pop_front().unwrap()
        } else {
            queue.front().cloned().unwrap_or_default()
        })
    }
}

/// Maps validated addresses to local listeners (a test-network DNAT) and records every
/// address the gate asked to connect to. An unmapped address is refused.
#[derive(Default)]
pub struct MapConnector {
    routes: HashMap<SocketAddr, SocketAddr>,
    pub attempts: Mutex<Vec<SocketAddr>>,
}

impl MapConnector {
    pub fn route(mut self, from: &str, to: SocketAddr) -> Self {
        self.routes.insert(from.parse().unwrap(), to);
        self
    }

    pub fn attempted(&self) -> Vec<SocketAddr> {
        self.attempts.lock().unwrap().clone()
    }
}

impl Connector for MapConnector {
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        self.attempts.lock().unwrap().push(address);
        match self.routes.get(&address) {
            Some(real) => TcpStream::connect_timeout(real, timeout),
            None => Err(io::Error::new(io::ErrorKind::ConnectionRefused, "unrouted")),
        }
    }
}

// ---- local HTTP listeners ---------------------------------------------------------------

pub struct Server {
    pub addr: SocketAddr,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Server {
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn http_response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// Binds 127.0.0.1:0 and answers every request with `respond(request_head)`.
pub fn serve(respond: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Result<Server, Failure> {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(Failure::NotRun("EPERM (sandbox): no local sockets".into()));
        }
        Err(error) => return fail(format!("bind failed: {error}")),
    };
    let addr = listener
        .local_addr()
        .map_err(|e| Failure::Failed(e.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| Failure::Failed(e.to_string()))?;
    let accepted = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let respond = Arc::new(respond);
    let (counter, flag) = (Arc::clone(&accepted), Arc::clone(&stop));
    let join = thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let respond = Arc::clone(&respond);
                    thread::spawn(move || {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let mut head = Vec::new();
                        let mut chunk = [0u8; 1024];
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => head.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let _ = stream.write_all(&respond(&String::from_utf8_lossy(&head)));
                    });
                }
                Err(_) => thread::sleep(Duration::from_millis(2)),
            }
        }
    });
    Ok(Server {
        addr,
        accepted,
        stop,
        join: Some(join),
    })
}

// ---- helpers ----------------------------------------------------------------------------

fn wat_string(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/.-_:".contains(&b) {
                (b as char).to_string()
            } else {
                format!("\\{b:02x}")
            }
        })
        .collect()
}

fn fill(template: &str, pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .fold(template.to_owned(), |text, (key, value)| {
            text.replace(&format!("{{{{{key}}}}}"), value)
        })
}

fn grant(family: &str, interface: &str, field: &str, entries: &[String]) -> Value {
    json!({"family": family, "interface": interface, "scope": {field: entries}})
}

fn grants(list: Vec<Value>) -> Result<GrantSet, Failure> {
    GrantSet::from_manifest(&Value::Array(list)).map_err(|e| Failure::Failed(e.to_string()))
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn gate(grants: GrantSet, caps: Capabilities, limits: HostLimits) -> Result<Gate, Failure> {
    Gate::new(grants, caps, limits).map_err(|e| Failure::Failed(e.to_string()))
}

fn instantiate(gate: &Gate, wat: &str) -> Result<Result<Guest, GateError>, Failure> {
    let component = gate
        .compile_wat(wat)
        .map_err(|e| Failure::Failed(format!("fixture does not compile: {e}")))?;
    Ok(gate.instantiate(&component, InvocationContext::default()))
}

fn guest(gate: &Gate, wat: &str) -> Result<Guest, Failure> {
    match instantiate(gate, wat)? {
        Ok(guest) => Ok(guest),
        Err(error) => fail(format!("granted fixture failed to instantiate: {error}")),
    }
}

fn run(gate: &Gate, wat: &str) -> Result<u32, Failure> {
    guest(gate, wat)?
        .call_u32("run")
        .map_err(|e| Failure::Failed(e.to_string()))
}

fn storage_fixture(fx: &Fixtures, key: &str, op: u32, data_len: usize) -> String {
    fill(
        fx.storage,
        &[
            ("KEY", wat_string(key)),
            ("KEY_LEN", key.len().to_string()),
            ("OP", op.to_string()),
            ("DATA_LEN", data_len.to_string()),
        ],
    )
}

fn events_fixture(fx: &Fixtures, topic: &str, payload_len: usize) -> String {
    fill(
        fx.events,
        &[
            ("TOPIC", wat_string(topic)),
            ("TOPIC_LEN", topic.len().to_string()),
            ("PAYLOAD_LEN", payload_len.to_string()),
        ],
    )
}

fn workflows_fixture(fx: &Fixtures, target: &str, signal: u32) -> String {
    fill(
        fx.workflows,
        &[
            ("TARGET", wat_string(target)),
            ("TARGET_LEN", target.len().to_string()),
            ("SIGNAL", signal.to_string()),
        ],
    )
}

fn http_fixture(fx: &Fixtures, url: &str) -> String {
    fill(
        fx.http,
        &[("URL", wat_string(url)), ("URL_LEN", url.len().to_string())],
    )
}

// ---- scenarios --------------------------------------------------------------------------

/// ac1: WASI filesystem, sockets and environment imports fail instantiation, naming the
/// import, and the guest start function never runs, even with every grant present.
pub fn wasi_imports_are_denied(fx: &Fixtures) -> Verdict {
    let logs = Arc::new(MockLogs::default());
    let storage = Arc::new(MockStorage::default());
    let caps = Capabilities {
        storage: Some(storage.clone()),
        logs: logs.clone(),
        ..Capabilities::default()
    };
    let all = grants(vec![grant(
        "storage",
        STORAGE,
        "prefixes",
        &strings(&["p"]),
    )])?;
    let gate = gate(all, caps, HostLimits::default())?;
    let control = guest(&gate, fx.baseline_only)?;
    drop(control);
    equal(
        logs.messages(),
        vec!["start".to_owned()],
        "control start marker",
    )?;
    let before = logs.messages().len();
    for (wat, import) in [
        (fx.imports_wasi_filesystem, "wasi:filesystem/preopens@0.2.0"),
        (fx.imports_wasi_sockets, "wasi:sockets/tcp@0.2.0"),
        (fx.imports_wasi_environment, "wasi:cli/environment@0.2.0"),
    ] {
        match instantiate(&gate, wat)? {
            Err(GateError::UndeclaredImport(name)) => equal(name.as_str(), import, "named import")?,
            Err(other) => return fail(format!("{import}: unexpected error {other}")),
            Ok(_) => return fail(format!("{import}: instantiation succeeded")),
        }
        let message = format!("{}", GateError::UndeclaredImport(import.to_owned()));
        ensure(message.contains(import), || {
            "error does not name the import".into()
        })?;
    }
    equal(
        logs.messages().len(),
        before,
        "start markers after denied instantiations",
    )?;
    equal(storage.calls(), 0, "binding calls")
}

/// ac2: a declared falcone:host interface without a grant fails instantiation; the binding is
/// never called. With the grant the same fixture instantiates (the control).
pub fn ungranted_interface_is_denied(fx: &Fixtures) -> Verdict {
    let logs = Arc::new(MockLogs::default());
    let storage = Arc::new(MockStorage::default());
    let caps = || Capabilities {
        storage: Some(storage.clone()),
        logs: logs.clone(),
        ..Capabilities::default()
    };
    let denied = gate(grants(vec![])?, caps(), HostLimits::default())?;
    match instantiate(&denied, fx.ungranted_storage)? {
        Err(GateError::UngrantedImport(name)) => equal(name.as_str(), STORAGE, "named import")?,
        Err(other) => return fail(format!("unexpected error {other}")),
        Ok(_) => return fail("instantiation succeeded without a grant"),
    }
    equal(logs.messages().len(), 0, "start markers")?;
    equal(storage.calls(), 0, "binding calls")?;
    let granted = gate(
        grants(vec![grant(
            "storage",
            STORAGE,
            "prefixes",
            &strings(&["p"]),
        )])?,
        caps(),
        HostLimits::default(),
    )?;
    guest(&granted, fx.ungranted_storage)?;
    equal(
        logs.messages(),
        vec!["start".to_owned()],
        "control start marker",
    )
}

/// ac3: storage inside the granted prefix works; traversal, other workspaces and foreign
/// tenants are denied before the binding is called.
pub fn storage_is_scope_bound(fx: &Fixtures) -> Verdict {
    let storage = Arc::new(MockStorage {
        response: b"hello".to_vec(),
        ..MockStorage::default()
    });
    let caps = Capabilities {
        storage: Some(storage.clone()),
        ..Capabilities::default()
    };
    let scope = grants(vec![grant(
        "storage",
        STORAGE,
        "prefixes",
        &strings(&["tenants/t1/ws1"]),
    )])?;
    let gate = gate(scope, caps, HostLimits::default())?;
    equal(
        run(&gate, &storage_fixture(fx, "tenants/t1/ws1/obj", 0, 0))?,
        5,
        "granted read length",
    )?;
    equal(
        run(&gate, &storage_fixture(fx, "tenants/t1/ws1/obj", 1, 16))?,
        0,
        "granted write",
    )?;
    equal(storage.reads.load(Ordering::SeqCst), 1, "granted reads")?;
    equal(storage.writes.load(Ordering::SeqCst), 1, "granted writes")?;
    equal(
        storage.last_write_len.load(Ordering::SeqCst),
        16,
        "written bytes",
    )?;
    let denied = host_error(0x100, HostError::Denied);
    for key in [
        "tenants/t1/ws1/../ws2/obj",
        "tenants/t1/ws2/obj",
        "tenants/t2/ws1/obj",
        "tenants/t1/ws10/obj",
        "../tenants/t1/ws1/obj",
        "/tenants/t1/ws1/obj",
        "tenants/t1/ws1/%2e%2e/ws2/obj",
        "other",
    ] {
        for op in [0, 1] {
            equal(run(&gate, &storage_fixture(fx, key, op, 4))?, denied, key)?;
        }
    }
    equal(storage.calls(), 2, "binding calls after denied keys")
}

/// ac4: events and workflows are authorized from the grant only.
pub fn events_and_workflows_are_scope_bound(fx: &Fixtures) -> Verdict {
    let events = Arc::new(MockEvents::default());
    let workflows = Arc::new(MockWorkflows::default());
    let caps = Capabilities {
        events: Some(events.clone()),
        workflows: Some(workflows.clone()),
        ..Capabilities::default()
    };
    let scope = grants(vec![
        grant("event", EVENTS, "topics", &strings(&["orders.created"])),
        grant("workflow", WORKFLOWS, "targets", &strings(&["wf-ingest"])),
    ])?;
    let gate = gate(scope, caps, HostLimits::default())?;
    equal(
        run(&gate, &events_fixture(fx, "orders.created", 8))?,
        0,
        "granted publish",
    )?;
    equal(events.publishes.load(Ordering::SeqCst), 1, "publishes")?;
    for topic in ["orders.deleted", "orders.created.v2", "*"] {
        equal(
            run(&gate, &events_fixture(fx, topic, 8))?,
            host_error(0x100, HostError::Denied),
            topic,
        )?;
    }
    equal(
        events.publishes.load(Ordering::SeqCst),
        1,
        "publishes after denied topics",
    )?;
    equal(
        run(&gate, &workflows_fixture(fx, "wf-ingest", 1))?,
        0,
        "granted start and signal",
    )?;
    equal(workflows.starts.load(Ordering::SeqCst), 1, "starts")?;
    equal(
        workflows.signals.lock().unwrap().clone(),
        vec![("wf-ingest".to_owned(), "exec-1".to_owned(), "go".to_owned())],
        "signals",
    )?;
    equal(
        run(&gate, &workflows_fixture(fx, "wf-other", 1))?,
        host_error(0x100, HostError::Denied),
        "ungranted workflow start",
    )?;
    equal(
        workflows.starts.load(Ordering::SeqCst),
        1,
        "starts after denial",
    )?;
    // A signal handle the host never issued traps; it can never reach the binding.
    let mut forged = guest(&gate, &workflows_fixture(fx, "wf-ingest", 2))?;
    ensure(forged.call_u32("run").is_err(), || {
        "a forged handle did not trap".into()
    })?;
    equal(
        workflows.signals.lock().unwrap().len(),
        1,
        "signals after forged handle",
    )
}

/// ac5: HTTP is allow-listed; unlisted hosts, redirects to them and names resolving to
/// loopback, private or metadata addresses are denied without a connection.
pub fn http_is_allow_listed(fx: &Fixtures) -> Verdict {
    let allowed = serve(|_| http_response("200 OK", "", b"ok"))?;
    let forbidden = serve(|_| http_response("200 OK", "", b"bad"))?;
    let redirector = {
        let target = forbidden.port();
        serve(move |_| {
            http_response(
                "302 Found",
                &format!("Location: http://127.0.0.1:{target}/\r\n"),
                b"",
            )
        })?
    };
    let resolver = Arc::new(StubResolver::default());
    resolver.add("api.test", &[&["93.184.216.34"]]);
    resolver.add("loop.test", &[&["127.0.0.1"]]);
    resolver.add("priv.test", &[&["10.0.0.5"]]);
    resolver.add("meta.test", &[&["169.254.169.254"]]);
    resolver.add("mixed.test", &[&["93.184.216.34", "127.0.0.1"]]);
    let connector = Arc::new(
        MapConnector::default().route(&format!("93.184.216.34:{}", allowed.port()), allowed.addr),
    );
    let (a, f, r) = (allowed.port(), forbidden.port(), redirector.port());
    let entries = strings(&[
        &format!("127.0.0.1:{a}"),
        &format!("127.0.0.1:{r}"),
        &format!("api.test:{a}"),
        &format!("loop.test:{f}"),
        &format!("priv.test:{f}"),
        &format!("meta.test:{f}"),
        &format!("mixed.test:{f}"),
        "169.254.169.254",
    ]);
    let make = |connector: Arc<dyn Connector>| -> Result<Gate, Failure> {
        let caps = Capabilities {
            resolver: Some(resolver.clone()),
            connector,
            ..Capabilities::default()
        };
        gate(
            grants(vec![grant("http", HTTP, "hosts", &entries)])?,
            caps,
            HostLimits::default(),
        )
    };
    let direct = make(Arc::new(TcpConnector))?;
    let ok = (200 << 16) | 2;
    equal(
        run(
            &direct,
            &http_fixture(fx, &format!("http://127.0.0.1:{a}/")),
        )?,
        ok,
        "literal grant",
    )?;
    let routed = make(connector.clone())?;
    equal(
        run(
            &routed,
            &http_fixture(fx, &format!("http://api.test:{a}/x?y=1")),
        )?,
        ok,
        "allow-listed name",
    )?;
    equal(
        connector.attempted(),
        vec![format!("93.184.216.34:{a}").parse().unwrap()],
        "validated address only",
    )?;
    let denied = host_error(0x100, HostError::Denied);
    // Not on the list at all (the forbidden listener's own address).
    equal(
        run(
            &direct,
            &http_fixture(fx, &format!("http://127.0.0.1:{f}/")),
        )?,
        denied,
        "unlisted host",
    )?;
    equal(
        run(
            &direct,
            &http_fixture(fx, &format!("http://unlisted.test:{a}/")),
        )?,
        denied,
        "unlisted name",
    )?;
    // A listed host that redirects to an unlisted one.
    equal(
        run(
            &direct,
            &http_fixture(fx, &format!("http://127.0.0.1:{r}/")),
        )?,
        denied,
        "redirect to unlisted host",
    )?;
    equal(redirector.accepted(), 1, "redirector connections")?;
    for name in ["loop.test", "priv.test", "meta.test", "mixed.test"] {
        equal(
            run(&routed, &http_fixture(fx, &format!("http://{name}:{f}/")))?,
            denied,
            name,
        )?;
    }
    // The metadata address is unreachable even when a grant names it.
    equal(
        run(
            &direct,
            &http_fixture(fx, "http://169.254.169.254/latest/meta-data/"),
        )?,
        denied,
        "metadata literal",
    )?;
    equal(forbidden.accepted(), 0, "forbidden listener connections")?;
    equal(connector.attempted().len(), 1, "connector attempts")
}

/// ac6: a resolver that answers an allowed address and then a forbidden one cannot steer the
/// connection: the gate connects to the validated address only and re-validates every hop.
pub fn dns_rebinding_is_refused(fx: &Fixtures) -> Verdict {
    let forbidden = serve(|_| http_response("200 OK", "", b"bad"))?;
    let f = forbidden.port();
    let allowed = serve(move |_| {
        http_response(
            "302 Found",
            &format!("Location: http://rebind.test:{f}/next\r\n"),
            b"",
        )
    })?;
    let plain = serve(|_| http_response("200 OK", "", b"ok"))?;
    let build = |first_target: SocketAddr| -> Result<(Gate, Arc<StubResolver>, Arc<MapConnector>), Failure> {
        let resolver = Arc::new(StubResolver::with(
            "rebind.test",
            &[&["93.184.216.34"], &["127.0.0.1"]],
        ));
        let connector = Arc::new(MapConnector::default().route(&format!("93.184.216.34:{f}"), first_target));
        let caps = Capabilities {
            resolver: Some(resolver.clone()),
            connector: connector.clone(),
            ..Capabilities::default()
        };
        let scope = grants(vec![grant("http", HTTP, "hosts", &[format!("rebind.test:{f}")])])?;
        Ok((gate(scope, caps, HostLimits::default())?, resolver, connector))
    };
    // Across a redirect: the second resolution is forbidden, so the second hop is denied.
    let (redirecting, resolver, connector) = build(allowed.addr)?;
    equal(
        run(
            &redirecting,
            &http_fixture(fx, &format!("http://rebind.test:{f}/")),
        )?,
        host_error(0x100, HostError::Denied),
        "rebind across redirect",
    )?;
    equal(resolver.lookup_count(), 2, "resolutions")?;
    equal(
        connector.attempted(),
        vec![format!("93.184.216.34:{f}").parse().unwrap()],
        "connections",
    )?;
    // Without a redirect: one resolution, and the connection stays on the first address.
    let (single, resolver, connector) = build(plain.addr)?;
    equal(
        run(
            &single,
            &http_fixture(fx, &format!("http://rebind.test:{f}/")),
        )?,
        (200 << 16) | 2,
        "single request",
    )?;
    equal(resolver.lookup_count(), 1, "resolutions without redirect")?;
    equal(
        connector.attempted().len(),
        1,
        "connections without redirect",
    )?;
    equal(forbidden.accepted(), 0, "forbidden listener connections")
}

/// ac7: oversized payloads and responses are rejected, and a stalled binding returns
/// timeout or cancelled within its deadline plus `DEADLINE_TOLERANCE`.
pub fn limits_and_deadlines_hold(fx: &Fixtures) -> Verdict {
    let events = Arc::new(MockEvents::default());
    let storage = Arc::new(MockStorage::default());
    let caps = Capabilities {
        events: Some(events.clone()),
        storage: Some(storage.clone()),
        ..Capabilities::default()
    };
    let scope = || {
        grants(vec![grant(
            "storage",
            STORAGE,
            "prefixes",
            &strings(&["p"]),
        )])
    };
    let both = grants(vec![
        grant("event", EVENTS, "topics", &strings(&["t"])),
        grant("storage", STORAGE, "prefixes", &strings(&["p"])),
    ])?;
    let gate_default = gate(both, caps, HostLimits::default())?;
    equal(
        run(&gate_default, &events_fixture(fx, "t", MAX))?,
        host_error(0x100, HostError::LimitExceeded),
        "aggregate topic + payload over the ceiling",
    )?;
    equal(
        run(&gate_default, &events_fixture(fx, "t", MAX + 1))?,
        host_error(0x100, HostError::LimitExceeded),
        "oversized publish",
    )?;
    equal(
        events.publishes.load(Ordering::SeqCst),
        0,
        "publishes after oversized payloads",
    )?;
    equal(
        run(&gate_default, &storage_fixture(fx, "p/o", 1, MAX))?,
        0,
        "write at the ceiling",
    )?;
    equal(
        run(&gate_default, &storage_fixture(fx, "p/o", 1, MAX + 1))?,
        host_error(0x200, HostError::LimitExceeded),
        "oversized write",
    )?;
    equal(
        storage.writes.load(Ordering::SeqCst),
        1,
        "writes after oversized payload",
    )?;
    // Oversized binding responses are rejected; one at the ceiling passes.
    for (len, expected) in [
        (MAX, MAX as u32),
        (MAX + 1, host_error(0x200, HostError::LimitExceeded)),
    ] {
        let big = Arc::new(MockStorage {
            response: vec![7; len],
            ..MockStorage::default()
        });
        let caps = Capabilities {
            storage: Some(big),
            ..Capabilities::default()
        };
        let g = gate(scope()?, caps, HostLimits::default())?;
        equal(
            run(&g, &storage_fixture(fx, "p/o", 0, 0))?,
            expected,
            "binding response size",
        )?;
    }
    // A stalled binding hits the host-call deadline.
    let stalled = Arc::new(MockStorage {
        stall: true,
        ..MockStorage::default()
    });
    let stall_caps = Capabilities {
        storage: Some(stalled.clone()),
        ..Capabilities::default()
    };
    let deadline = Duration::from_millis(200);
    let g = gate(scope()?, stall_caps, HostLimits::new(MAX, MAX, deadline))?;
    let started = Instant::now();
    equal(
        run(&g, &storage_fixture(fx, "p/o", 0, 0))?,
        host_error(0x200, HostError::Timeout),
        "stalled binding",
    )?;
    ensure(started.elapsed() <= deadline + DEADLINE_TOLERANCE, || {
        format!("deadline overrun: {:?}", started.elapsed())
    })?;
    stalled.release.store(true, Ordering::SeqCst);
    wait_for(&stalled.stopped, "the released binding thread to finish")?;
    // Cancellation of the invocation is independent of the deadline.
    let cancelled = Arc::new(MockStorage {
        stall: true,
        ..MockStorage::default()
    });
    let caps = Capabilities {
        storage: Some(cancelled.clone()),
        ..Capabilities::default()
    };
    let g = gate(scope()?, caps, HostLimits::default())?;
    let mut guest = guest(&g, &storage_fixture(fx, "p/o", 0, 0))?;
    let token = guest.cancel_token();
    let delay = Duration::from_millis(100);
    let canceller = thread::spawn(move || {
        thread::sleep(delay);
        token.cancel();
    });
    let started = Instant::now();
    let result = guest
        .call_u32("run")
        .map_err(|e| Failure::Failed(e.to_string()))?;
    let _ = canceller.join();
    equal(
        result,
        host_error(0x200, HostError::Cancelled),
        "cancelled binding",
    )?;
    ensure(started.elapsed() <= delay + DEADLINE_TOLERANCE, || {
        format!("cancellation overrun: {:?}", started.elapsed())
    })?;
    cancelled.release.store(true, Ordering::SeqCst);
    wait_for(&cancelled.stopped, "the released binding thread to finish")?;
    oversized_http_response(fx)
}

fn oversized_http_response(fx: &Fixtures) -> Verdict {
    let server = serve(|_| http_response("200 OK", "", &vec![b'x'; MAX + 1]))?;
    let caps = Capabilities {
        resolver: Some(Arc::new(StubResolver::default())),
        ..Capabilities::default()
    };
    let scope = grants(vec![grant(
        "http",
        HTTP,
        "hosts",
        &[format!("127.0.0.1:{}", server.port())],
    )])?;
    let g = gate(scope, caps, HostLimits::default())?;
    equal(
        run(
            &g,
            &http_fixture(fx, &format!("http://127.0.0.1:{}/", server.port())),
        )?,
        host_error(0x100, HostError::LimitExceeded),
        "oversized http response",
    )
}

fn wait_for(flag: &AtomicBool, what: &str) -> Verdict {
    let started = Instant::now();
    while !flag.load(Ordering::SeqCst) {
        if started.elapsed() > DEADLINE_TOLERANCE {
            return fail(format!("timed out waiting for {what}"));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// ac8: an over-budget compile is killed with a limit error and leaves no output; a
/// within-budget compile succeeds.
pub fn compile_budget_is_enforced(fx: &Fixtures, child: &Path) -> Verdict {
    let small = wat::parse_str(fx.allowed).map_err(|e| Failure::Failed(e.to_string()))?;
    let body = "i32.const 3 i32.mul i32.const 1 i32.add\n".repeat(8192);
    let function = fx.heavy_function.replace(";; OPERATIONS", &body);
    let heavy = wat::parse_str(format!(
        "(component (core module {}))",
        function.repeat(128)
    ))
    .map_err(|e| Failure::Failed(e.to_string()))?;
    let budget = CompileBudget {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 20,
        memory_bytes: 2 * 1024 * 1024 * 1024,
        wall: Duration::from_secs(60),
    };
    let within = compile_within_budget(child, &small, budget);
    equal(within.outcome, BudgetOutcome::WithinBudget, "within budget")?;
    let cpu = CompileBudget {
        cpu_seconds: 1,
        ..budget
    };
    let over_cpu = compile_within_budget(child, &heavy, cpu);
    equal(
        over_cpu.outcome,
        BudgetOutcome::CpuLimitExceeded,
        "cpu budget",
    )?;
    ensure(over_cpu.child_reaped, || {
        "cpu-killed child was not reaped".into()
    })?;
    let memory = CompileBudget {
        memory_bytes: 64 * 1024 * 1024,
        ..budget
    };
    let over_memory = compile_within_budget(child, &heavy, memory);
    equal(
        over_memory.outcome,
        BudgetOutcome::MemoryLimitExceeded,
        "memory budget",
    )?;
    ensure(over_memory.child_reaped, || {
        "memory-killed child was not reaped".into()
    })?;
    equal(
        compile_within_budget(child, &small, memory).outcome,
        BudgetOutcome::WithinBudget,
        "recovery after a kill",
    )
}
