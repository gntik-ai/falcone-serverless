use super::support::{MockKernel, SIGNING_KEY, TempRoot};
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
fn a_credential_at_a_configured_path_fails_readiness_at_startup_and_before_each_build() {
    // Startup: the configured path is already mounted.
    let root = TempRoot::new();
    let kernel = MockKernel {
        forbidden_present: vec![SIGNING_KEY.into()],
        ..Default::default()
    };
    let mut launcher = Launcher::new(root.config(), kernel);
    let report = launcher.check_readiness();
    assert!(!report.ready);
    assert_eq!(report.failure, Some(Failure::CredentialMount));
    // Before a build: ready first, then the path appears.
    let root = TempRoot::new();
    let mut launcher = Launcher::new(root.config(), MockKernel::default());
    assert!(launcher.check_readiness().ready);
    launcher.kernel_mut().forbidden_present = vec![SIGNING_KEY.into()];
    assert!(launcher.connect(1));
    assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::NotReady);
    assert_eq!(launcher.report().failure, Some(Failure::CredentialMount));
    assert!(launcher.kernel().spawned.is_empty());
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn only_the_configured_path_is_a_credential_mount() {
    let root = TempRoot::new();
    let kernel = MockKernel {
        forbidden_present: vec!["/srv/falcone/unrelated".into()],
        ..Default::default()
    };
    let mut launcher = Launcher::new(root.config(), kernel);
    assert!(launcher.check_readiness().ready);
}
