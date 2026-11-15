//! Pod-local, process-owned trusted compile cache for validated components.
mod load;

#[cfg(test)]
pub use load::LoadEvent;
pub use load::Loaded;

use crate::artifact::Error;
use crate::artifact::cache_key::{CacheKey, ENGINE_CONFIG, ENGINE_VERSION, INTERFACE_VERSION};
use crate::artifact::supervisor::{CompileLimits, MAX_COMPILED_OUTPUT_BYTES, Supervisor};
use crate::artifact::validate::validate;
use ring::{hmac, rand::SecureRandom, rand::SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub const CONTRACT_VERSION: u32 = 3;
const MAX_MANIFEST_BYTES: usize = 16 * 1024;
const MAX_ENTRY_BYTES: usize = 4 + MAX_MANIFEST_BYTES + MAX_COMPILED_OUTPUT_BYTES;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Provenance {
    engine_version: String,
    config_hash: [u8; 32],
    module_hash: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    key: String,
    bytes_sha256: [u8; 32],
    provenance: Provenance,
    mac: Vec<u8>,
}

impl Manifest {
    fn message(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(&(
            "falcone-native-cache-v3",
            &self.key,
            self.bytes_sha256,
            &self.provenance,
        ))
        .map_err(|_| error("trusted_cache_unavailable"))
    }
}

fn error(code: &'static str) -> Error {
    Error { code, import: None }
}

fn active(limits: CompileLimits, cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Acquire) {
        return Err(error("compile_cancelled"));
    }
    if Instant::now() >= limits.wall_clock_deadline {
        return Err(error("compile_deadline_exceeded"));
    }
    Ok(())
}

/// Construct once at process startup. Paths come from the trusted embedding code,
/// never from a tenant request. Dropping this owner loses all cache authority.
pub struct Cache {
    directory: PathBuf,
    mac_key: hmac::Key,
    engine: load::Engine,
    supervisor: Supervisor,
    child: PathBuf,
    #[cfg(any(test, feature = "qualification-hooks"))]
    hooks: std::sync::Arc<hooks::Hooks>,
    #[cfg(test)]
    events: std::cell::RefCell<Vec<LoadEvent>>,
}

/// Test-only stall points and counters. They exist only under `cfg(test)` or the
/// `qualification-hooks` feature and are absent from every release build.
#[cfg(any(test, feature = "qualification-hooks"))]
pub mod hooks {
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    /// A point of a cache operation at which a hook may stall.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Stage {
        Validate,
        Read,
        Compile,
        Publish,
    }

    type Callback = Arc<dyn Fn(Stage) + Send + Sync>;

    #[derive(Default)]
    pub struct Hooks {
        callback: Mutex<Option<Callback>>,
        /// How many times an artifact was validated.
        pub validations: AtomicUsize,
    }

    impl Hooks {
        pub fn set(&self, callback: impl Fn(Stage) + Send + Sync + 'static) {
            *self
                .callback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(callback));
        }

        pub(super) fn reach(&self, stage: Stage) {
            let callback = self
                .callback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(callback) = callback {
                callback(stage);
            }
        }
    }
}

macro_rules! stage {
    ($cache:expr, $stage:ident) => {
        #[cfg(any(test, feature = "qualification-hooks"))]
        $cache.hooks.reach(hooks::Stage::$stage);
    };
}

impl Cache {
    pub fn new(directory: impl Into<PathBuf>, child: impl Into<PathBuf>) -> Result<Self, Error> {
        let mac_key = hmac::Key::generate(hmac::HMAC_SHA256, &SystemRandom::new())
            .map_err(|_| error("trusted_cache_unavailable"))?;
        let directory = directory.into();
        let child = child.into();
        fs::create_dir_all(&directory).map_err(|_| error("trusted_cache_unavailable"))?;
        Ok(Self {
            directory,
            mac_key,
            engine: load::engine()?,
            supervisor: Supervisor::new(child.clone()),
            child,
            #[cfg(any(test, feature = "qualification-hooks"))]
            hooks: std::sync::Arc::default(),
            #[cfg(test)]
            events: std::cell::RefCell::new(Vec::new()),
        })
    }

    /// A handle on the same cache directory, MAC key and engine with its own supervisor, so
    /// concurrent executions never share a lock and an abandoned operation blocks no one.
    pub fn fork(&self) -> Self {
        Self {
            directory: self.directory.clone(),
            mac_key: self.mac_key.clone(),
            engine: self.engine.clone(),
            supervisor: Supervisor::new(self.child.clone()),
            child: self.child.clone(),
            #[cfg(any(test, feature = "qualification-hooks"))]
            hooks: self.hooks.clone(),
            #[cfg(test)]
            events: std::cell::RefCell::new(Vec::new()),
        }
    }

