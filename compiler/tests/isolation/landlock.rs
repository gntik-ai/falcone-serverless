use falcone_serverless_compiler::isolate::{LandlockPlan, Profile, RunnerConfig};
use landlock::{ABI, Access, AccessFs, AccessNet, CompatLevel, Scope};

#[test]
fn each_build_has_exactly_the_reviewed_paths_and_accesses() {
    let config = RunnerConfig::new(
        "/opt/toolchains",
        "/opt/catalogue",
        "/workspaces",
        "compiler-control",
    );
    let plan =
        LandlockPlan::for_build(&config, "/workspaces/build-1".as_ref(), Profile::Rust).unwrap();
    assert_eq!(plan.abi, ABI::V6);
    assert_eq!(plan.compatibility, CompatLevel::HardRequirement);
    assert_eq!(plan.handled_fs, AccessFs::from_all(ABI::V6));
    assert_eq!(plan.handled_net, AccessNet::BindTcp | AccessNet::ConnectTcp);
    assert_eq!(plan.scopes, Scope::Signal | Scope::AbstractUnixSocket);
    assert_eq!(
        plan.paths
            .iter()
            .map(|p| p.path.to_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "/opt/toolchains",
            "/opt/catalogue",
            "/workspaces/build-1",
            "/dev/null",
            "/proc/self"
        ]
    );
    assert_eq!(
        plan.paths[0].access,
        AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute
    );
    assert_eq!(plan.paths[1].access, AccessFs::ReadFile | AccessFs::ReadDir);
    assert_eq!(
        plan.paths[2].access,
        AccessFs::from_all(ABI::V6) & !AccessFs::Execute
    );
    assert_eq!(
        plan.paths[3].access,
        AccessFs::ReadFile | AccessFs::WriteFile
    );
    assert_eq!(plan.paths[4].access, AccessFs::ReadFile | AccessFs::ReadDir);
    for profile in Profile::ALL {
        assert!(profile.extra_paths().is_empty());
    }
}

#[test]
fn broad_overlapping_and_escape_paths_and_pathname_sockets_are_rejected() {
    for path in [
        "/",
        "/usr",
        "/usr/lib",
        "/lib",
        "/lib/toolchains",
        "/lib64/toolchains",
        "/proc",
        "/proc/self",
        "/sys",
        "/sys/devices",
        "/dev",
        "/dev/shm",
        "/tmp",
        "/run/secrets",
        "/workspaces",
        "/opt/../etc",
    ] {
        let config = RunnerConfig::new(path, "/opt/catalogue", "/workspaces", "compiler-control");
        assert!(
            LandlockPlan::for_build(&config, "/workspaces/build-1".as_ref(), Profile::Rust)
                .is_err()
        );
    }
    for socket in ["", "/run/control.sock", "a/b", "bad\0name"] {
        let config = RunnerConfig::new("/opt/toolchains", "/opt/catalogue", "/workspaces", socket);
        assert!(config.validate().is_err());
    }
    let config = RunnerConfig::new(
        "/opt/toolchains",
        "/opt/catalogue",
        "/workspaces",
        "compiler-control",
    );
    for workspace in [
        "/workspaces",
        "/workspaces/a/b",
        "/other/build",
        "/workspaces/../other",
    ] {
        assert!(LandlockPlan::for_build(&config, workspace.as_ref(), Profile::Rust).is_err());
    }
}

#[test]
fn trampoline_arguments_carry_only_typed_runner_configuration() {
    let config = RunnerConfig::new(
        "/opt/toolchains",
        "/opt/catalogue",
        "/workspaces",
        "compiler-control",
    );
    let args = config.arguments();
    assert_eq!(args.len(), 9);
    let parsed = RunnerConfig::from_arguments(&args).unwrap();
    assert_eq!(parsed.arguments(), args);
    assert!(RunnerConfig::from_arguments(&args[..8]).is_err());
    let mut invalid = args;
    invalid.push("signing_key_path".into());
    assert!(RunnerConfig::from_arguments(&invalid).is_err());
}
