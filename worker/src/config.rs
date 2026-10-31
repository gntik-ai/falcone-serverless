//! Versioned worker runtime configuration; parsing has no runtime side effects.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default = "default_u32::<1>")]
    pub version: u32,
    #[serde(default = "default_u16::<8080>")]
    pub listen_port: u16,
    #[serde(default = "default_u16::<9090>")]
    pub metrics_port: u16,
    #[serde(default = "default_u16::<8081>")]
    pub probe_port: u16,
    #[serde(default = "startup_probe_path")]
    pub startup_probe_path: String,
    #[serde(default = "readiness_probe_path")]
    pub readiness_probe_path: String,
    pub pool_id: String,
    #[serde(default = "default_u32::<1>")]
    pub concurrency: u32,
    #[serde(default = "default_u64::<30000>")]
    pub default_wall_clock_ms: u64,
    #[serde(default = "default_u64::<300000>")]
    pub max_wall_clock_ms: u64,
    #[serde(default = "default_u64::<134217728>")]
    pub default_memory_bytes: u64,
    #[serde(default = "default_u64::<536870912>")]
    pub max_memory_bytes: u64,
    #[serde(default = "default_u64::<30000>")]
    pub drain_ms: u64,
    #[serde(default = "default_u64::<5000>")]
    pub cancel_reserve_ms: u64,
    #[serde(default = "default_u64::<10000>")]
    pub checkpoint_reserve_ms: u64,
    #[serde(default = "default_u64::<5000>")]
    pub kill_grace_ms: u64,
    #[serde(default = "cache_path")]
    pub cache_path: String,
    #[serde(default)]
    pub egress: EgressConfig,
    pub falcone_api_endpoint: String,
    pub falcone_dispatcher_endpoint: String,
    #[serde(default = "dispatcher_key_path")]
    pub dispatcher_key_path: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EgressConfig {
    #[serde(default = "resolver_path")]
    pub resolver: String,
    #[serde(default)]
    pub allow_list: Vec<String>,
}

impl Default for EgressConfig {
    fn default() -> Self {
        Self {
            resolver: resolver_path(),
            allow_list: Vec::new(),
        }
    }
}

fn default_u16<const N: u16>() -> u16 {
    N
}
fn default_u32<const N: u32>() -> u32 {
    N
}
fn default_u64<const N: u64>() -> u64 {
    N
}
fn startup_probe_path() -> String {
    "/startupz".into()
}
fn readiness_probe_path() -> String {
    "/readyz".into()
}
fn cache_path() -> String {
    "/var/cache/falcone-worker".into()
}
fn dispatcher_key_path() -> String {
    "/var/run/falcone/dispatcher-keys".into()
}
fn resolver_path() -> String {
    "/etc/resolv.conf".into()
}

