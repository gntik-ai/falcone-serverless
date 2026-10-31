use super::*;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadinessReport {
    pub ready: bool,
    pub failure: Option<Failure>,
}
impl ReadinessReport {
    fn from_result(result: Result<()>) -> Self {
        Self {
            ready: result.is_ok(),
            failure: result.err(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildOutcome {
    Refused,
    NotReady,
    Started,
    Finished(bool),
    Idle,
}
struct ActiveBuild {
    child: u64,
    workspace: PathBuf,
}
pub struct Launcher<K> {
    config: RunnerConfig,
    kernel: K,
    connection: Option<u64>,
    active: Option<ActiveBuild>,
    exited: bool,
    report: ReadinessReport,
}
impl<K: KernelInterface> Launcher<K> {
    pub fn new(config: RunnerConfig, kernel: K) -> Self {
        Self {
            config,
            kernel,
            connection: None,
            active: None,
            exited: false,
            report: ReadinessReport {
                ready: false,
                failure: None,
            },
        }
    }
    pub fn config(&self) -> &RunnerConfig {
        &self.config
    }
    pub fn connection(&self) -> Option<u64> {
        self.connection
    }
    pub fn shutdown(&mut self) -> Result<()> {
        if let Some(connection) = self.connection {
            self.disconnect(connection)
        } else {
            self.exited = true;
            Ok(())
        }
    }
    pub fn kernel(&self) -> &K {
        &self.kernel
    }
    pub fn kernel_mut(&mut self) -> &mut K {
        &mut self.kernel
    }
    pub fn report(&self) -> ReadinessReport {
        self.report
    }
    pub fn exited(&self) -> bool {
        self.exited
    }
    pub fn connect(&mut self, id: u64) -> bool {
        if self.exited || self.connection.is_some() {
            return false;
        }
        self.connection = Some(id);
        true
    }
    fn prepare(&mut self, profile: Profile) -> Result<SpawnSpec> {
        check_runner_environment(&mut self.kernel, &self.config)?;
        let workspace = self.kernel.create_workspace(&self.config.workspace_root)?;
        let result = SpawnSpec::for_build(&self.config, &workspace, profile).and_then(|spec| {
            self.kernel.probe_confinement(&spec)?;
            Ok(spec)
        });
        if result.is_err() {
            self.kernel.remove_workspace(&workspace)?;
        }
        result
    }
    /// Probe in a disposable child, never confine the long-lived control listener.
    pub fn check_readiness(&mut self) -> ReadinessReport {
        if self.exited {
            self.report = ReadinessReport::from_result(Err(Failure::Control));
            return self.report;
        }
        let result = self
            .prepare(Profile::Rust)
            .and_then(|spec| self.kernel.remove_workspace(&spec.workspace));
        self.report = ReadinessReport::from_result(result);
        self.report
    }
    pub fn build(&mut self, connection: u64, profile: Profile) -> BuildOutcome {
        if self.exited || self.connection != Some(connection) || self.active.is_some() {
            return BuildOutcome::Refused;
        }
        let result = self
            .prepare(profile)
            .and_then(|spec| match self.kernel.spawn(&spec) {
                Ok(child) => {
                    self.active = Some(ActiveBuild {
                        child,
                        workspace: spec.workspace,
                    });
                    Ok(())
                }
                Err(error) => {
                    self.kernel.remove_workspace(&spec.workspace)?;
                    Err(error)
                }
            });
        self.report = ReadinessReport::from_result(result);
        if self.report.ready {
            BuildOutcome::Started
        } else {
            BuildOutcome::NotReady
        }
    }
    pub fn poll(&mut self) -> Result<BuildOutcome> {
        let Some(active) = self.active.as_ref() else {
            return Ok(BuildOutcome::Idle);
        };
        match self.kernel.try_wait(active.child) {
            Ok(Some(success)) => {
                self.kernel.remove_workspace(&active.workspace)?;
                self.active = None;
                Ok(BuildOutcome::Finished(success))
            }
            Ok(None) => Ok(BuildOutcome::Idle),
            Err(error) => {
                self.report = ReadinessReport::from_result(Err(error));
                self.stop_active()?;
                Err(error)
            }
        }
    }
    pub fn disconnect(&mut self, connection: u64) -> Result<()> {
        if self.connection != Some(connection) {
            return Ok(());
        }
        self.connection = None;
        self.exited = true;
        self.report = ReadinessReport::from_result(Err(Failure::Control));
        self.stop_active()
    }
    fn stop_active(&mut self) -> Result<()> {
        if let Some(active) = self.active.take() {
            // Whole-tree termination/reaping is the separate DEP-01.t12 enablement gate.
            let terminated = self.kernel.terminate(active.child);
            let removed = self.kernel.remove_workspace(&active.workspace);
            // An uncertain child state must never allow another build on this runner.
            if terminated.is_err() || removed.is_err() {
                self.exited = true;
            }
            terminated.and(removed)?;
        }
        Ok(())
    }
}
