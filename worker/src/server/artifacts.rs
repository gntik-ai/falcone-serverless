//! Where the component of a revision comes from. The service never reads a path or URL from a
//! request: the artifact store is a trusted boundary that answers for a digest.

use std::io::Read;
use std::path::PathBuf;

/// The largest component the worker accepts.
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

pub trait Artifacts: Send + Sync + 'static {
    /// The raw component whose digest is `digest` (`sha256:` and 64 lowercase hex digits), or
    /// `None` when the store has none. The caller checks the bytes against the digest.
    fn component(&self, digest: &str) -> Option<Vec<u8>>;
}

/// A directory of `<64 hex digits>.wasm` files, mounted read-only in the pod.
pub struct DirArtifacts(pub PathBuf);

impl Artifacts for DirArtifacts {
    fn component(&self, digest: &str) -> Option<Vec<u8>> {
        let hex = digest.strip_prefix("sha256:")?;
        // The digest was validated by the API, and is checked again: no separator or dot can
        // reach the path.
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(self.0.join(format!("{hex}.wasm")))
            .ok()?
            .take(MAX_ARTIFACT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    }
}
