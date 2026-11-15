use std::io::{self, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Resource, Rlimit, prlimit};

use super::Error;
use super::cache_key::{CacheKey, ENGINE_CONFIG, ENGINE_VERSION, INTERFACE_VERSION};
use super::validate::{Validated, validate};

/// Version 3 bounds native compiler output independently of raw input limits.
pub const MAX_COMPILED_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// All budgets are mandatory; numeric production ceilings belong to the profile.
#[derive(Clone, Copy)]
pub struct CompileLimits {
    pub input_bytes: usize,
    pub cpu_seconds: u64,
    /// RLIMIT_AS: virtual address space, not resident memory.
    pub memory_bytes: u64,
    pub wall_clock_deadline: Instant,
}

/// Why the supervisor stopped a child. It is recorded before the kill, so the outcome follows
/// the reason and never the status or signal the killed child then reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stop {
    Running = 0,
    Deadline = 1,
    Cancelled = 2,
}

impl Stop {
    fn code(self) -> Option<&'static str> {
        match self {
            Self::Running => None,
            Self::Deadline => Some("compile_deadline_exceeded"),
            Self::Cancelled => Some("compile_cancelled"),
        }
    }
}

/// The stop reason of one supervised child, set at most once before any kill.
struct StopReason(AtomicU8);

impl StopReason {
    fn new() -> Self {
        Self(AtomicU8::new(Stop::Running as u8))
    }

    fn get(&self) -> Stop {
        match self.0.load(Ordering::Acquire) {
            1 => Stop::Deadline,
            2 => Stop::Cancelled,
            _ => Stop::Running,
        }
    }

    /// Records the first reason; cancellation outranks the deadline when both hold.
    fn observe(&self, deadline: Instant, cancelled: &AtomicBool) -> Stop {
        let observed = if cancelled.load(Ordering::Acquire) {
            Stop::Cancelled
        } else if Instant::now() >= deadline {
            Stop::Deadline
        } else {
            return self.get();
        };
        let _ = self.0.compare_exchange(
            Stop::Running as u8,
            observed as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.get()
    }
}

pub struct Supervisor {
    child_path: PathBuf,
    spawned: Arc<AtomicUsize>,
    last_child: Option<Child>,
}

impl Supervisor {
    /// The executable is a trusted platform binary, never a tenant-supplied path.
    pub fn new(child_path: impl Into<PathBuf>) -> Self {
        Self {
            child_path: child_path.into(),
            spawned: Arc::new(AtomicUsize::new(0)),
            last_child: None,
        }
    }

    pub fn spawn_count(&self) -> usize {
        self.spawned.load(Ordering::Acquire)
    }

    pub fn spawn_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.spawned)
    }

    /// Uses Child::try_wait so callers can observe the reap guarantee.
    pub fn last_child_status(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.last_child.as_mut() {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }

    pub fn compile(
        &mut self,
        bytes: &[u8],
        limits: CompileLimits,
        cancelled: &AtomicBool,
    ) -> Result<CacheKey, Error> {
        self.compile_output(bytes, limits, cancelled)
            .map(|(key, _)| key)
    }

    /// Captures the trusted child's bounded stdout; callers must verify it before use.
    /// The legacy compile-only API also permits empty stdout for status-only harnesses.
    pub fn compile_output(
        &mut self,
        bytes: &[u8],
        limits: CompileLimits,
        cancelled: &AtomicBool,
    ) -> Result<(CacheKey, Vec<u8>), Error> {
        let validated = validate(bytes, limits.input_bytes)?;
        self.compile_validated(&validated, limits, cancelled)
    }

    /// As `compile_output` for bytes the caller has already validated, so an execution
    /// validates each artifact exactly once.
    pub fn compile_validated(
        &mut self,
        validated: &Validated<'_>,
        limits: CompileLimits,
        cancelled: &AtomicBool,
    ) -> Result<(CacheKey, Vec<u8>), Error> {
        if limits.input_bytes == 0
            || limits.cpu_seconds == 0
            || limits.cpu_seconds == u64::MAX
            || limits.memory_bytes == 0
            || limits.memory_bytes == u64::MAX
        {
            return Err(Error::new("compile_failed"));
        }
        let key = CacheKey::new(
            validated.artifact_digest,
            ENGINE_VERSION,
            std::env::consts::ARCH,
            ENGINE_CONFIG,
            INTERFACE_VERSION,
        )?;
        let stop = StopReason::new();
        let child = spawn(
            &self.child_path,
            limits.wall_clock_deadline,
            cancelled,
            &stop,
        )?;
        self.last_child = Some(child);
        self.spawned.fetch_add(1, Ordering::Release);
        let child = self.last_child.as_mut().expect("just spawned child");
        // Before this returns, even setup and I/O failures terminate and reap.
        let result = run(child, validated.bytes(), limits, cancelled, &stop);
        let cleanup = terminate_and_reap(child);
        // A child the supervisor stopped is reported by the reason it was stopped, even when
        // reaping it failed or the child reported some other status on its way out.
        if let Some(code) = stop.get().code() {
            return Err(Error::new(code));
        }
        cleanup?;
        Ok((key, result?))
    }
}

