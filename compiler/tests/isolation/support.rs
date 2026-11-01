use falcone_serverless_compiler::isolate::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A deployment-configured credential mount outside the five fixed probe locations.
pub const SIGNING_KEY: &str = "/srv/falcone/signing/ed25519.key";

pub struct TempRoot(pub PathBuf);
impl TempRoot {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "compiler-isolation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    pub fn config(&self) -> RunnerConfig {
        RunnerConfig::new(
            "/opt/toolchains",
            "/opt/catalogue",
            &self.0,
            "compiler-control",
        )
        .with_forbidden_paths([SIGNING_KEY])
    }
}
impl Drop for TempRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

pub struct MockKernel {
    pub events: Vec<&'static str>,
    pub fail: Option<Failure>,
    pub fail_at: Option<&'static str>,
    pub applied_limits: Vec<Limits>,
    pub descriptor_inventory: Vec<InheritedFd>,
    pub child_descriptors: Vec<u32>,
    pub abi: u32,
    pub credential: Option<CredentialProbe>,
    pub forbidden_present: Vec<PathBuf>,
    /// Models a child that confines itself *after* spawn started: the readiness probe
    /// passes and the failure is only reported by the setup handshake.
    pub child_confinement_fault: bool,
    pub spawned: Vec<SpawnSpec>,
    pub workspaces: Vec<PathBuf>,
    pub finished: Option<bool>,
}
impl Default for MockKernel {
    fn default() -> Self {
        Self {
            events: vec![],
            fail: None,
            fail_at: None,
            applied_limits: vec![],
            descriptor_inventory: (0..3)
                .map(|number| InheritedFd {
                    number,
                    close_on_exec: false,
                })
                .collect(),
            child_descriptors: vec![],
            abi: 6,
            credential: None,
            forbidden_present: vec![],
            child_confinement_fault: false,
            spawned: vec![],
            workspaces: vec![],
            finished: None,
        }
    }
}
impl MockKernel {
    fn step(&mut self, event: &'static str, failure: Failure) -> Result<()> {
        self.events.push(event);
        if self.fail == Some(failure) && self.fail_at.is_none_or(|at| at == event) {
            Err(failure)
        } else {
            Ok(())
        }
    }
}
impl KernelInterface for MockKernel {
    fn probe_landlock(&mut self, minimum: u32) -> Result<()> {
        self.step("abi", Failure::Landlock)?;
        if minimum < 6 {
            return Err(Failure::Configuration);
        }
        // Match production: this probe requires V6; the disposable confinement
        // probe checks a configured minimum above V6 when applying its plan.
        if self.abi < 6 {
            Err(Failure::Landlock)
        } else {
            Ok(())
        }
    }
    fn path_present(&mut self, probe: CredentialProbe) -> Result<bool> {
        self.step("path", Failure::Probe)?;
        Ok(self.credential == Some(probe))
    }
    fn forbidden_path_present(&mut self, path: &Path) -> Result<bool> {
        self.step("path", Failure::Probe)?;
        Ok(self.forbidden_present.iter().any(|present| present == path))
    }
    fn create_workspace(&mut self, root: &Path) -> Result<PathBuf> {
        self.step("create", Failure::Workspace)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = root.join(format!("build-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).map_err(|_| Failure::Workspace)?;
        self.workspaces.push(path.clone());
        Ok(path)
    }
    fn remove_workspace(&mut self, path: &Path) -> Result<()> {
        self.events.push("remove");
        std::fs::remove_dir_all(path).map_err(|_| Failure::Workspace)
    }
    fn probe_confinement(&mut self, spec: &SpawnSpec) -> Result<()> {
        self.events.push("probe");
        apply_confinement(self, spec)
    }
    fn no_new_privs(&mut self) -> Result<()> {
        self.step("no_new_privs", Failure::Privileges)
    }
    fn set_limits(&mut self, limits: &Limits) -> Result<()> {
        self.applied_limits.push(limits.clone());
        self.step("rlimits", Failure::Limits)
    }
    fn check_descriptors(&mut self) -> Result<()> {
        self.step("fds", Failure::Descriptors)?;
        self.child_descriptors =
            Descriptors::OwnStdioPipes.after_exec(&self.descriptor_inventory)?;
        Ok(())
    }
    fn restrict_landlock(&mut self, plan: &LandlockPlan) -> Result<()> {
        self.step("landlock", Failure::Landlock)?;
        if self.abi < plan.minimum_abi {
            Err(Failure::Landlock)
        } else {
            Ok(())
        }
    }
    fn install_seccomp(&mut self, _: &SeccompPlan) -> Result<()> {
        self.step("seccomp", Failure::Seccomp)
    }
    fn exec(&mut self, _: &SpawnSpec) -> Result<()> {
        self.step("exec", Failure::Spawn)
    }
    fn spawn(&mut self, spec: &SpawnSpec) -> Result<u64> {
        if self.child_confinement_fault {
            self.events.push("child_confinement_failed");
            return Err(Failure::Confinement);
        }
        confine_and_exec(self, spec)?;
        self.spawned.push(spec.clone());
        Ok(self.spawned.len() as u64)
    }
    fn try_wait(&mut self, _: u64) -> Result<Option<bool>> {
        self.step("wait", Failure::Spawn)?;
        Ok(self.finished)
    }
    fn terminate(&mut self, _: u64) -> Result<()> {
        self.step("terminate", Failure::Spawn)
    }
}

/// Serializes every test that forks: writing an executable script while another thread
/// forks makes the later exec fail with ETXTBSY.
pub static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
