//! The explicit cluster credential boundary: `KUBECONFIG` is split here, never by kube or
//! kubectl. There is no fallback to `~/.kube/config` or the in-cluster configuration; a list
//! that is empty, has an empty entry or names a file that cannot be read is UNVERIFIED before
//! any API call, kubectl call or mutation. File contents are never read, printed or logged.

use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kubeconfigs(Vec<PathBuf>);

impl Kubeconfigs {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        let value = value
            .filter(|value| !value.trim().is_empty())
            .ok_or("UNVERIFIED: KUBECONFIG must list the kubeconfig files to use")?;
        let mut paths = Vec::new();
        for (index, entry) in value.split(':').enumerate() {
            if entry.trim().is_empty() {
                return Err(format!(
                    "UNVERIFIED: KUBECONFIG entry {} is empty; ambient kubeconfig fallbacks are never used",
                    index + 1
                ));
            }
            let path = Path::new(entry);
            if !path.is_file() || File::open(path).is_err() {
                return Err(format!(
                    "UNVERIFIED: KUBECONFIG entry {} is not a readable file",
                    index + 1
                ));
            }
            paths.push(path.to_path_buf());
        }
        Ok(Self(paths))
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.0
    }

    /// One `--kubeconfig` flag per listed file, so a kubectl call can never pick up a
    /// kubeconfig of its own accord.
    pub fn kubectl_flags(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|path| format!("--kubeconfig={}", path.display()))
            .collect()
    }
}