/// Parse only: no key/cache/resolver files are opened and no endpoints are contacted.
pub fn load_from_str(input: &str) -> Result<RuntimeConfig, String> {
    let config: RuntimeConfig =
        toml::from_str(input).map_err(|error: toml::de::Error| error.message().to_owned())?;
    if config.version != 1 {
        return Err("version: only v1 (version = 1) is supported".into());
    }
    for (field, value) in [
        ("pool_id", &config.pool_id),
        ("falcone_api_endpoint", &config.falcone_api_endpoint),
        (
            "falcone_dispatcher_endpoint",
            &config.falcone_dispatcher_endpoint,
        ),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{field}: must not be empty"));
        }
    }
    if config.default_wall_clock_ms > config.max_wall_clock_ms {
        return Err("default_wall_clock_ms must not exceed max_wall_clock_ms".into());
    }
    if config.default_memory_bytes > config.max_memory_bytes {
        return Err("default_memory_bytes must not exceed max_memory_bytes".into());
    }
    let reserves = config
        .cancel_reserve_ms
        .checked_add(config.checkpoint_reserve_ms)
        .and_then(|total| total.checked_add(config.kill_grace_ms));
    if reserves.is_none_or(|total| total > config.drain_ms) {
        return Err(
            "cancel_reserve_ms + checkpoint_reserve_ms + kill_grace_ms must fit in drain_ms".into(),
        );
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    const DOC: &str = include_str!("../../docs/contracts/worker-runtime-config.md");
    const FIELDS: [[&str; 5]; 22] = [
        ["version", "u32", "none", "1", "false"],
        ["listen_port", "u16", "port", "8080", "false"],
        ["metrics_port", "u16", "port", "9090", "false"],
        ["probe_port", "u16", "port", "8081", "false"],
        ["startup_probe_path", "string", "path", "/startupz", "false"],
        ["readiness_probe_path", "string", "path", "/readyz", "false"],
        ["pool_id", "string", "none", "none", "true"],
        ["concurrency", "u32", "invocations/replica", "1", "false"],
        ["default_wall_clock_ms", "u64", "ms", "30000", "false"],
        ["max_wall_clock_ms", "u64", "ms", "300000", "false"],
        ["default_memory_bytes", "u64", "bytes", "134217728", "false"],
        ["max_memory_bytes", "u64", "bytes", "536870912", "false"],
        ["drain_ms", "u64", "ms", "30000", "false"],
        ["cancel_reserve_ms", "u64", "ms", "5000", "false"],
        ["checkpoint_reserve_ms", "u64", "ms", "10000", "false"],
        ["kill_grace_ms", "u64", "ms", "5000", "false"],
        [
            "cache_path",
            "string",
            "path",
            "/var/cache/falcone-worker",
            "false",
        ],
        [
            "egress.resolver",
            "string",
            "path",
            "/etc/resolv.conf",
            "false",
        ],
        [
            "egress.allow_list",
            "array<string>",
            "URL origin",
            "[]",
            "false",
        ],
        ["falcone_api_endpoint", "string", "URL", "none", "true"],
        [
            "falcone_dispatcher_endpoint",
            "string",
            "URL",
            "none",
            "true",
        ],
        [
            "dispatcher_key_path",
            "string",
            "path",
            "/var/run/falcone/dispatcher-keys",
            "false",
        ],
    ];
    const POSTURE: [[&str; 2]; 7] = [
        ["readOnlyRootFilesystem", "true"],
        ["runAsNonRoot", "true"],
        ["allowPrivilegeEscalation", "false"],
        ["capabilities.drop", "ALL"],
        ["seccompProfile", "RuntimeDefault"],
        ["automountServiceAccountToken", "false"],
        ["standingFalconeCredential", "none"],
    ];

    fn section<'a>(doc: &'a str, heading: &str) -> Result<&'a str, String> {
        let marker = format!("## {heading}\n");
        let (_, body) = doc
            .split_once(&marker)
            .ok_or_else(|| format!("missing {heading}"))?;
        Ok(body.split("\n## ").next().unwrap_or(body))
    }

    fn table<'a>(
        text: &'a str,
        header: &[&str],
    ) -> Result<BTreeMap<&'a str, Vec<&'a str>>, String> {
        let mut lines = text.lines().filter(|line| line.starts_with('|'));
        let cells = |line: &'a str| -> Vec<&'a str> {
            line.trim_matches('|').split('|').map(str::trim).collect()
        };
        if lines.next().map(cells).as_deref() != Some(header) {
            return Err(format!("missing table header {header:?}"));
        }
        let separator = lines.next().map(cells).ok_or("missing table separator")?;
        if separator.len() != header.len() || separator.iter().any(|cell| *cell != "---") {
            return Err("invalid table separator".into());
        }
        let mut rows = BTreeMap::new();
        for line in lines {
            let row = cells(line);
            if row.len() != header.len() || row[0].is_empty() {
                return Err(format!("invalid table row: {line}"));
            }
            if rows.insert(row[0], row).is_some() {
                return Err(format!("duplicate table row: {line}"));
            }
        }
        Ok(rows)
    }

    fn check_rows<const N: usize>(
        rows: BTreeMap<&str, Vec<&str>>,
        expected: &[[&str; N]],
    ) -> Result<(), String> {
        for row in expected {
            if rows.get(row[0]).map(Vec::as_slice) != Some(row.as_slice()) {
                return Err(format!("{}: missing or changed v1 row", row[0]));
            }
        }
        if rows.len() != expected.len() {
            return Err("unexpected v1 row".into());
        }
        Ok(())
    }

    fn check_contract(doc: &str) -> Result<(), String> {
        if !doc.lines().any(|line| line == "Contract version: v1") {
            return Err("missing Contract version: v1".into());
        }
        check_rows(
            table(
                section(doc, "Fields")?,
                &["name", "type", "unit", "default", "required"],
            )?,
            &FIELDS,
        )?;
        check_rows(
            table(
                section(doc, "Restricted pod posture")?,
                &["requirement", "required value"],
            )?,
            &POSTURE,
        )
    }

    fn row(doc: &str, name: &str) -> String {
        doc.lines()
            .find(|line| line.starts_with(&format!("| {name} |")))
            .expect("fixture row")
            .to_owned()
    }

    fn canonical_toml(doc: &str) -> Result<&str, String> {
        if doc.matches("```toml\n").count() != 1 || doc.matches("```").count() != 2 {
            return Err("expected exactly one fenced TOML example".into());
        }
        doc.split_once("```toml\n")
            .and_then(|(_, body)| body.split_once("\n```"))
            .map(|(toml, _)| toml)
            .ok_or_else(|| "missing closing TOML fence".into())
    }

    fn assert_documented_values(config: &super::RuntimeConfig) {
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

    const MINIMAL: &str = r#"
        pool_id = "tenant-a-interactive"
        falcone_api_endpoint = "https://falcone.example.test/api"
        falcone_dispatcher_endpoint = "https://dispatcher.example.test"
    "#;

    #[test]
    fn rejects_missing_or_blank_identity_and_endpoints() {
        for field in [
            "pool_id",
            "falcone_api_endpoint",
            "falcone_dispatcher_endpoint",
        ] {
            let line = MINIMAL
                .lines()
                .find(|line| line.trim().starts_with(field))
                .unwrap();
            for replacement in [String::new(), format!("{field} = \"   \"")] {
                let fixture = MINIMAL.replace(line, &replacement);
                let error = super::load_from_str(&fixture).unwrap_err();
                assert!(error.contains(field), "{error}");
            }
        }
    }

    #[test]
    fn rejects_unknown_keys_credential_values_and_destination_check_switches() {
        for key in [
            "unknown",
            "dispatcher_token",
            "block_private",
            "block_link_local",
            "block_metadata",
            "redirect_rechecks",
            "dns_pinning",
        ] {
            for table in ["", "[egress]\n"] {
                let fixture = format!("{MINIMAL}\n{table}{key} = false");
                let error = super::load_from_str(&fixture).unwrap_err();
                assert!(error.contains(key), "{error}");
            }
        }
        let error =
            super::load_from_str(&format!("dispatcher_token = \"fixture-token\"\n{MINIMAL}"))
                .unwrap_err();
        assert!(error.contains("dispatcher_token"));
        assert!(!error.contains("fixture-token"));
    }

    #[test]
    fn default_budgets_must_not_exceed_their_maximums() {
        for (default, maximum, limit) in [
            ("default_wall_clock_ms", "max_wall_clock_ms", 30000),
            ("default_memory_bytes", "max_memory_bytes", 134217728),
        ] {
            let equal = format!("{MINIMAL}\n{maximum} = {limit}");
            super::load_from_str(&equal).expect("default equal to maximum");
            for fixture in [
                format!("{MINIMAL}\n{maximum} = {}", limit - 1),
                format!("{equal}\n{default} = {}", limit + 1),
            ] {
                let error = super::load_from_str(&fixture).unwrap_err();
                assert!(
                    error.contains(default) && error.contains(maximum),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn drain_reserves_and_kill_grace_must_fit_without_overflow() {
        super::load_from_str(&format!("{MINIMAL}\ndrain_ms = 20000")).expect("equal drain budget");
        for budgets in [
            "drain_ms = 19999",
            "cancel_reserve_ms = 20000",
            "checkpoint_reserve_ms = 20001",
            "kill_grace_ms = 15001",
            "cancel_reserve_ms = 9223372036854775807\ncheckpoint_reserve_ms = 9223372036854775807\nkill_grace_ms = 2",
        ] {
            let error = super::load_from_str(&format!("{MINIMAL}\n{budgets}")).unwrap_err();
            assert!(error.contains("drain_ms"), "{error}");
        }
    }

    #[test]
    fn rejects_unsupported_version() {
        let error = super::load_from_str(&format!("version = 2\n{MINIMAL}")).unwrap_err();
        assert!(error.contains("version"), "{error}");
    }

    #[test]
    fn minimal_config_defaults_every_optional_field() {
        let config = super::load_from_str(MINIMAL).expect("minimal config");
        assert_documented_values(&config);
    }

    #[test]
    fn canonical_example_loads_with_documented_values() {
        let config = super::load_from_str(canonical_toml(DOC).unwrap()).expect("canonical config");
        assert_documented_values(&config);
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
        let printed: super::RuntimeConfig =
            serde_json::from_slice(&stdout).expect("effective JSON");
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

    #[test]
    fn committed_document_matches_v1() {
        check_contract(DOC).expect("v1 runtime contract");
    }

    #[test]
    fn checker_rejects_missing_or_changed_version() {
        for version in ["", "Contract version: v2"] {
            assert!(
                check_contract(&DOC.replace("Contract version: v1", version))
                    .unwrap_err()
                    .contains("v1")
            );
        }
    }

    #[test]
    fn checker_names_each_missing_field_and_each_changed_cell() {
        for field in FIELDS {
            let original = row(DOC, field[0]);
            assert!(
                check_contract(&DOC.replace(&original, ""))
                    .unwrap_err()
                    .contains(field[0])
            );
            for column in 1..5 {
                let mut changed = field;
                changed[column] = "wrong";
                let changed = format!("| {} |", changed.join(" | "));
                assert!(
                    check_contract(&DOC.replace(&original, &changed))
                        .unwrap_err()
                        .contains(field[0])
                );
            }
        }
    }

    #[test]
    fn checker_names_each_missing_or_changed_posture() {
        for posture in POSTURE {
            let original = row(DOC, posture[0]);
            assert!(
                check_contract(&DOC.replace(&original, ""))
                    .unwrap_err()
                    .contains(posture[0])
            );
            let changed = format!(
                "| {} | {} |",
                posture[0],
                if posture[1] == "false" {
                    "true"
                } else {
                    "wrong"
                }
            );
            assert!(
                check_contract(&DOC.replace(&original, &changed))
                    .unwrap_err()
                    .contains(posture[0])
            );
        }
    }
}
