//! Falcone serverless execution worker.
//!
//! Placeholder binary so the workspace, CI and Hermes test commands exist before the
//! Wasmtime host lands (EP-RUN, #18). It embeds no engine and runs no guest code yet.

#[cfg_attr(not(test), allow(dead_code))]
mod checkpoint;

#[cfg(test)]
mod runtime_metrics;

/// The worker's build identity, reported by `--version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn main() {
    println!("falcone-serverless-worker {}", version());
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn version_is_the_crate_version() {
        assert_eq!(version(), "0.1.0");
    }
}