/// Starts the trusted child. `ETXTBSY` is the transient refusal to execute a file that another
/// thread's fork still holds open for writing; it is retried until the deadline or a cancel.
fn spawn(
    path: &std::path::Path,
    deadline: Instant,
    cancelled: &AtomicBool,
    stop: &StopReason,
) -> Result<Child, Error> {
    loop {
        match Command::new(path)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(error) if error.raw_os_error() == Some(26) => {
                if let Some(code) = stop.observe(deadline, cancelled).code() {
                    return Err(Error::new(code));
                }
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return Err(Error::new("compile_failed")),
        }
    }
}

fn apply_limits(child: &Child, limits: CompileLimits) -> Result<(), Error> {
    let pid = Some(Pid::from_child(child));
    for (resource, value) in [
        (Resource::Core, 0),
        (Resource::Cpu, limits.cpu_seconds),
        (Resource::As, limits.memory_bytes),
    ] {
        let limit = Rlimit {
            current: Some(value),
            maximum: Some(value),
        };
        prlimit(pid, resource, limit).map_err(|_| Error::new("compile_failed"))?;
        // Read back by applying the same limit; fail closed if it was not set.
        let applied = prlimit(pid, resource, limit).map_err(|_| Error::new("compile_failed"))?;
        if applied != limit {
            return Err(Error::new("compile_failed"));
        }
    }
    Ok(())
}

fn run(
    child: &mut Child,
    bytes: &[u8],
    limits: CompileLimits,
    cancelled: &AtomicBool,
    stop: &StopReason,
) -> Result<Vec<u8>, Error> {
    apply_limits(child, limits)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::new("compile_failed"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::new("compile_failed"))?;
    // Poll both pipes with the child. Even an inherited pipe handle cannot strand
    // a reader/writer join after deadline or cancellation terminates the child.
    rustix::fs::fcntl_setfl(&stdin, rustix::fs::OFlags::NONBLOCK)
        .and_then(|()| rustix::fs::fcntl_setfl(&stdout, rustix::fs::OFlags::NONBLOCK))
        .map_err(|_| Error::new("compile_failed"))?;
    monitor(
        child,
        stdin,
        stdout,
        bytes,
        limits.wall_clock_deadline,
        cancelled,
        stop,
    )
}

fn monitor(
    child: &mut Child,
    stdin: ChildStdin,
    mut stdout: ChildStdout,
    bytes: &[u8],
    deadline: Instant,
    cancelled: &AtomicBool,
    stop: &StopReason,
) -> Result<Vec<u8>, Error> {
    let mut stdin = Some(stdin);
    let mut written = 0;
    let mut output = Vec::new();
    let mut buffer = [0; 64 * 1024];
    let mut eof = false;
    let mut exited = false;
    loop {
        // The reason is recorded here, before any kill or reap.
        if let Some(code) = stop.observe(deadline, cancelled).code() {
            return Err(Error::new(code));
        }
        let mut progressed = false;
        if !eof {
            match stdout.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(count) => {
                    if output.len() + count > MAX_COMPILED_OUTPUT_BYTES {
                        return Err(Error::new("compile_failed"));
                    }
                    output.extend_from_slice(&buffer[..count]);
                    progressed = true;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(Error::new("compile_failed")),
            }
        }
        if let Some(pipe) = &mut stdin {
            match pipe.write(&bytes[written..]) {
                Ok(0) => {
                    stdin.take();
                }
                Ok(count) => {
                    written += count;
                    progressed = true;
                    if written == bytes.len() {
                        // EOF is the child's barrier before compilation.
                        stdin.take();
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                    // A child may hit a limit before consuming all input. Close
                    // the pipe and let its exit status retain that diagnostic.
                    stdin.take();
                }
                Err(_) => return Err(Error::new("compile_failed")),
            }
        }
        if !exited
            && let Some(status) = child.try_wait().map_err(|_| Error::new("compile_failed"))?
        {
            // A child that exits abnormally after the deadline or a cancel was stopped by
            // that reason as far as the supervisor can tell: report the reason, not the exit.
            if status.code() != Some(0)
                && let Some(code) = stop.observe(deadline, cancelled).code()
            {
                return Err(Error::new(code));
            }
            match (status.code(), status.signal()) {
                (Some(0), None) => exited = true,
                (_, Some(24 | 9)) => return Err(Error::new("compile_cpu_limit_exceeded")),
                (Some(66), None) | (_, Some(6)) => {
                    return Err(Error::new("compile_memory_limit_exceeded"));
                }
                _ => return Err(Error::new("compile_failed")),
            }
        }
        if exited {
            if written != bytes.len() {
                return Err(Error::new("compile_failed"));
            }
            if eof {
                return Ok(output);
            }
        }
        if !progressed {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

fn terminate_and_reap(child: &mut Child) -> Result<(), Error> {
    // kill may report an already-exited child; wait is still mandatory.
    let _ = child.kill();
    child.wait().map_err(|_| Error::new("compile_failed"))?;
    Ok(())
}
