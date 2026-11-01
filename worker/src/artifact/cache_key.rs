use sha2::{Digest, Sha256};

use super::Error;

pub const ENGINE_VERSION: &str = "49.0.5";
pub const ENGINE_CONFIG: &str = "49.0.5-defaults;consume_fuel=true;epoch_interruption=true";
pub const INTERFACE_VERSION: &str = "falcone:host@0.1.0";

/// In-memory identity only; a matching key establishes no cache provenance.
#[derive(Debug, PartialEq, Eq)]
pub struct CacheKey(String);

impl CacheKey {
    pub fn new(
        artifact_digest: [u8; 32],
        engine_version: &str,
        target_architecture: &str,
        engine_config: &str,
        interface_version: &str,
    ) -> Result<Self, Error> {
        let parts = [
            engine_version,
            target_architecture,
            engine_config,
            interface_version,
        ];
        if parts.iter().any(|part| part.is_empty()) {
            return Err(Error::new("cache_key_mismatch"));
        }
        let mut hash = Sha256::new();
        hash.update(artifact_digest);
        for part in parts {
            hash.update((part.len() as u64).to_be_bytes());
            hash.update(part.as_bytes());
        }
        let digest: String = artifact_digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self(format!("sha256:{digest}:{:x}", hash.finalize())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
