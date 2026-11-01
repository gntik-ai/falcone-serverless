//! The setup handshake between `ProductionKernel::spawn` and the `--child` trampoline.
//! Fake trampolines (shell scripts) stand in for the launcher binary so the parent side
//! runs against every child behaviour; the real binary and the real exec path are driven
//! where they need no Landlock.
use super::support::{SIGNING_KEY, SPAWN_LOCK};
use falcone_serverless_compiler::isolate::*;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

struct Deployment {
    root: PathBuf,
}
impl Deployment {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "compiler-handshake-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        for dir in ["toolchains/rust/bin", "catalogue", "workspaces"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self { root }
    }
    fn config(&self) -> RunnerConfig {
        RunnerConfig::new(
            self.root.join("toolchains"),
            self.root.join("catalogue"),
            self.root.join("workspaces"),
            "compiler-control",
        )
        .with_forbidden_paths([SIGNING_KEY])
    }
    fn spec(&self, config: &RunnerConfig) -> SpawnSpec {
        let workspace = self.root.join("workspaces/build-1");
        fs::create_dir_all(&workspace).unwrap();
        SpawnSpec::for_build(config, &workspace, Profile::Rust).unwrap()
    }
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}
impl Drop for Deployment {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn serialized() -> MutexGuard<'static, ()> {
    SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
const FAST: Duration = Duration::from_millis(400);
// The status pipe arrives as the trampoline's descriptor 0 (the only one std can map).
const SLEEP: &str = "PATH=/usr/bin:/bin exec sleep 30";

fn spawn_with(body: &str) -> (Result<u64>, Duration) {
    let _serialized = serialized();
    let deployment = Deployment::new();
    let launcher = deployment.script("launcher.sh", body);
    let mut kernel = ProductionKernel::new(launcher).with_handshake_timeout(FAST);
    let spec = deployment.spec(&deployment.config());
    let started = Instant::now();
    (kernel.spawn(&spec), started.elapsed())
}

#[test]
fn a_probe_that_passes_followed_by_a_child_that_dies_silently_is_a_confinement_failure() {
    let _serialized = serialized();
    let deployment = Deployment::new();
    let launcher = deployment.script("launcher.sh", "[ \"$1\" = --child ] && exit 1\nexit 0");
    let mut kernel = ProductionKernel::new(launcher).with_handshake_timeout(FAST);
    let spec = deployment.spec(&deployment.config());
    assert_eq!(kernel.probe_confinement(&spec), Ok(()));
    assert_eq!(kernel.spawn(&spec), Err(Failure::Confinement));
    assert_eq!(
        kernel.try_wait(1),
        Err(Failure::Spawn),
        "no child is retained"
    );
}

#[test]
fn a_reported_failure_byte_maps_to_confinement_or_spawn_and_anything_else_fails_closed() {
    for (body, expected) in [
        ("printf '\\001' >&0\nexit 1", Failure::Confinement),
        (
            "printf '\\000' >&0\nprintf '\\002' >&0\nexit 1",
            Failure::Spawn,
        ),
        ("printf 'x' >&0\nexit 1", Failure::Confinement),
        ("printf '\\000x' >&0\nexit 1", Failure::Confinement),
    ] {
        let (result, _) = spawn_with(body);
        assert_eq!(result, Err(expected), "{body:?}");
    }
}

#[test]
fn a_child_that_never_reports_is_killed_after_the_bounded_wait() {
    let (result, elapsed) = spawn_with(SLEEP);
    assert_eq!(result, Err(Failure::Confinement));
    assert!(
        elapsed >= FAST && elapsed < Duration::from_secs(10),
        "{elapsed:?}"
    );
}

#[test]
fn a_child_that_reports_ready_but_never_completes_exec_is_a_spawn_failure() {
    let (result, elapsed) = spawn_with(&format!("printf '\\000' >&0\n{SLEEP}"));
    assert_eq!(result, Err(Failure::Spawn));
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
}

#[test]
fn spawn_returns_only_after_the_ready_byte_and_exec_close_the_status_descriptor() {
    let _serialized = serialized();
    let deployment = Deployment::new();
    // Ready, then "exec": closing descriptor 0 stands in for the CLOEXEC close.
    let launcher = deployment.script(
        "launcher.sh",
        "printf '\\000' >&0\nexec 0</dev/null\n/bin/sleep 0.2\nexit 3",
    );
    let mut kernel = ProductionKernel::new(launcher).with_handshake_timeout(Duration::from_secs(5));
    let spec = deployment.spec(&deployment.config());
    let child = kernel.spawn(&spec).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match kernel.try_wait(child).unwrap() {
            Some(success) => {
                assert!(!success, "the child's own exit status is still reported");
                break;
            }
            None => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
#[ignore = "re-executed by spawned_toolchain_sees_only_stdio as the trampoline"]
fn trampoline_child() {
    let root =
        PathBuf::from(std::env::var_os("ISOLATE_HANDSHAKE_ROOT").expect("run through parent"));
    let config = RunnerConfig::new(
        root.join("toolchains"),
        root.join("catalogue"),
        root.join("workspaces"),
        "compiler-control",
    )
    .with_forbidden_paths([SIGNING_KEY]);
    let spec =
        SpawnSpec::for_build(&config, &root.join("workspaces/build-1"), Profile::Rust).unwrap();
    let mut kernel = ProductionKernel::new(std::env::current_exe().unwrap());
    kernel.adopt_setup_status().unwrap();
    kernel.exec(&spec).unwrap();
}

#[test]
fn the_toolchain_sees_only_descriptors_0_1_2_and_the_handshake_ends_with_ready_then_eof() {
    let _serialized = serialized();
    let deployment = Deployment::new();
    deployment.script(
        "toolchains/rust/bin/build",
        r#"for entry in /proc/$$/fd/*; do
  fd=${entry##*/}
  [ -e /proc/$$/fdinfo/$fd ] || continue
  while read -r key value; do
    [ "$key" = "flags:" ] && echo "FD $fd $value"
  done < /proc/$$/fdinfo/$fd
done"#,
    );
    fs::create_dir_all(deployment.root.join("workspaces/build-1")).unwrap();
    let (mut status, writer) = std::io::pipe().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "handshake::trampoline_child",
            "--ignored",
            "--nocapture",
        ])
        .arg("--test-threads=1")
        .env("ISOLATE_HANDSHAKE_ROOT", &deployment.root)
        .stdin(writer)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    drop(command);
    let mut bytes = Vec::new();
    status.read_to_end(&mut bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        bytes,
        [0],
        "one ready byte, then EOF when exec closed the descriptor"
    );
    let inheritable: BTreeSet<u32> = text
        .lines()
        .filter_map(|line| line.split_once("FD ").map(|(_, rest)| rest))
        .filter_map(|rest| rest.split_once(' '))
        .filter(|(_, flags)| u32::from_str_radix(flags.trim(), 8).unwrap() & 0o2000000 == 0)
        .map(|(fd, _)| fd.parse().unwrap())
        .collect();
    assert_eq!(inheritable, BTreeSet::from([0, 1, 2]), "{text}");
}

#[test]
fn the_real_launcher_child_reports_failure_instead_of_exiting_quietly() {
    let _serialized = serialized();
    let deployment = Deployment::new();
    let credential = deployment.root.join("credential");
    fs::write(&credential, b"").unwrap();
    let mut kernel = ProductionKernel::new(env!("CARGO_BIN_EXE_launcher").into())
        .with_handshake_timeout(Duration::from_secs(20));
    // Present credential (or no Landlock on this host): the child must refuse to run.
    let present = deployment.config().with_forbidden_paths([credential]);
    assert_eq!(
        kernel.spawn(&deployment.spec(&present)),
        Err(Failure::Confinement)
    );
    // A trampoline argument vector without the forbidden-path list is refused too.
    let bare = RunnerConfig::new(
        deployment.root.join("toolchains"),
        deployment.root.join("catalogue"),
        deployment.root.join("workspaces"),
        "compiler-control",
    );
    assert_eq!(
        kernel.spawn(&deployment.spec(&bare)),
        Err(Failure::Confinement)
    );
}

/// Production OS boundary, except filesystem credential presence (a CI pod may mount a
/// service-account token) and the Landlock ABI probe (the fake trampoline never confines).
struct Hosted(ProductionKernel);
impl KernelInterface for Hosted {
    fn probe_landlock(&mut self, _: u32) -> Result<()> {
        Ok(())
    }
    fn path_present(&mut self, _: CredentialProbe) -> Result<bool> {
        Ok(false)
    }
    fn forbidden_path_present(&mut self, path: &Path) -> Result<bool> {
        self.0.forbidden_path_present(path)
    }
    fn create_workspace(&mut self, root: &Path) -> Result<PathBuf> {
        self.0.create_workspace(root)
    }
    fn remove_workspace(&mut self, workspace: &Path) -> Result<()> {
        self.0.remove_workspace(workspace)
    }
    fn probe_confinement(&mut self, spec: &SpawnSpec) -> Result<()> {
        self.0.probe_confinement(spec)
    }
    fn no_new_privs(&mut self) -> Result<()> {
        self.0.no_new_privs()
    }
    fn set_limits(&mut self, limits: &Limits) -> Result<()> {
        self.0.set_limits(limits)
    }
    fn check_descriptors(&mut self) -> Result<()> {
        self.0.check_descriptors()
    }
    fn restrict_landlock(&mut self, plan: &LandlockPlan) -> Result<()> {
        self.0.restrict_landlock(plan)
    }
    fn install_seccomp(&mut self, plan: &SeccompPlan) -> Result<()> {
        self.0.install_seccomp(plan)
    }
    fn exec(&mut self, spec: &SpawnSpec) -> Result<()> {
        self.0.exec(spec)
    }
    fn spawn(&mut self, spec: &SpawnSpec) -> Result<u64> {
        self.0.spawn(spec)
    }
    fn try_wait(&mut self, child: u64) -> Result<Option<bool>> {
        self.0.try_wait(child)
    }
    fn terminate(&mut self, child: u64) -> Result<()> {
        self.0.terminate(child)
    }
}

#[test]
fn production_launcher_reports_not_ready_when_the_child_fails_after_a_passing_probe() {
    let _serialized = serialized();
    let deployment = Deployment::new();
    let workspaces = deployment.root.join("workspaces");
    let fake = deployment.script("launcher.sh", "[ \"$1\" = --child ] && exit 1\nexit 0");
    let kernel = Hosted(ProductionKernel::new(fake).with_handshake_timeout(FAST));
    let mut launcher = Launcher::new(deployment.config(), kernel);
    assert!(launcher.check_readiness().ready);
    assert!(launcher.connect(1));
    assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::NotReady);
    assert_eq!(
        launcher.report(),
        ReadinessReport {
            ready: false,
            failure: Some(Failure::Confinement)
        }
    );
    assert_eq!(fs::read_dir(&workspaces).unwrap().count(), 0);
}
