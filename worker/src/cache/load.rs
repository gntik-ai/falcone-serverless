//! Audited native load boundary (ADR 0013). Every check precedes the single load.
#![allow(unsafe_code)]

use super::{Cache, MAX_ENTRY_BYTES, MAX_MANIFEST_BYTES, Manifest, Provenance};
use crate::artifact::Error;
use crate::artifact::cache_key::CacheKey;
use crate::artifact::supervisor::MAX_COMPILED_OUTPUT_BYTES;
use ring::hmac;
use rustix::fs::OFlags;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use wasmtime as engine;

pub(super) type Engine = engine::Engine;

pub struct Loaded {
    pub key: String,
    pub cache_hit: bool,
    pub component: engine::component::Component,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("key", &self.key)
            .field("cache_hit", &self.cache_hit)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub enum LoadEvent {
    Read,
    Rejected(&'static str),
    MacVerified(&'static str),
    Deserialize(&'static str),
}

macro_rules! witness {
    ($cache:expr, $event:expr) => {
        #[cfg(test)]
        $cache.events.borrow_mut().push($event);
    };
}

pub(super) fn engine() -> Result<Engine, Error> {
    let mut config = engine::Config::new();
    // Must match the trusted child and ENGINE_CONFIG; all other settings are pinned defaults.
    config.consume_fuel(true).epoch_interruption(true);
    Engine::new(&config).map_err(|_| super::error("trusted_cache_unavailable"))
}

fn read(path: &Path) -> Result<Vec<u8>, &'static str> {
    // NOFOLLOW rejects symlinks; NONBLOCK prevents a FIFO from blocking before fstat.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(|_| "unavailable")?;
    let metadata = file.metadata().map_err(|_| "unavailable")?;
    if !metadata.is_file() || metadata.len() > MAX_ENTRY_BYTES as u64 {
        return Err("not_bounded_regular_file");
    }
    let mut bytes = Vec::new();
    file.take(MAX_ENTRY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "unavailable")?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err("not_bounded_regular_file");
    }
    Ok(bytes)
}

pub(super) fn cached(
    cache: &Cache,
    path: &Path,
    key: &CacheKey,
    expected: &Provenance,
) -> Option<Loaded> {
    witness!(cache, LoadEvent::Read);
    match read(path).and_then(|bytes| verified(cache, &bytes, key, expected, true)) {
        Ok(loaded) => Some(loaded),
        Err(reason) => {
            let _ = reason;
            witness!(cache, LoadEvent::Rejected(reason));
            None
        }
    }
}

pub(super) fn verified(
    cache: &Cache,
    entry: &[u8],
    key: &CacheKey,
    expected: &Provenance,
    cache_hit: bool,
) -> Result<Loaded, &'static str> {
    if entry.len() > MAX_ENTRY_BYTES {
        return Err("manifest");
    }
    let length = u32::from_be_bytes(
        entry
            .get(..4)
            .ok_or("manifest")?
            .try_into()
            .map_err(|_| "manifest")?,
    ) as usize;
    if length > MAX_MANIFEST_BYTES {
        return Err("manifest");
    }
    let manifest: Manifest = serde_json::from_slice(entry.get(4..4 + length).ok_or("manifest")?)
        .map_err(|_| "manifest")?;
    let bytes = entry.get(4 + length..).ok_or("manifest")?;
    if bytes.is_empty() || bytes.len() > MAX_COMPILED_OUTPUT_BYTES {
        return Err("output_size");
    }
    if manifest.key != key.as_str() {
        return Err("key");
    }
    if manifest.provenance != *expected {
        return Err("provenance");
    }
    if manifest.bytes_sha256 != <[u8; 32]>::from(Sha256::digest(bytes)) {
        return Err("integrity");
    }
    let message = manifest.message().map_err(|_| "manifest")?;
    hmac::verify(&cache.mac_key, &message, &manifest.mac).map_err(|_| "mac")?;
    witness!(
        cache,
        LoadEvent::MacVerified(if cache_hit {
            "cache_file"
        } else {
            "fresh_harness_output"
        })
    );
    witness!(
        cache,
        LoadEvent::Deserialize(if cache_hit {
            "cache_file"
        } else {
            "fresh_harness_output"
        })
    );
    // SAFETY: only this process's successful trusted compiler output can have this
    // MAC. Key, engine/config/module provenance and integrity match this request.
    // The verified in-memory bytes are exactly those passed here; no path is reopened.
    #[allow(clippy::disallowed_methods)]
    let component = unsafe { engine::component::Component::deserialize(&cache.engine, bytes) }
        .map_err(|_| "engine")?;
    Ok(Loaded {
        key: key.as_str().to_owned(),
        cache_hit,
        component,
    })
}
