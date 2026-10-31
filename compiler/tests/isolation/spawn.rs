use super::support::{MockKernel, TempRoot};
use falcone_serverless_compiler::isolate::*;
use std::collections::BTreeMap;
use std::ffi::OsString;

#[test]
fn child_is_confined_in_order_before_exec_and_has_only_its_pipes_and_toolchain_env() {
    let temp = TempRoot::new();
    let mut config = temp.config();
    config.limits = Limits {
        address_space: 536870912,
        cpu_seconds: 17,
        open_files: 64,
        file_size: 1048576,
    };
    let workspace = config.workspace_root.join("build-1");
    let spec = SpawnSpec::for_build(&config, &workspace, Profile::Rust).unwrap();
    let mut kernel = MockKernel::default();
    kernel.spawn(&spec).unwrap();
    assert_eq!(
        kernel.events,
        [
            "no_new_privs",
            "rlimits",
            "fds",
            "landlock",
            "seccomp",
            "exec"
        ]
    );
    assert_eq!(spec.descriptors, Descriptors::OwnStdioPipes);
    let expected: BTreeMap<OsString, OsString> = [
        ("PATH".into(), "/opt/toolchains/rust/bin".into()),
        ("HOME".into(), workspace.clone().into_os_string()),
        ("TMPDIR".into(), workspace.clone().into_os_string()),
        ("LANG".into(), "C.UTF-8".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
    ]
    .into_iter()
    .collect();
    assert_eq!(spec.environment, expected);
    assert_eq!(
        kernel.applied_limits,
        [Limits {
            address_space: 536870912,
            cpu_seconds: 17,
            open_files: 64,
            file_size: 1048576
        }]
    );
    assert_eq!(
        spec.program.to_str().unwrap(),
        "/opt/toolchains/rust/bin/build"
    );
    // Model inherited service state at the OS boundary: env_clear discards all of it.
    let ambient = [
        "BUILD_CREDENTIAL",
        "SIGNING_KEY",
        "SERVICE_TOKEN",
        "LD_PRELOAD",
        "RUSTC_WRAPPER",
    ];
    for key in ambient {
        assert!(!spec.environment.contains_key(&OsString::from(key)));
    }
    let command = spec.command();
    assert_eq!(
        command
            .get_envs()
            .filter(|(_, value)| value.is_some())
            .count(),
        5
    );
}

#[test]
fn failure_in_each_child_setup_step_prevents_exec() {
    let temp = TempRoot::new();
    let spec = SpawnSpec::for_build(&temp.config(), &temp.0.join("build-1"), Profile::Go).unwrap();
    for failure in [
        Failure::Privileges,
        Failure::Limits,
        Failure::Descriptors,
        Failure::Landlock,
        Failure::Seccomp,
    ] {
        let mut kernel = MockKernel {
            fail: Some(failure),
            ..Default::default()
        };
        assert_eq!(kernel.spawn(&spec), Err(failure));
        assert!(!kernel.events.contains(&"exec"));
        assert!(kernel.spawned.is_empty());
    }
}

#[test]
fn reexec_drops_extra_cloexec_descriptors_and_refuses_inheritable_descriptors() {
    let inventory = vec![
        InheritedFd {
            number: 0,
            close_on_exec: false,
        },
        InheritedFd {
            number: 1,
            close_on_exec: false,
        },
        InheritedFd {
            number: 2,
            close_on_exec: false,
        },
        InheritedFd {
            number: 3,
            close_on_exec: true,
        },
        InheritedFd {
            number: 99,
            close_on_exec: true,
        },
    ];
    assert_eq!(
        Descriptors::OwnStdioPipes.after_exec(&inventory).unwrap(),
        [0, 1, 2]
    );
    let root = TempRoot::new();
    let spec =
        SpawnSpec::for_build(&root.config(), &root.0.join("build-1"), Profile::Rust).unwrap();
    let mut kernel = MockKernel {
        descriptor_inventory: inventory.clone(),
        ..Default::default()
    };
    kernel.spawn(&spec).unwrap();
    assert_eq!(kernel.child_descriptors, [0, 1, 2]);
    let mut leaked = inventory;
    leaked[3].close_on_exec = false;
    let mut kernel = MockKernel {
        descriptor_inventory: leaked.clone(),
        ..Default::default()
    };
    assert_eq!(kernel.spawn(&spec), Err(Failure::Descriptors));
    assert!(!kernel.events.contains(&"exec"));
    assert_eq!(
        Descriptors::OwnStdioPipes.after_exec(&leaked),
        Err(Failure::Descriptors)
    );
}

#[derive(Default)]
struct MockProcessEnvironment(BTreeMap<OsString, OsString>);
impl CommandEnvironment for MockProcessEnvironment {
    fn clear_environment(&mut self) {
        self.0.clear();
    }
    fn set_environment(&mut self, entries: &BTreeMap<OsString, OsString>) {
        self.0.extend(entries.clone());
    }
}
#[test]
fn inherited_credentials_and_even_inherited_toolchain_values_are_scrubbed() {
    let root = TempRoot::new();
    let spec =
        SpawnSpec::for_build(&root.config(), &root.0.join("build-1"), Profile::Rust).unwrap();
    let mut process = MockProcessEnvironment(
        [
            ("BUILD_CREDENTIAL", "test-canary"),
            ("SERVICE_TOKEN", "test-canary"),
            ("SIGNING_KEY", "test-canary"),
            ("LD_PRELOAD", "test-canary"),
            ("RUSTC_WRAPPER", "test-canary"),
            ("PATH", "/service/bin"),
            ("HOME", "/service/home"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect(),
    );
    spec.configure_environment(&mut process);
    assert_eq!(process.0.len(), 5);
    assert_eq!(
        process.0.get(&OsString::from("PATH")),
        Some(&OsString::from("/opt/toolchains/rust/bin"))
    );
    assert_eq!(
        process.0.get(&OsString::from("HOME")),
        Some(&root.0.join("build-1").into_os_string())
    );
    for key in [
        "BUILD_CREDENTIAL",
        "SERVICE_TOKEN",
        "SIGNING_KEY",
        "LD_PRELOAD",
        "RUSTC_WRAPPER",
    ] {
        assert!(!process.0.contains_key(&OsString::from(key)));
    }
}