    #[cfg(any(test, feature = "qualification-hooks"))]
    pub fn hooks(&self) -> std::sync::Arc<hooks::Hooks> {
        self.hooks.clone()
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn spawn_count(&self) -> usize {
        self.supervisor.spawn_count()
    }

    #[cfg(test)]
    pub fn spawn_counter(&self) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        self.supervisor.spawn_counter()
    }

    pub fn last_child_status(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.supervisor.last_child_status()
    }

    #[cfg(test)]
    pub fn take_events(&self) -> Vec<LoadEvent> {
        self.events.take()
    }

    pub fn compile(
        &mut self,
        contract_version: u32,
        bytes: &[u8],
        limits: CompileLimits,
        cancelled: &AtomicBool,
    ) -> Result<Loaded, Error> {
        self.compile_declared(contract_version, bytes, limits, cancelled, None)
    }

    /// As `compile`, and every import of the component must also appear in `declared` (the
    /// revision projection's `imports`; empty means none). Validation runs once here.
    pub fn compile_declared(
        &mut self,
        contract_version: u32,
        bytes: &[u8],
        limits: CompileLimits,
        cancelled: &AtomicBool,
        declared: Option<&[String]>,
    ) -> Result<Loaded, Error> {
        if contract_version != CONTRACT_VERSION {
            return Err(error("unsupported_feature"));
        }
        stage!(self, Validate);
        #[cfg(any(test, feature = "qualification-hooks"))]
        self.hooks
            .validations
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let validated = validate(bytes, limits.input_bytes)?;
        if let Some(declared) = declared
            && let Some(import) = validated
                .imports
                .iter()
                .find(|import| !declared.contains(import))
        {
            return Err(Error {
                code: "undeclared_import",
                import: Some(import.chars().take(256).collect()),
            });
        }
        let key = CacheKey::new(
            validated.artifact_digest,
            ENGINE_VERSION,
            std::env::consts::ARCH,
            ENGINE_CONFIG,
            INTERFACE_VERSION,
        )?;
        let expected = Provenance {
            engine_version: ENGINE_VERSION.to_owned(),
            config_hash: Sha256::digest(ENGINE_CONFIG.as_bytes()).into(),
            module_hash: validated.artifact_digest,
        };
        active(limits, cancelled)?;
        let path = self.directory.join(format!("{}.cache", key.as_str()));
        if let Some(loaded) = load::cached(self, &path, &key, &expected) {
            active(limits, cancelled)?;
            return Ok(loaded);
        }
        stage!(self, Compile);
        active(limits, cancelled)?;
        let (compiled_key, output) = self
            .supervisor
            .compile_validated(&validated, limits, cancelled)?;
        active(limits, cancelled)?;
        if compiled_key != key || output.is_empty() || output.len() > MAX_COMPILED_OUTPUT_BYTES {
            return Err(error("compile_failed"));
        }
        // Only this successful child-output path can mint a manifest or a MAC.
        let mut manifest = Manifest {
            key: key.as_str().to_owned(),
            bytes_sha256: Sha256::digest(&output).into(),
            provenance: expected.clone(),
            mac: Vec::new(),
        };
        manifest.mac = hmac::sign(&self.mac_key, &manifest.message()?)
            .as_ref()
            .to_vec();
        let header =
            serde_json::to_vec(&manifest).map_err(|_| error("trusted_cache_unavailable"))?;
        if header.len() > MAX_MANIFEST_BYTES {
            return Err(error("trusted_cache_unavailable"));
        }
        let mut entry = Vec::with_capacity(4 + header.len() + output.len());
        entry.extend_from_slice(&(header.len() as u32).to_be_bytes());
        entry.extend_from_slice(&header);
        entry.extend_from_slice(&output);
        let loaded = load::verified(self, &entry, &key, &expected, false)
            .map_err(|_| error("trusted_cache_unavailable"))?;
        self.publish(&path, &entry, limits, cancelled)?;
        Ok(loaded)
    }

    fn publish(
        &self,
        path: &Path,
        entry: &[u8],
        limits: CompileLimits,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        // A PID and restarting counter can collide with crash remnants in a
        // surviving pod-local directory. Never adopt or remove another owner's file.
        let mut nonce = [0; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| error("trusted_cache_unavailable"))?;
        let id = u128::from_be_bytes(nonce);
        let temp = self
            .directory
            .join(format!(".{}-{id:032x}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|_| error("trusted_cache_unavailable"))?;
        let staged = Staged(temp);
        file.write_all(entry)
            .and_then(|()| file.sync_all())
            .map_err(|_| error("trusted_cache_unavailable"))?;
        stage!(self, Publish);
        active(limits, cancelled)?;
        fs::rename(&staged.0, path).map_err(|_| error("trusted_cache_unavailable"))?;
        Ok(())
    }
}

struct Staged(PathBuf);

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Version 3's externally visible outcome table; both success paths have no diagnostic.
pub fn verdict(result: &Result<Loaded, Error>) -> (&'static str, Option<&'static str>) {
    match result {
        Ok(_) => ("accepted", None),
        Err(error) => ("rejected", Some(error.code)),
    }
}
