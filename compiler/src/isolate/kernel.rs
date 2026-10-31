use super::*;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Descriptors {
    OwnStdioPipes,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InheritedFd {
    pub number: u32,
    pub close_on_exec: bool,
}
impl Descriptors {
    /// CLOEXEC is enforced by the trusted trampoline re-exec. Unexpected inheritable
    /// FDs fail closed: safe APIs cannot assume ownership of arbitrary raw numbers.
    pub fn after_exec(self, inventory: &[InheritedFd]) -> Result<Vec<u32>> {
        let mut inherited: Vec<_> = inventory
            .iter()
            .filter(|fd| !fd.close_on_exec)
            .map(|fd| fd.number)
            .collect();
        inherited.sort_unstable();
        if inherited != [0, 1, 2] {
            return Err(Failure::Descriptors);
        }
        Ok(inherited)
    }
}
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub workspace: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
    pub descriptors: Descriptors,
    pub limits: Limits,
    pub landlock: LandlockPlan,
    pub seccomp: SeccompPlan,
    pub config: RunnerConfig,
    pub profile: Profile,
}
impl SpawnSpec {
    pub fn for_build(config: &RunnerConfig, workspace: &Path, profile: Profile) -> Result<Self> {
        let bin = config.toolchain_dir.join(profile.name()).join("bin");
        let environment = [
            ("PATH".into(), bin.clone().into_os_string()),
            ("HOME".into(), workspace.into()),
            ("TMPDIR".into(), workspace.into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("LC_ALL".into(), "C.UTF-8".into()),
        ]
        .into_iter()
        .collect();
        Ok(Self {
            program: bin.join("build"),
            workspace: workspace.into(),
            environment,
            descriptors: Descriptors::OwnStdioPipes,
            limits: config.limits.clone(),
            landlock: LandlockPlan::for_build(config, workspace, profile)?,
            seccomp: SeccompPlan::for_host()?,
            config: config.clone(),
            profile,
        })
    }
    /// OS command-environment boundary; tests inject inherited service state here.
    pub fn configure_environment(&self, command: &mut impl CommandEnvironment) {
        command.clear_environment();
        command.set_environment(&self.environment);
    }
    /// No values from the launcher's environment are copied, even for allow-listed names.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        self.configure_environment(&mut command);
        command
            .current_dir(&self.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

/// The process builder is an OS boundary, allowing environment inheritance to be
/// tested without spawning a toolchain or mutating the test process environment.
pub trait CommandEnvironment {
    fn clear_environment(&mut self);
    fn set_environment(&mut self, entries: &BTreeMap<OsString, OsString>);
}
impl CommandEnvironment for Command {
    fn clear_environment(&mut self) {
        self.env_clear();
    }
    fn set_environment(&mut self, entries: &BTreeMap<OsString, OsString>) {
        self.envs(entries);
    }
}

/// All OS boundaries, including the re-exec probe, are replaceable only by test callers.
/// The launcher binary constructs ProductionKernel directly; no runtime bypass exists.
pub trait KernelInterface {
    fn probe_landlock(&mut self, minimum: u32) -> Result<()>;
    fn path_present(&mut self, probe: CredentialProbe) -> Result<bool>;
    fn create_workspace(&mut self, root: &Path) -> Result<PathBuf>;
    fn remove_workspace(&mut self, workspace: &Path) -> Result<()>;
    fn probe_confinement(&mut self, spec: &SpawnSpec) -> Result<()>;
    fn no_new_privs(&mut self) -> Result<()>;
    fn set_limits(&mut self, limits: &Limits) -> Result<()>;
    fn check_descriptors(&mut self) -> Result<()>;
    fn restrict_landlock(&mut self, plan: &LandlockPlan) -> Result<()>;
    fn install_seccomp(&mut self, plan: &SeccompPlan) -> Result<()>;
    fn exec(&mut self, spec: &SpawnSpec) -> Result<()>;
    fn spawn(&mut self, spec: &SpawnSpec) -> Result<u64>;
    fn try_wait(&mut self, child: u64) -> Result<Option<bool>>;
    fn terminate(&mut self, child: u64) -> Result<()>;
}

/// Shared by the production trampoline, its readiness probe, and mock tests.
pub fn apply_confinement(kernel: &mut impl KernelInterface, spec: &SpawnSpec) -> Result<()> {
    kernel.no_new_privs()?;
    kernel.set_limits(&spec.limits)?;
    kernel.check_descriptors()?;
    kernel.restrict_landlock(&spec.landlock)?;
    kernel.install_seccomp(&spec.seccomp)
}
pub fn confine_and_exec(kernel: &mut impl KernelInterface, spec: &SpawnSpec) -> Result<()> {
    apply_confinement(kernel, spec)?;
    kernel.exec(spec)
}
