use super::*;
use landlock::{
    ABI, Access, AccessFs, AccessNet, Compatible, LandlockStatus, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreated, RulesetCreatedAttr, RulesetStatus, Scope,
};
use rustix::process::{Resource, Rlimit};
use std::collections::BTreeMap;
use std::fs::File;
use std::fs::{self, DirBuilder};
use std::io::{ErrorKind, PipeReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Setup handshake bytes. The child sends READY once confinement is applied, just before
// exec, and a failure byte instead if anything before or during exec fails. Silence,
// EOF without READY and unknown bytes all mean failure: only READY is trusted.
const SETUP_READY: u8 = 0;
const SETUP_CONFINEMENT_FAILED: u8 = 1;
const SETUP_EXEC_FAILED: u8 = 2;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Only this implementation is constructed by the launcher binary.
pub struct ProductionKernel {
    executable: PathBuf,
    children: BTreeMap<u64, Child>,
    next_child: u64,
    handshake_timeout: Duration,
    setup_status: Option<File>,
}
enum SetupRead {
    Byte(u8),
    Eof,
    Timeout,
    Broken,
}
/// Wait for the next status byte without spinning: poll the pipe up to the deadline.
fn read_setup_status(status: &mut PipeReader, deadline: Instant) -> SetupRead {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = rustix::event::Timespec {
            tv_sec: remaining.as_secs().try_into().unwrap_or(i64::MAX),
            tv_nsec: remaining.subsec_nanos().into(),
        };
        let mut fds = [rustix::event::PollFd::new(
            &*status,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return SetupRead::Timeout,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return SetupRead::Broken,
        }
        let mut byte = [0];
        match status.read(&mut byte) {
            Ok(0) => return SetupRead::Eof,
            Ok(_) => return SetupRead::Byte(byte[0]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return SetupRead::Broken,
        }
    }
}
/// Presence only: a dangling credential mount/symlink is also rejected.
fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(_) => Err(Failure::Probe),
    }
}
impl ProductionKernel {
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            children: BTreeMap::new(),
            next_child: 0,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            setup_status: None,
        }
    }
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }
    /// Trampoline side of the setup handshake, called first in `--child` mode. std can map
    /// only stdio into a child, so `spawn` hands over the status pipe as descriptor 0.
    /// Move it to a CLOEXEC descriptor (the exec closes it, which is the parent's proof
    /// that exec happened and keeps it from the toolchain) and give descriptor 0 back to
    /// the toolchain as /dev/null: an empty stdin, as the parent's closed pipe used to be.
    pub fn adopt_setup_status(&mut self) -> Result<()> {
        let status = rustix::io::fcntl_dupfd_cloexec(rustix::stdio::stdin(), 3)
            .map_err(|_| Failure::Confinement)?;
        let null = File::open("/dev/null").map_err(|_| Failure::Confinement)?;
        rustix::stdio::dup2_stdin(&null).map_err(|_| Failure::Confinement)?;
        self.setup_status = Some(File::from(status));
        Ok(())
    }
    /// Best effort: if the byte is lost the parent sees EOF without READY, also a failure.
    pub fn report_setup_failure(&mut self, failure: Failure) {
        let _ = self.send_setup_status(if failure == Failure::Spawn {
            SETUP_EXEC_FAILED
        } else {
            SETUP_CONFINEMENT_FAILED
        });
    }
    fn send_setup_status(&mut self, byte: u8) -> Result<()> {
        match &mut self.setup_status {
            Some(status) => status.write_all(&[byte]).map_err(|_| Failure::Confinement),
            None => Ok(()),
        }
    }
    /// Parent side: READY must arrive, then EOF once exec closed the descriptor.
    fn await_setup(&self, mut status: PipeReader) -> Result<()> {
        let deadline = Instant::now() + self.handshake_timeout;
        match read_setup_status(&mut status, deadline) {
            SetupRead::Byte(SETUP_READY) => {}
            SetupRead::Byte(SETUP_EXEC_FAILED) => return Err(Failure::Spawn),
            _ => return Err(Failure::Confinement),
        }
        match read_setup_status(&mut status, deadline) {
            SetupRead::Eof => Ok(()),
            SetupRead::Byte(SETUP_EXEC_FAILED) | SetupRead::Timeout => Err(Failure::Spawn),
            _ => Err(Failure::Confinement),
        }
    }
    fn trampoline(&mut self, spec: &SpawnSpec, probe: bool) -> Result<Command> {
        // std creates its own FDs with CLOEXEC. Refuse unexpected inherited descriptors
        // rather than attempting unsafe ownership of arbitrary raw descriptor numbers.
        self.check_descriptors()?;
        for root in [
            &spec.config.toolchain_dir,
            &spec.config.catalogue_dir,
            &spec.config.workspace_root,
            &spec.workspace,
        ] {
            if fs::canonicalize(root).map_err(|_| Failure::Configuration)? != *root {
                return Err(Failure::Configuration);
            }
        }
        let mut command = Command::new(&self.executable);
        spec.configure_environment(&mut command);
        command
            .arg(if probe { "--probe" } else { "--child" })
            .args(spec.config.arguments())
            .arg(&spec.workspace)
            .arg(spec.profile.name())
            .current_dir(&spec.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }
}
fn required_ruleset() -> Result<RulesetCreated> {
    Ruleset::default()
        .set_compatibility(landlock::CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V6))
        .map_err(|_| Failure::Landlock)?
        .handle_access(AccessNet::BindTcp | AccessNet::ConnectTcp)
        .map_err(|_| Failure::Landlock)?
        .scope(Scope::Signal | Scope::AbstractUnixSocket)
        .map_err(|_| Failure::Landlock)?
        .create()
        .map_err(|_| Failure::Landlock)
}
impl KernelInterface for ProductionKernel {
    fn probe_landlock(&mut self, minimum: u32) -> Result<()> {
        if minimum < 6 {
            return Err(Failure::Configuration);
        }
        // HardRequirement proves ABI >=6 without restricting the control process.
        // The disposable confinement probe additionally checks the reported ABI against
        // the configured minimum, including when it is higher than six.
        drop(required_ruleset()?);
        Ok(())
    }
    fn path_present(&mut self, probe: CredentialProbe) -> Result<bool> {
        path_exists(probe.path())
    }
    fn forbidden_path_present(&mut self, path: &Path) -> Result<bool> {
        path_exists(path)
    }
    fn create_workspace(&mut self, root: &Path) -> Result<PathBuf> {
        if fs::canonicalize(root).map_err(|_| Failure::Workspace)? != root {
            return Err(Failure::Workspace);
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let tick = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Failure::Workspace)?
                .as_nanos();
            let path = root.join(format!(
                "build-{}-{tick}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(Failure::Workspace),
            }
        }
        Err(Failure::Workspace)
    }
    fn remove_workspace(&mut self, workspace: &Path) -> Result<()> {
        fs::remove_dir_all(workspace).map_err(|_| Failure::Workspace)
    }
    fn probe_confinement(&mut self, spec: &SpawnSpec) -> Result<()> {
        // No toolchain runs here. This trusted helper applies the *same* plan and exits.
        let status = self
            .trampoline(spec, true)?
            .status()
            .map_err(|_| Failure::Probe)?;
        if status.success() {
            Ok(())
        } else {
            Err(Failure::Probe)
        }
    }
    fn no_new_privs(&mut self) -> Result<()> {
        rustix::thread::set_no_new_privs(true).map_err(|_| Failure::Privileges)
    }
    fn set_limits(&mut self, limits: &Limits) -> Result<()> {
        for (resource, value) in [
            (Resource::As, limits.address_space),
            (Resource::Cpu, limits.cpu_seconds),
            (Resource::Nofile, limits.open_files),
            (Resource::Fsize, limits.file_size),
        ] {
            rustix::process::setrlimit(
                resource,
                Rlimit {
                    current: Some(value),
                    maximum: Some(value),
                },
            )
            .map_err(|_| Failure::Limits)?;
        }
        Ok(())
    }
    fn check_descriptors(&mut self) -> Result<()> {
        // Snapshot numbers, then close the directory iterator before checking fdinfo.
        // No /proc fd is passed into the toolchain. fdinfo reads metadata, never contents.
        let fds = fs::read_dir("/proc/self/fd")
            .map_err(|_| Failure::Descriptors)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|_| Failure::Descriptors)?;
        let mut inventory = Vec::new();
        for fd in fds {
            let number: u32 = fd
                .to_str()
                .ok_or(Failure::Descriptors)?
                .parse()
                .map_err(|_| Failure::Descriptors)?;
            let text = match fs::read_to_string(Path::new("/proc/self/fdinfo").join(fd)) {
                Ok(text) => text,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(_) => return Err(Failure::Descriptors),
            };
            let flags = text
                .lines()
                .find_map(|line| line.strip_prefix("flags:\t"))
                .and_then(|flags| u32::from_str_radix(flags.trim(), 8).ok())
                .ok_or(Failure::Descriptors)?;
            inventory.push(InheritedFd {
                number,
                close_on_exec: flags & 0o2000000 != 0,
            });
        }
        Descriptors::OwnStdioPipes.after_exec(&inventory)?;
        Ok(())
    }
    fn restrict_landlock(&mut self, plan: &LandlockPlan) -> Result<()> {
        let mut ruleset = Ruleset::default()
            .set_compatibility(plan.compatibility)
            .handle_access(plan.handled_fs)
            .map_err(|_| Failure::Landlock)?
            .handle_access(plan.handled_net)
            .map_err(|_| Failure::Landlock)?
            .scope(plan.scopes)
            .map_err(|_| Failure::Landlock)?
            .create()
            .map_err(|_| Failure::Landlock)?;
        for path in &plan.paths {
            ruleset = ruleset
                .add_rule(PathBeneath::new(
                    PathFd::new(&path.path).map_err(|_| Failure::Landlock)?,
                    path.access,
                ))
                .map_err(|_| Failure::Landlock)?;
        }
        let status = ruleset.restrict_self().map_err(|_| Failure::Landlock)?;
        let actual = match status.landlock {
            LandlockStatus::Available {
                effective_abi,
                kernel_abi,
            } => kernel_abi.unwrap_or(effective_abi as i32),
            _ => return Err(Failure::Landlock),
        };
        if status.ruleset != RulesetStatus::FullyEnforced
            || !status.no_new_privs
            || i64::from(actual) < i64::from(plan.minimum_abi)
        {
            return Err(Failure::Landlock);
        }
        Ok(())
    }
    fn install_seccomp(&mut self, plan: &SeccompPlan) -> Result<()> {
        for program in plan.compile()? {
            seccompiler::apply_filter(&program).map_err(|_| Failure::Seccomp)?;
        }
        Ok(())
    }
    fn exec(&mut self, spec: &SpawnSpec) -> Result<()> {
        // Confinement is complete; the parent's spawn() waits for this byte.
        self.send_setup_status(SETUP_READY)?;
        // The trampoline already owns three private pipes. Reuse them across exec;
        // creating new pipes here would leave them with no trusted reader/writer.
        let mut command = spec.command();
        let _ = command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .exec();
        Err(Failure::Spawn)
    }
    fn spawn(&mut self, spec: &SpawnSpec) -> Result<u64> {
        let (status, writer) = std::io::pipe().map_err(|_| Failure::Spawn)?;
        let mut command = self.trampoline(spec, false)?;
        command.stdin(writer);
        let spawned = command.spawn();
        // The command owns the parent's copy of the write end; EOF needs it closed.
        drop(command);
        let mut child = spawned.map_err(|_| Failure::Spawn)?;
        // Not started until the child reports its own confinement (or fails to).
        if let Err(failure) = self.await_setup(status) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure);
        }
        // Drain/discard bounded chunks during poll. Never log workspace/toolchain output.
        let flags = rustix::fs::OFlags::NONBLOCK;
        let pipe_setup = (|| {
            if let Some(stdout) = &child.stdout {
                rustix::fs::fcntl_setfl(stdout, flags)?;
            }
            if let Some(stderr) = &child.stderr {
                rustix::fs::fcntl_setfl(stderr, flags)?;
            }
            std::io::Result::Ok(())
        })();
        if pipe_setup.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failure::Spawn);
        }
        self.next_child += 1;
        self.children.insert(self.next_child, child);
        Ok(self.next_child)
    }
    fn try_wait(&mut self, id: u64) -> Result<Option<bool>> {
        let child = self.children.get_mut(&id).ok_or(Failure::Spawn)?;
        let mut buffer = [0; 8192];
        for pipe in [
            child.stdout.as_mut().map(|p| p as &mut dyn Read),
            child.stderr.as_mut().map(|p| p as &mut dyn Read),
        ]
        .into_iter()
        .flatten()
        {
            for _ in 0..8 {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => return Err(Failure::Spawn),
                }
            }
        }
        let status = child.try_wait().map_err(|_| Failure::Spawn)?;
        if status.is_some() {
            self.children.remove(&id);
        }
        Ok(status.map(|status| status.success()))
    }
    fn terminate(&mut self, id: u64) -> Result<()> {
        let child = self.children.get_mut(&id).ok_or(Failure::Spawn)?;
        if child.try_wait().map_err(|_| Failure::Spawn)?.is_none() {
            child.kill().map_err(|_| Failure::Spawn)?;
        }
        child.wait().map_err(|_| Failure::Spawn)?;
        self.children.remove(&id);
        Ok(())
    }
}
