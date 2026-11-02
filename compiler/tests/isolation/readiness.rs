use super::support::{MockKernel, TempRoot};
use falcone_serverless_compiler::isolate::*;

#[test]
fn startup_and_every_build_fail_closed_on_each_unavailable_boundary() {
    for failure in [
        Failure::Landlock,
        Failure::Seccomp,
        Failure::Probe,
        Failure::Privileges,
        Failure::Limits,
        Failure::Descriptors,
    ] {
        let root = TempRoot::new();
        let kernel = MockKernel {
            fail: Some(failure),
            ..Default::default()
        };
        let mut launcher = Launcher::new(root.config(), kernel);
        assert!(!launcher.check_readiness().ready);
        assert!(launcher.connect(1));
        assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::NotReady);
        assert!(launcher.kernel().spawned.is_empty());
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    }
    for abi in [0, 1, 5] {
        let root = TempRoot::new();
        let mut launcher = Launcher::new(
            root.config(),
            MockKernel {
                abi,
                ..Default::default()
            },
        );
        assert!(!launcher.check_readiness().ready);
        assert!(launcher.kernel().spawned.is_empty());
    }
    for minimum in [0, 1, 5, 7] {
        let root = TempRoot::new();
        let mut config = root.config();
        config.landlock_min_abi = minimum;
        let mut launcher = Launcher::new(config, MockKernel::default());
        assert!(!launcher.check_readiness().ready);
        assert!(launcher.connect(1));
        assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::NotReady);
        assert!(launcher.kernel().spawned.is_empty());
    }
}

#[test]
fn credential_probes_run_again_before_build_and_never_read_file_contents() {
    for credential in CredentialProbe::ALL {
        let root = TempRoot::new();
        let mut launcher = Launcher::new(root.config(), MockKernel::default());
        assert!(launcher.check_readiness().ready);
        launcher.kernel_mut().credential = Some(credential);
        assert!(launcher.connect(1));
        assert_eq!(launcher.build(1, Profile::Go), BuildOutcome::NotReady);
        assert_eq!(launcher.report().failure, Some(Failure::CredentialMount));
        assert!(launcher.kernel().spawned.is_empty());
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    }
}

#[test]
fn ruleset_application_failure_is_distinct_from_abi_probe_and_blocks_every_build() {
    let root = TempRoot::new();
    let mut launcher = Launcher::new(root.config(), MockKernel::default());
    assert!(launcher.check_readiness().ready);
    launcher.kernel_mut().fail = Some(Failure::Landlock);
    launcher.kernel_mut().fail_at = Some("landlock");
    assert!(!launcher.check_readiness().ready);
    assert_eq!(launcher.report().failure, Some(Failure::Landlock));
    assert!(launcher.connect(1));
    assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::NotReady);
    assert!(launcher.kernel().spawned.is_empty());
    assert!(launcher.kernel().events.contains(&"landlock"));
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn configured_forbidden_paths_are_probed_at_startup_and_before_build() {
    let root = TempRoot::new();
    let forbidden = TempRoot::new();
    let path = forbidden.0.join("signing-key");
    let mut arguments = root.config().arguments();
    arguments.push(path.clone().into_os_string());
    let config = RunnerConfig::from_arguments(&arguments).expect("optional forbidden paths");
    let mut launcher = Launcher::new(config, MockKernel::default());
    assert!(launcher.check_readiness().ready);
    // Only a dangling symlink: a presence probe must not follow or read it.
    std::os::unix::fs::symlink("missing", &path).unwrap();
    assert_eq!(
        launcher.check_readiness().failure,
        Some(Failure::CredentialMount)
    );
    assert!(launcher.connect(1));
    assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::NotReady);
    assert_eq!(launcher.report().failure, Some(Failure::CredentialMount));
    assert!(launcher.kernel().spawned.is_empty());
}

#[test]
fn forbidden_paths_reject_overlaps_and_invalid_path_bytes() {
    let root = TempRoot::new();
    for forbidden in [
        root.0.clone(),
        root.0.join("key"),
        root.0.parent().unwrap().into(),
        "/opt/toolchains/key".into(),
        "/opt/catalogue/key".into(),
        "/proc/self/key".into(),
        "/dev/null".into(),
        "/secrets/key\0suffix".into(),
        "relative/key".into(),
        "/secrets/../key".into(),
    ] {
        let mut args = root.config().arguments();
        args.push(forbidden.into_os_string());
        assert!(matches!(
            RunnerConfig::from_arguments(&args),
            Err(Failure::Configuration)
        ));
    }
    let mut config = root.config();
    config.forbidden_paths = vec!["/secrets/key:part".into()];
    assert_eq!(config.validate(), Err(Failure::Configuration));
}

#[test]
fn forbidden_path_arguments_roundtrip_and_absence_preserves_base_behavior() {
    let root = TempRoot::new();
    let config = root.config();
    let arguments = config.arguments();
    assert_eq!(arguments.len(), 9);
    assert!(
        RunnerConfig::from_arguments(&arguments)
            .unwrap()
            .forbidden_paths
            .is_empty()
    );
    let mut arguments = arguments;
    arguments.push("/deployment/key:/deployment/identity:/deployment/config".into());
    let configured = RunnerConfig::from_arguments(&arguments).unwrap();
    assert_eq!(configured.arguments(), arguments);
    assert_eq!(configured.forbidden_paths.len(), 3);
    let mut launcher = Launcher::new(config, MockKernel::default());
    assert!(launcher.check_readiness().ready);
    assert!(launcher.connect(1));
    assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::Started);
}
