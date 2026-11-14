//! The worker execution API, end to end on loopback: real listeners, a real cache and trusted
//! compile child, real guests. The cases are the ones the restricted pod runs
//! (`cluster_execute_api`); they live in `src/qualification/execution_api.rs`.
//!
//! The cases run one at a time: several of them measure the documented deadlines.
#![cfg(target_os = "linux")]

#[path = "../src/qualification/execution_api.rs"]
mod cases;

use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

macro_rules! cases {
    ($($name:ident),* $(,)?) => {$(
        #[test]
        fn $name() {
            let _serial = SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            cases::$name();
        }
    )*};
}

cases! {
    legacy_envelope_and_identity,
    non_wasm_refused_without_instance,
    another_engine_version_is_refused,
    credential_never_leaves_the_host_layer,
    redaction_holds_across_the_cut,
    imports_must_be_declared_and_allowed,
    each_listener_refuses_the_others_operations,
    cancel_survives_a_saturated_execute_listener,
    a_stalled_control_listener_does_not_touch_executions,
    admission_order_when_every_slot_is_busy,
    update_and_cancel_semantics,
    http_ceilings_and_deadlines,
    control_ceilings_and_deadlines,
    a_slow_reader_is_cut_off_at_the_response_deadline,
    the_shortest_timeout_wins,
    budgets_are_enforced,
    storage_and_refused_entries_count_against_the_budget,
    one_absolute_deadline_end_to_end,
    cache_stalls_are_supervised,
    validation_is_cancellable_and_bounded,
}

#[test]
fn every_case_has_a_test_and_a_pod_name() {
    let mut names: Vec<_> = cases::CASES.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 20);
}

#[test]
fn the_installation_check_finds_the_compile_child_beside_the_worker() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("--check-install")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("\"production_image\""), "{stdout}");
}

#[test]
fn the_production_image_ships_the_compile_child_beside_the_worker() {
    let containerfile = include_str!("../Containerfile");
    let shipped: Vec<(&str, &str)> = containerfile
        .lines()
        .filter_map(|line| line.strip_prefix("COPY --from=build "))
        .filter_map(|rest| rest.split_once(' '))
        .collect();
    let destination = |source: &str| {
        shipped
            .iter()
            .find(|(from, _)| *from == source)
            .map(|(_, to)| *to)
    };
    assert_eq!(
        destination("/build/target/release/falcone-serverless-worker"),
        Some("/usr/local/bin/falcone-serverless-worker")
    );
    // The worker resolves the child as a sibling of its own executable.
    assert_eq!(
        destination("/build/target/release/artifact-compile-child"),
        Some("/usr/local/bin/artifact-compile-child")
    );
    // The build stage builds every binary of the package, the child included.
    assert!(containerfile.contains("cargo build --release --locked -p falcone-serverless-worker"));
}

/// The worker binary, started as the pod starts it, serves both listeners from its configuration.
#[test]
fn the_worker_binary_serves_the_execution_api_from_its_configuration() {
    use std::net::{Ipv4Addr, TcpListener};
    use std::process::{Command, Stdio};

    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = std::env::temp_dir().join(format!("worker-serve-{}", std::process::id()));
    std::fs::create_dir_all(root.join("artifacts")).unwrap();
    std::fs::write(
        root.join("artifacts")
            .join(format!("{}.wasm", cases::production_digest())),
        cases::production_artifact(),
    )
    .unwrap();
    let free = || {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let (execute, control) = (free(), free());
    let config = root.join("worker.toml");
    std::fs::write(
        &config,
        cases::worker_config(execute, control, free(), free(), &root),
    )
    .unwrap();
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _worker = Child(
        Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
            .arg("serve")
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    cases::the_production_worker_serves_the_execution_api(
        (Ipv4Addr::LOCALHOST, execute).into(),
        (Ipv4Addr::LOCALHOST, control).into(),
    );
    drop(_worker);
    let _ = std::fs::remove_dir_all(&root);
}
