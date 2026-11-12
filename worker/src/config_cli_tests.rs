//! CLI configuration regressions kept at the worker binary's I/O seam.
use falcone_serverless_worker::config;
const DOC: &str = include_str!("../../docs/contracts/worker-runtime-config.md");
fn canonical_toml(doc: &str) -> Result<&str, String> {
    if doc.matches("```toml\n").count() != 1 || doc.matches("```").count() != 2 {
        return Err("expected exactly one fenced TOML example".into());
    }
    doc.split_once("```toml\n")
        .and_then(|(_, body)| body.split_once("\n```"))
        .map(|(toml, _)| toml)
        .ok_or_else(|| "missing closing TOML fence".into())
}

fn assert_documented_values(config: &config::RuntimeConfig) {
    assert_eq!(
        serde_json::to_value(config).unwrap(),
        serde_json::json!({
            "version": 1,
            "listen_port": 8080,
            "metrics_port": 9090,
            "probe_port": 8081,
            "startup_probe_path": "/startupz",
            "readiness_probe_path": "/readyz",
            "pool_id": "tenant-a-interactive",
            "concurrency": 1,
            "default_wall_clock_ms": 30000,
            "max_wall_clock_ms": 300000,
            "default_memory_bytes": 134217728,
            "max_memory_bytes": 536870912,
            "drain_ms": 30000,
            "cancel_reserve_ms": 5000,
            "checkpoint_reserve_ms": 10000,
            "kill_grace_ms": 5000,
            "cache_path": "/var/cache/falcone-worker",
            "egress": { "resolver": "/etc/resolv.conf", "allow_list": [] },
            "falcone_api_endpoint": "https://falcone.example.test/api",
            "falcone_dispatcher_endpoint": "https://dispatcher.example.test",
            "dispatcher_key_path": "/var/run/falcone/dispatcher-keys"
        })
    );
}

struct TempConfig(std::path::PathBuf);

impl TempConfig {
    fn new(text: &str) -> Self {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "falcone-config-{}-{}.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::File::create_new(&path).expect("create temporary config");
        let fixture = Self(path);
        file.write_all(text.as_bytes())
            .expect("write temporary config");
        fixture
    }

    fn args(&self) -> [std::ffi::OsString; 2] {
        ["--check-config".into(), self.0.as_os_str().to_owned()]
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).expect("remove temporary config");
    }
}

#[test]
fn check_config_returns_success_and_prints_effective_config() {
    let fixture = TempConfig::new(canonical_toml(DOC).unwrap());
    assert_eq!(crate::run(&fixture.args()), 0);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        crate::run_with_io(&fixture.args(), &mut stdout, &mut stderr),
        0
    );
    assert!(stderr.is_empty());
    let printed: config::RuntimeConfig = serde_json::from_slice(&stdout).expect("effective JSON");
    assert_documented_values(&printed);
}

#[test]
fn check_config_returns_failure_with_errors_on_stderr() {
    let fixture = TempConfig::new(&format!("unknown = true\n{}", canonical_toml(DOC).unwrap()));
    let mut missing = fixture.args();
    missing[1] = fixture.0.with_extension("missing").into_os_string();
    for (args, expected) in [(fixture.args(), "unknown"), (missing, "read config")] {
        assert_ne!(crate::run(&args), 0);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_ne!(crate::run_with_io(&args, &mut stdout, &mut stderr), 0);
        assert!(stdout.is_empty());
        let error = String::from_utf8(stderr).unwrap();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn check_config_requires_exactly_one_path() {
    for args in [
        vec!["--check-config".into()],
        vec!["--check-config".into(), "a".into(), "b".into()],
    ] {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_ne!(crate::run_with_io(&args, &mut stdout, &mut stderr), 0);
        assert!(stdout.is_empty());
        assert!(
            String::from_utf8(stderr)
                .unwrap()
                .contains("--check-config PATH")
        );
    }
}
