//! The platform boundary of the execution API: where an execution's artifact comes from and
//! where its host capabilities are bound.
//!
//! This is the host binding layer. The execution credential reaches it, and only it: it is
//! never part of the guest's input, environment, files or memory, and it is not in any
//! value the guest can read back.

use crate::api::Plan;
use crate::host::{self, Bindings, Context, HostError, Operation, Reply};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// The largest artifact the worker accepts: the compiler's input bound.
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

/// Why the platform could not supply what an execution needs. The reason is deliberately
/// coarse: nothing about the credential or the request is carried in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unavailable;

/// Supplies an execution's artifact and binds its capabilities. Both calls receive the
/// execution's absolute deadline and cancellation flag and should observe them; the server
/// does not rely on that, because it stops waiting at the deadline or on cancellation either
/// way, and a call that ignores both only loses its own result.
pub trait Platform: Send + Sync + 'static {
    /// The raw artifact the plan's `artifact_digest` names.
    fn artifact(
        &self,
        plan: &Plan,
        until: Instant,
        cancel: &AtomicBool,
    ) -> Result<Arc<[u8]>, Unavailable>;

    /// The capability bindings of one execution. They hold the credential and are the only
    /// place that does.
    fn bind(
        &self,
        plan: &Plan,
        until: Instant,
        cancel: &AtomicBool,
    ) -> Result<Arc<dyn Bindings>, Unavailable>;
}

/// The production platform until the dispatcher and Falcone define the remote capability
/// contract: artifacts come from a read-only pod-local directory, by digest, and the host
/// grants nothing remote. Storage, HTTP, events and workflows are denied; logging and
/// progress are accepted and dropped. The credential is not used and not kept.
pub struct LocalPlatform {
    directory: PathBuf,
}

impl LocalPlatform {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
}

struct Local;

impl Bindings for Local {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        match operation {
            Operation::Log { .. } | Operation::Progress { .. } => Ok(Reply::Empty),
            _ => Err(HostError::Denied),
        }
    }
}

fn hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Platform for LocalPlatform {
    fn artifact(&self, plan: &Plan, _: Instant, _: &AtomicBool) -> Result<Arc<[u8]>, Unavailable> {
        let path = self
            .directory
            .join(format!("{}.wasm", hex(&plan.artifact_digest)));
        // No symlink is followed, and a FIFO cannot block the open.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(path)
            .map_err(|_| Unavailable)?;
        if !file.metadata().is_ok_and(|meta| meta.is_file()) {
            return Err(Unavailable);
        }
        let mut bytes = Vec::new();
        file.take(MAX_ARTIFACT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Unavailable)?;
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(Unavailable);
        }
        Ok(bytes.into())
    }

    fn bind(&self, _: &Plan, _: Instant, _: &AtomicBool) -> Result<Arc<dyn Bindings>, Unavailable> {
        Ok(Arc::new(Local))
    }
}

/// Collects what the guest logs, bounded, in front of the platform's own bindings.
pub(super) struct Collecting {
    pub inner: Arc<dyn Bindings>,
    pub lines: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Bindings for Collecting {
    fn call(&self, context: &Context, operation: Operation) -> Result<Reply, HostError> {
        if let Operation::Log {
            severity,
            message,
            fields,
        } = &operation
        {
            let mut line = format!("{severity:?}: {message}");
            for field in fields {
                line.push_str(&format!(" {}={}", field.name, field.value));
            }
            let mut lines = self
                .lines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if lines.len() < super::redact::MAX_LINES {
                // Cut at capture: a guest cannot make the buffer larger than the result.
                let cut = line
                    .char_indices()
                    .nth(super::redact::MAX_LINE_CHARS)
                    .map_or(line.len(), |(end, _)| end);
                line.truncate(cut);
                lines.push(line);
            }
        }
        self.inner.call(context, operation)
    }

    fn resolve(&self, name: &str) -> Result<Vec<std::net::IpAddr>, HostError> {
        self.inner.resolve(name)
    }

    fn http(
        &self,
        context: &Context,
        request: &host::net::PinnedRequest,
    ) -> Result<host::net::Response, HostError> {
        self.inner.http(context, request)
    }
}
