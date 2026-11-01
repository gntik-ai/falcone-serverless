use falcone_serverless_compiler::isolate::{Failure, LandlockPlan, Profile, RunnerConfig};
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

fn deployed() -> RunnerConfig {
    RunnerConfig::new(
        "/opt/toolchains",
        "/opt/catalogue",
        "/workspaces",
        "compiler-control",
    )
    .with_forbidden_paths(["/srv/falcone/signing/ed25519.key", "/srv/falcone/identity"])
}

#[test]
fn trampoline_arguments_carry_only_typed_runner_configuration() {
    let config = deployed();
    let args = config.arguments();
    assert_eq!(args.len(), 11);
    assert_eq!(args[9], "/srv/falcone/signing/ed25519.key");
    assert_eq!(args[10], "/srv/falcone/identity");
    let parsed = RunnerConfig::from_arguments(&args).unwrap();
    assert_eq!(parsed.arguments(), args);
    assert_eq!(parsed.forbidden_paths, config.forbidden_paths);
    assert!(RunnerConfig::from_arguments(&args[..8]).is_err());
    // Everything after the nine fixed values is a forbidden path, so a stray key is
    // rejected because it is not an absolute normalized path.
    let mut invalid = args;
    invalid.push("signing_key_path".into());
    assert_eq!(
        RunnerConfig::from_arguments(&invalid).err(),
        Some(Failure::Configuration)
    );
}

#[test]
fn a_missing_or_empty_forbidden_path_list_fails_closed() {
    let args = deployed().arguments();
    assert_eq!(
        RunnerConfig::from_arguments(&args[..9]).err(),
        Some(Failure::Configuration)
    );
    let mut empty = args[..9].to_vec();
    empty.push("".into());
    assert_eq!(
        RunnerConfig::from_arguments(&empty).err(),
        Some(Failure::Configuration)
    );
    // A config built without paths cannot be turned into a runnable argument vector.
    let bare = RunnerConfig::new("/opt/toolchains", "/opt/catalogue", "/workspaces", "c");
    assert_eq!(
        RunnerConfig::from_arguments(&bare.arguments()).err(),
        Some(Failure::Configuration)
    );
}

#[test]
fn forbidden_paths_must_be_absolute_normalized_and_clear_of_every_allowed_root() {
    for path in [
        "relative/key",
        "./key",
        "/srv/../etc/key",
        "/srv/./key",
        "key",
    ] {
        assert_eq!(
            deployed().with_forbidden_paths([path]).validate(),
            Err(Failure::Configuration),
            "{path}"
        );
    }
    // equals, sits under or contains: toolchain, catalogue, workspace root, /dev/null,
    // /proc/self (and, by containment, "/").
    for path in [
        "/opt/toolchains",
        "/opt/toolchains/rust/key",
        "/opt",
        "/opt/catalogue",
        "/opt/catalogue/key",
        "/workspaces",
        "/workspaces/build-1/key",
        "/dev/null",
        "/dev",
        "/proc/self",
        "/proc/self/environ",
        "/proc",
        "/",
    ] {
        assert_eq!(
            deployed()
                .with_forbidden_paths(["/srv/falcone/ok", path])
                .validate(),
            Err(Failure::Configuration),
            "{path}"
        );
    }
    for path in [
        "/srv/falcone/signing/ed25519.key",
        "/opt/toolchains-key",
        "/etc/falcone/x",
    ] {
        assert_eq!(
            deployed().with_forbidden_paths([path]).validate(),
            Ok(()),
            "{path}"
        );
    }
}
