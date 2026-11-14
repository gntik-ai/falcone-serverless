//! Pod-local, process-owned trusted compile cache for validated components.
mod load;

#[cfg(test)]
pub use load::LoadEvent;
pub use load::Loaded;

use crate::artifact::Error;
use crate::artifact::cache_key::{CacheKey, ENGINE_CONFIG, ENGINE_VERSION, INTERFACE_VERSION};
use crate::artifact::supervisor::{CompileLimits, MAX_COMPILED_OUTPUT_BYTES, Supervisor};
use crate::artifact::validate::{Validated, validate};
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
    #[cfg(test)]
    events: std::sync::Mutex<Vec<LoadEvent>>,
    #[cfg(any(test, feature = "qualification"))]
    stage_hook: Option<StageHook>,
    #[cfg(any(test, feature = "qualification"))]
    validations: std::sync::atomic::AtomicUsize,
}

/// A step of the cache operation that a test or qualification build can pause or fail.
#[cfg(any(test, feature = "qualification"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Before the artifact is validated.
    Validate,
    /// Before the cache file is read.
    Read,
    /// Before the trusted compiler is started.
    Compile,
    /// After the temporary file is written, before it is synced and renamed.
    Publish,
}

#[cfg(any(test, feature = "qualification"))]
pub type StageHook = std::sync::Arc<dyn Fn(Stage) + Send + Sync>;

impl Cache {
    pub fn new(directory: impl Into<PathBuf>, child: impl Into<PathBuf>) -> Result<Self, Error> {
        let mac_key = hmac::Key::generate(hmac::HMAC_SHA256, &SystemRandom::new())
            .map_err(|_| error("trusted_cache_unavailable"))?;
        let directory = directory.into();
        fs::create_dir_all(&directory).map_err(|_| error("trusted_cache_unavailable"))?;
        Ok(Self {
            directory,
            mac_key,
            engine: load::engine()?,
            supervisor: Supervisor::new(child),
            #[cfg(test)]
            events: std::sync::Mutex::new(Vec::new()),
            #[cfg(any(test, feature = "qualification"))]
            stage_hook: None,
            #[cfg(any(test, feature = "qualification"))]
            validations: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Test and qualification builds only: pauses the operation at each `Stage`.
    #[cfg(any(test, feature = "qualification"))]
    pub fn set_stage_hook(&mut self, hook: StageHook) {
        self.stage_hook = Some(hook);
    }

    /// Test and qualification builds only: how many artifacts this cache has validated.
    #[cfg(any(test, feature = "qualification"))]
    pub fn validations(&self) -> usize {
        self.validations.load(Ordering::Acquire)
    }

    #[cfg(any(test, feature = "qualification"))]
    fn stage(&self, stage: Stage) {
        if let Some(hook) = &self.stage_hook {
            hook(stage);
        }
    }

    /// The directory the cache entries live in.
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
        std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn compile(
        &mut self,
        contract_version: u32,
        bytes: &[u8],
        limits: CompileLimits,
        cancelled: &AtomicBool,
    ) -> Result<Loaded, Error> {
        let mut child = None;
        let result = self.compile_inner(
            contract_version,
            bytes,
            limits,
            cancelled,
            &|_| Ok(()),
            &mut |reaped| child = Some(reaped),
        );
        if let Some(child) = child {
            self.supervisor.keep_child(child);
        }
        result
    }

    /// As `compile`, for concurrent executions sharing one cache. The artifact is validated
    /// exactly once; `admit` sees the validated component (its imports) before anything is
    /// compiled or loaded, and an error from it ends the operation. The whole operation
    /// (lookup, read, load, compile, publish) observes `cancelled` and the deadline in `limits`,
    /// and publishes only by atomic rename of a complete temporary file.
    pub fn compile_shared(
        &self,
        contract_version: u32,
        bytes: &[u8],
        limits: CompileLimits,
        cancelled: &AtomicBool,
        admit: &dyn Fn(&Validated<'_>) -> Result<(), Error>,
    ) -> Result<Loaded, Error> {
        self.compile_inner(
            contract_version,
            bytes,
            limits,
            cancelled,
            admit,
            &mut |_| {},
        )
    }

    fn compile_inner(
        &self,
        contract_version: u32,
        bytes: &[u8],
        limits: CompileLimits,
        cancelled: &AtomicBool,
        admit: &dyn Fn(&Validated<'_>) -> Result<(), Error>,
        keep: &mut dyn FnMut(std::process::Child),
    ) -> Result<Loaded, Error> {
        if contract_version != CONTRACT_VERSION {
            return Err(error("unsupported_feature"));
        }
        #[cfg(any(test, feature = "qualification"))]
        {
            self.validations.fetch_add(1, Ordering::AcqRel);
            self.stage(Stage::Validate);
        }
        let validated = validate(bytes, limits.input_bytes)?;
        active(limits, cancelled)?;
        admit(&validated)?;
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
        #[cfg(any(test, feature = "qualification"))]
        self.stage(Stage::Read);
        if let Some(loaded) = load::cached(self, &path, &key, &expected) {
            active(limits, cancelled)?;
            return Ok(loaded);
        }
        active(limits, cancelled)?;
        #[cfg(any(test, feature = "qualification"))]
        self.stage(Stage::Compile);
        let (compiled_key, output) = self
            .supervisor
            .compile_validated_keep(&validated, limits, cancelled, keep)?;
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
            .map_err(|_| error("trusted_cache_unavailable"))?;
        #[cfg(any(test, feature = "qualification"))]
        self.stage(Stage::Publish);
        // An abandoned operation never publishes: the temporary file is removed on drop and
        // the rename below is the only step that makes an entry visible.
        active(limits, cancelled)?;
        file.sync_all()
            .map_err(|_| error("trusted_cache_unavailable"))?;
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
