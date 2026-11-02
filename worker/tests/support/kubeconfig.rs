//! The explicit cluster credential boundary: `KUBECONFIG` is split here, never by kube or
//! kubectl. There is no fallback to `~/.kube/config` or the in-cluster configuration; a list
//! that is empty, has an empty entry, names a file that cannot be read or holds an invalid
//! kubeconfig is UNVERIFIED before any API call, kubectl call or mutation.
//!
//! Every listed file is made absolute, read with `Kubeconfig::read_from` and merged once, first
//! file wins. Absolute paths matter: `read_from` resolves certificate, key and token-file
//! references against the file's directory, and kubectl reads the merge from another
//! directory, so a relative entry would make the two consumers resolve them differently.
//! The merge must then pass `Config::from_custom_kubeconfig`, so a readable file that selects
//! no usable cluster is UNVERIFIED here, before any API call or kubectl call. The kube client
//! is built from that merge and kubectl is pointed at a private file that holds the very same
//! merge, so both harnesses always select the same cluster and identity. File contents are
//! never printed or logged.

use kube::Config;
use kube::config::{KubeConfigOptions, Kubeconfig};
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The merged configuration on disk for kubectl: a private directory and a 0600 file that
/// are removed when the last `Kubeconfigs` clone is dropped.
struct MergedFile {
    dir: PathBuf,
    file: PathBuf,
}

impl Drop for MergedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

fn random_name() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let counter = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let random = RandomState::new().hash_one((std::process::id(), counter, nanos));
    format!("fs-test-kubeconfig-{random:016x}")
}

fn write_merged(config: &Kubeconfig) -> Result<MergedFile, String> {
    const FAILED: &str = "UNVERIFIED: cannot prepare the merged kubeconfig for kubectl";
    let dir = std::env::temp_dir().join(random_name());
    DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|_| FAILED.to_owned())?;
    let merged = MergedFile {
        file: dir.join("config"),
        dir,
    };
    let bytes = serde_json::to_vec(config).map_err(|_| FAILED.to_owned())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&merged.file)
        .map_err(|_| FAILED.to_owned())?;
    file.write_all(&bytes).map_err(|_| FAILED.to_owned())?;
    Ok(merged)
}

/// Checks the merge with the very call the client is built from. It runs on its own thread
/// and runtime so it works inside and outside an async context, and it reads local files only.
fn validate(config: &Kubeconfig) -> Result<(), String> {
    const UNUSABLE: &str = "UNVERIFIED: KUBECONFIG does not describe a usable cluster";
    let config = config.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| UNUSABLE.to_owned())?
            .block_on(Config::from_custom_kubeconfig(
                config,
                &KubeConfigOptions::default(),
            ))
            .map(drop)
            .map_err(|_| UNUSABLE.to_owned())
    })
    .join()
    .map_err(|_| UNUSABLE.to_owned())?
}

#[derive(Clone)]
pub struct Kubeconfigs {
    paths: Vec<PathBuf>,
    merged: Arc<Kubeconfig>,
    on_disk: Arc<Mutex<Option<MergedFile>>>,
}

impl std::fmt::Debug for Kubeconfigs {
    /// Paths only: the merged contents carry credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kubeconfigs")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl Kubeconfigs {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        let value = value
            .filter(|value| !value.trim().is_empty())
            .ok_or("UNVERIFIED: KUBECONFIG must list the kubeconfig files to use")?;
        let mut paths = Vec::new();
        let mut merged: Option<Kubeconfig> = None;
        for (index, entry) in value.split(':').enumerate() {
            let number = index + 1;
            if entry.trim().is_empty() {
                return Err(format!(
                    "UNVERIFIED: KUBECONFIG entry {number} is empty; ambient kubeconfig fallbacks are never used"
                ));
            }
            let path = std::path::absolute(Path::new(entry)).map_err(|_| {
                format!("UNVERIFIED: KUBECONFIG entry {number} is not a readable file")
            })?;
            if !path.is_file() || File::open(&path).is_err() {
                return Err(format!(
                    "UNVERIFIED: KUBECONFIG entry {number} is not a readable file"
                ));
            }
            let file = Kubeconfig::read_from(&path).map_err(|_| {
                format!("UNVERIFIED: KUBECONFIG entry {number} is not a valid kubeconfig")
            })?;
            merged = Some(match merged {
                None => file,
                Some(first) => first.merge(file).map_err(|_| {
                    format!("UNVERIFIED: KUBECONFIG entry {number} cannot be merged")
                })?,
            });
            paths.push(path);
        }
        let merged = merged.ok_or("UNVERIFIED: KUBECONFIG lists no files")?;
        validate(&merged)?;
        Ok(Self {
            paths,
            merged: Arc::new(merged),
            on_disk: Arc::default(),
        })
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// The listed files, merged with the first file winning, exactly as kubectl would.
    pub fn merged(&self) -> Kubeconfig {
        (*self.merged).clone()
    }

    /// kubectl takes a single `--kubeconfig`, and repeating the flag would select only the
    /// last file. So kubectl is given the one file that holds the merge the kube client uses,
    /// and it can never pick up a kubeconfig of its own accord.
    pub fn kubectl_flags(&self) -> Result<Vec<String>, String> {
        let mut on_disk = self
            .on_disk
            .lock()
            .map_err(|_| "UNVERIFIED: cannot prepare the merged kubeconfig for kubectl")?;
        if on_disk.is_none() {
            *on_disk = Some(write_merged(&self.merged())?);
        }
        let file = on_disk
            .as_ref()
            .map_or_else(PathBuf::new, |m| m.file.clone());
        Ok(vec![format!("--kubeconfig={}", file.display())])
    }
}
