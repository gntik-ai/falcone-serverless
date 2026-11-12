//! The pod-local HTTP service of the worker execution API (ctr-worker-execution-api v1).
//!
//! `POST /v1/execute`, `POST /v1/update` and `POST /v1/cancel`, one request per connection. The
//! service drives the existing engine, cache and host harness: it compiles only through the
//! trusted compile child, never loads a tenant precompiled artifact, and keeps the execution
//! credential in the host binding layer (`binding`). Capacity is split in `admission`: execute
//! requests are bounded by `concurrency`, a fixed reserve of control slots serves cancel and
//! update, and the connection limit is the sum of the two.
//!
//! Authentication of the dispatcher is not part of this service (INV-01); the pool runs only
//! behind the network policy of its deployment.

mod admission;
mod artifacts;
mod binding;
mod execute;
mod http;
mod registry;

pub use artifacts::{Artifacts, DirArtifacts, MAX_ARTIFACT_BYTES};
pub use http::{
    BODY_DEADLINE, HEADER_DEADLINE, HttpLimits, IO_TIMEOUT, MAX_CONTROL_BODY, MAX_HEADERS,
    MAX_REQUEST,
};

use crate::cache::Cache;
use admission::{Admission, Connection};
use execute::Reply;
use http::{Reject, Route};
use registry::Registry;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Control slots reserved for cancel and update, never used by execute requests.
pub const CONTROL_RESERVE: u32 = 4;

/// How long a cancel waits for the guest to stop before it answers `uncertain`.
pub const CANCEL_DEADLINE: Duration = Duration::from_secs(5);

pub struct Config {
    /// Execute requests that run at once.
    pub concurrency: u32,
    /// Control slots beyond `concurrency`; the connection limit is the sum.
    pub control_reserve: u32,
    pub http: HttpLimits,
    pub cancel_deadline: Duration,
    compiler: PathBuf,
    cache: PathBuf,
    artifacts: Arc<dyn Artifacts>,
}

impl Config {
    /// `compiler` is the trusted `artifact-compile-child` executable; `cache` the pod-local
    /// directory of the trusted compile cache.
    pub fn new(compiler: PathBuf, cache: PathBuf, artifacts: Arc<dyn Artifacts>) -> Self {
        Self {
            concurrency: 1,
            control_reserve: CONTROL_RESERVE,
            http: HttpLimits::default(),
            cancel_deadline: CANCEL_DEADLINE,
            compiler,
            cache,
            artifacts,
        }
    }
}

/// What every connection of a service shares.
pub struct Shared {
    admission: Arc<Admission>,
    http: HttpLimits,
    cache: Mutex<Cache>,
    artifacts: Arc<dyn Artifacts>,
    registry: Arc<Registry>,
    cancel_deadline: Duration,
    instances: AtomicUsize,
}

pub struct Server {
    listener: TcpListener,
    shared: Arc<Shared>,
}

impl Server {
    pub fn bind(address: SocketAddr, config: Config) -> io::Result<Self> {
        let cache = Cache::new(config.cache, config.compiler)
            .map_err(|_| io::Error::other("trusted_cache_unavailable"))?;
        let listener = TcpListener::bind(address)?;
        Ok(Self {
            listener,
            shared: Arc::new(Shared {
                admission: Admission::new(config.concurrency, config.control_reserve),
                http: config.http,
                cache: Mutex::new(cache),
                artifacts: config.artifacts,
                registry: Arc::new(Registry::default()),
                cancel_deadline: config.cancel_deadline,
                instances: AtomicUsize::new(0),
            }),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves until the process ends.
    pub fn serve(self) -> io::Result<()> {
        accept_loop(&self.listener, &self.shared, &AtomicBool::new(false));
        Ok(())
    }

    /// Serves on a background thread until the returned handle is dropped.
    pub fn spawn(self) -> Running {
        let address = self
            .listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 0)));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = self.shared.clone();
        let thread = std::thread::spawn({
            let stop = stop.clone();
            move || accept_loop(&self.listener, &self.shared, &stop)
        });
        Running {
            address,
            shared,
            stop,
            thread: Some(thread),
        }
    }
}

/// A service running on a background thread. Dropping it stops the accept loop and asks every
/// execution in flight to stop.
pub struct Running {
    address: SocketAddr,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    pub fn addr(&self) -> SocketAddr {
        self.address
    }

    /// `(connections, executes, controls)` held right now. All zero when nothing is in flight:
    /// every slot is released when its request ends, however it ends.
    pub fn slots(&self) -> (usize, usize, usize) {
        self.shared.admission.slots()
    }

    /// The limit on open connections: `concurrency` plus the control reserve.
    pub fn connection_limit(&self) -> usize {
        self.shared.admission.connection_limit()
    }

    /// Guest instantiations attempted since the service started.
    pub fn instances(&self) -> usize {
        self.shared.instances.load(Ordering::SeqCst)
    }

    /// Compiles started (cache misses) since the service started.
    pub fn compiles(&self) -> usize {
        self.shared.compiles()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.shared.registry.cancel_all();
        // Wake the accept call so the loop sees the flag.
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>, stop: &AtomicBool) {
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        // At the connection limit the connection is closed before a byte is read.
        let Some(connection) = shared.admission.connect() else {
            continue;
        };
        let shared = shared.clone();
        let _ = std::thread::Builder::new()
            .name("execution-api".into())
            .spawn(move || serve_connection(&shared, stream, connection));
    }
}

fn serve_connection(shared: &Shared, mut stream: TcpStream, mut connection: Connection) {
    let _ = stream.set_nodelay(true);
    let limits = &shared.http;
    let head = match http::read_head(&mut stream, limits) {
        Ok(head) => head,
        Err(reject) => return refuse(&mut stream, reject, limits),
    };
    // The slot is claimed before the body is read, so a request over capacity is answered at
    // once and holds nothing. Execute never claims a control slot.
    let route = head.route;
    let admitted = match route {
        Route::Execute => connection.take_execute(),
        Route::Update | Route::Cancel => connection.take_control(),
    };
    if !admitted {
        return send(&mut stream, &Reply::busy(), limits, true);
    }
    let body = match http::read_body(&mut stream, head, limits) {
        Ok(body) => body,
        Err(reject) => return refuse(&mut stream, reject, limits),
    };
    let reply = match route {
        Route::Execute => execute::execute(shared, &body),
        Route::Cancel => execute::cancel(shared, &body),
        Route::Update => execute::update(shared, &body),
    };
    send(&mut stream, &reply, limits, false);
}

fn refuse(stream: &mut TcpStream, reject: Reject, limits: &HttpLimits) {
    match reject {
        Reject::Close => {}
        Reject::Status(status) => send(stream, &Reply::status(status), limits, true),
        Reject::TooLarge => send(stream, &Reply::too_large(), limits, true),
    }
}

fn send(stream: &mut TcpStream, reply: &Reply, limits: &HttpLimits, early: bool) {
    if http::respond(stream, reply.status, &reply.body, limits).is_ok() && early {
        http::close(stream);
    }
}
