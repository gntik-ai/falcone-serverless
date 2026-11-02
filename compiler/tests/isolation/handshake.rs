use super::support::TempRoot;
use falcone_serverless_compiler::isolate::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn fixture(body: &str) -> (TempRoot, ProductionKernel, SpawnSpec) {
    let root = TempRoot::new();
    let toolchain = root.0.join("tools");
    let catalogue = root.0.join("catalogue");
    let workspaces = root.0.join("workspaces");
    for path in [&toolchain, &catalogue, &workspaces] {
        std::fs::create_dir(path).unwrap();
    }
    let config = RunnerConfig::new(toolchain, catalogue, &workspaces, "handshake-test");
    let workspace = workspaces.join("build-1");
    std::fs::create_dir(&workspace).unwrap();
    let helper = root.0.join("trampoline");
    std::fs::write(&helper, format!("#!/bin/bash\nif [[ $1 == --probe ]]; then\n if [[ $2 =~ ^[0-9]+$ ]]; then printf R >&\"$2\"; fi\n exit 0\nfi\n{body}\n")).unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let spec = SpawnSpec::for_build(&config, &workspace, Profile::Rust).unwrap();
    (root, ProductionKernel::new(PathBuf::from(&helper)), spec)
}

#[test]
fn a_probe_that_passes_followed_by_a_child_that_dies_silently_is_a_confinement_failure() {
    let (_root, mut kernel, spec) = fixture("exit 9");
    assert_eq!(kernel.probe_confinement(&spec), Ok(()));
    assert_eq!(kernel.spawn(&spec), Err(Failure::Confinement));
}

#[test]
fn a_child_that_never_reports_is_killed_after_the_bounded_wait() {
    let (_root, mut kernel, spec) =
        fixture("echo $$ > \"${@: -2:1}/child-pid\"; exec /bin/sleep 60");
    let start = std::time::Instant::now();
    assert_eq!(kernel.spawn(&spec), Err(Failure::Confinement));
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
    let pid = std::fs::read_to_string(spec.workspace.join("child-pid")).unwrap();
    // Check in the child's PID namespace; /proc can expose the host PID view.
    let pid = rustix::process::Pid::from_raw(pid.trim().parse().unwrap()).unwrap();
    assert_eq!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    );
}

#[test]
fn a_child_that_reports_ready_but_never_completes_exec_is_a_spawn_failure() {
    let (_root, mut kernel, spec) = fixture("printf R >&\"$2\"; exec /bin/sleep 60");
    assert_eq!(kernel.spawn(&spec), Err(Failure::Spawn));
}

#[test]
fn production_launcher_reports_not_ready_when_the_child_fails_after_a_passing_probe() {
    let (_root, kernel, spec) = fixture("exit 9");
    let mut launcher = Launcher::new(spec.config, kernel);
    assert!(launcher.check_readiness().ready);
    assert!(launcher.connect(1));
    assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::NotReady);
    assert_eq!(launcher.report().failure, Some(Failure::Confinement));
}

#[test]
fn inherited_descriptors_do_not_change_handshake_outcomes() {
    let extra = std::fs::File::open("/dev/null").unwrap();
    let _inherited = rustix::io::fcntl_dupfd_cloexec(&extra, 64).unwrap();
    rustix::io::fcntl_setfd(&_inherited, rustix::io::FdFlags::empty()).unwrap();
    for (body, expected) in [
        ("exit 9", Err(Failure::Confinement)),
        ("printf D >&\"$2\"; exit 9", Err(Failure::Descriptors)),
        ("printf C >&\"$2\"; exit 9", Err(Failure::Confinement)),
        (
            "printf R >&\"$2\"; printf S >&\"$2\"; exit 9",
            Err(Failure::Spawn),
        ),
        ("printf R >&\"$2\"; exec /bin/true", Ok(())),
    ] {
        let (_root, mut kernel, spec) = fixture(body);
        assert_eq!(kernel.probe_confinement(&spec), Ok(()));
        match expected {
            Ok(()) => {
                let id = kernel.spawn(&spec).unwrap();
                kernel.terminate(id).unwrap();
            }
            Err(error) => assert_eq!(kernel.spawn(&spec), Err(error)),
        }
    }
}

#[test]
fn actual_trampoline_removes_inherited_descriptors_before_confinement() {
    let (_root, _kernel, spec) = fixture("exit 9");
    let extra = std::fs::File::open("/dev/null").unwrap();
    let _inherited = rustix::io::fcntl_dupfd_cloexec(&extra, 64).unwrap();
    rustix::io::fcntl_setfd(&_inherited, rustix::io::FdFlags::empty()).unwrap();
    let mut kernel = ProductionKernel::new(env!("CARGO_BIN_EXE_launcher").into());
    // An impossible minimum proves that cleanup reaches confinement, rather than
    // failing the inherited-descriptor check. No toolchain can execute.
    let mut spec = spec;
    spec.config.landlock_min_abi = u32::MAX;
    spec.landlock.minimum_abi = u32::MAX;
    assert_eq!(kernel.spawn(&spec), Err(Failure::Confinement));
}
