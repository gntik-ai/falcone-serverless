//! WAS-01 ac3 harness. Every operation is checked before a runner call.
use super::Namespace;
use super::evidence::{RESOURCES, VERBS};

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

pub fn plan(namespace: &Namespace) -> Vec<Vec<String>> {
    let name = namespace.name();
    let mut commands = vec![
        words(&["create", "namespace", name]),
        words(&["get", "--raw", "/version"]),
    ];
    for resource in RESOURCES {
        for verb in VERBS {
            commands.push(words(&["auth", "can-i", verb, resource, "-n", name]));
        }
    }
    commands.push(words(&["delete", "namespace", name, "--wait"]));
    commands
}

/// An exact grammar, rather than a flag blacklist: extra arguments always fail closed.
pub fn plan_safe(namespace: &Namespace, commands: &[Vec<String>]) -> bool {
    !commands.is_empty()
        && commands.iter().all(|command| {
            let args: Vec<_> = command.iter().map(String::as_str).collect();
            match args.as_slice() {
                ["get", "--raw", "/version"] => true,
                ["create", "namespace", name] | ["delete", "namespace", name, "--wait"] => {
                    *name == namespace.name()
                }
                ["auth", "can-i", verb, resource, "-n", name] => {
                    *name == namespace.name()
                        && VERBS.contains(verb)
                        && RESOURCES.contains(resource)
                }
                _ => false,
            }
        })
}

use super::Kubeconfigs;
use super::evidence::{Reason, Report, Verdict};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Output {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: Option<Reason>,
}
pub trait Runner {
    fn run(&self, args: &[String]) -> Result<Output, Reason>;
}

pub struct ProcessKubectl {
    program: PathBuf,
    flags: Vec<String>,
    timeout: Duration,
}
impl ProcessKubectl {
    pub fn new(program: &Path, configs: &Kubeconfigs, timeout: Duration) -> Result<Self, Reason> {
        Ok(Self {
            program: program.into(),
            flags: configs.kubectl_flags().map_err(|_| Reason::ProbeError)?,
            timeout,
        })
    }
}

const STREAM_BOUND: usize = 8192;
struct Capture {
    bytes: Vec<u8>,
    truncated: bool,
    eof: bool,
}
impl Capture {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            truncated: false,
            eof: false,
        }
    }
    fn drain(&mut self, stream: &mut impl Read) -> Result<(), Reason> {
        // Fairness matters: even an endless writer must yield to the deadline check.
        for _ in 0..16 {
            let mut buffer = [0u8; 4096];
            match stream.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(count) => {
                    let keep = count.min(STREAM_BOUND - self.bytes.len());
                    self.bytes.extend_from_slice(&buffer[..keep]);
                    self.truncated |= keep < count;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(Reason::ProbeError),
            }
        }
        Ok(())
    }
}
fn nonblocking(fd: &impl std::os::fd::AsFd) -> Result<(), Reason> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    let flags = fcntl_getfl(fd).map_err(|_| Reason::ProbeError)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(|_| Reason::ProbeError)
}

fn classify_stderr(bytes: &[u8]) -> Option<Reason> {
    if bytes.is_empty() {
        return None;
    }
    let forbidden = bytes
        .split(|b| *b == b'\n')
        .any(|line| line.starts_with(b"Error from server (Forbidden):"));
    Some(if forbidden {
        Reason::Forbidden
    } else {
        Reason::ProbeError
    })
}
struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // A new process group also bounds children that inherit the output pipes.
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Runner for ProcessKubectl {
    fn run(&self, args: &[String]) -> Result<Output, Reason> {
        use std::os::unix::process::CommandExt;
        let started = Instant::now();
        let child = Command::new(&self.program)
            .args(&self.flags)
            .args(args)
            .env_remove("KUBECONFIG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|_| Reason::ProbeError)?;
        let mut child = ChildGuard(child);
        let mut stdout = child.0.stdout.take().ok_or(Reason::ProbeError)?;
        let mut stderr = child.0.stderr.take().ok_or(Reason::ProbeError)?;
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let mut out = Capture::new();
        let mut err = Capture::new();
        let mut status = None;
        loop {
            out.drain(&mut stdout)?;
            err.drain(&mut stderr)?;
            if status.is_none() {
                status = child.0.try_wait().map_err(|_| Reason::ProbeError)?;
            }
            if let Some(status) = status.filter(|_| out.eof && err.eof) {
                if out.truncated || err.truncated {
                    return Err(Reason::ProbeError);
                }
                return Ok(Output {
                    exit_code: status.code(),
                    stdout: String::from_utf8(out.bytes).map_err(|_| Reason::ProbeError)?,
                    stderr: classify_stderr(&err.bytes),
                });
            }
            if started.elapsed() >= self.timeout {
                return Err(Reason::ProbeError);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn probe_error(output: &Result<Output, Reason>) -> Option<Reason> {
    match output {
        Err(_) => Some(Reason::ProbeError),
        Ok(output) => output.stderr,
    }
}
pub fn probe_version(report: &mut Report, runner: &impl Runner) -> Result<(), &'static str> {
    let output = runner.run(&words(&["get", "--raw", "/version"]));
    if let Some(reason) = probe_error(&output) {
        return report.record("kubernetes-version", Verdict::Unverified, reason, None);
    }
    let output = output.map_err(|_| "probe-error")?;
    if output.exit_code != Some(0) {
        return report.record(
            "kubernetes-version",
            Verdict::Unverified,
            Reason::ProbeError,
            None,
        );
    }
    let value = serde_json::from_str::<serde_json::Value>(&output.stdout).ok();
    let version = value
        .as_ref()
        .and_then(|v| v.get("gitVersion"))
        .and_then(serde_json::Value::as_str);
    if let Some(version) = version.filter(|v| super::evidence::version_target(v).is_ok()) {
        report.record(
            "kubernetes-version",
            Verdict::Passed,
            Reason::Recorded,
            Some(version),
        )
    } else {
        report.record(
            "kubernetes-version",
            Verdict::Unverified,
            Reason::InvalidVersion,
            None,
        )
    }
}
pub fn probe_permissions(
    report: &mut Report,
    runner: &impl Runner,
    namespace: &Namespace,
) -> Result<(), &'static str> {
    for resource in RESOURCES {
        for verb in VERBS {
            let output = runner.run(&words(&[
                "auth",
                "can-i",
                verb,
                resource,
                "-n",
                namespace.name(),
            ]));
            let id = format!("can-{verb}-{resource}");
            if let Some(reason) = probe_error(&output) {
                report.record(&id, Verdict::Unverified, reason, None)?;
            } else {
                let output = output.map_err(|_| "probe-error")?;
                match (output.stdout.trim(), output.exit_code) {
                    ("yes", Some(0)) => {
                        report.record(&id, Verdict::Passed, Reason::Allowed, Some("yes"))?
                    }
                    ("no", Some(0 | 1)) => {
                        report.record(&id, Verdict::Unverified, Reason::Denied, None)?
                    }
                    _ => report.record(&id, Verdict::Unverified, Reason::ProbeError, None)?,
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum ReportGate {
    Offline,
    Cluster(Kubeconfigs),
}
/// Local validation only. Either unset switch keeps even invalid configuration offline.
pub fn gate(
    was01: Option<&str>,
    cluster: Option<&str>,
    mode: Option<&str>,
    kubeconfig: Option<&str>,
) -> Result<ReportGate, String> {
    if was01 != Some("1") || cluster != Some("1") {
        return Ok(ReportGate::Offline);
    }
    if !matches!(mode, None | Some("create")) {
        return Err("FS_TEST_NAMESPACE_MODE must be create for the WAS-01 ac3 report; shared is unsupported".into());
    }
    Ok(ReportGate::Cluster(Kubeconfigs::parse(kubeconfig)?))
}
impl Runner for Option<ProcessKubectl> {
    fn run(&self, args: &[String]) -> Result<Output, Reason> {
        self.as_ref().ok_or(Reason::ProbeError)?.run(args)
    }
}
struct OwnedNamespace<'a, R: Runner> {
    runner: &'a R,
    delete: Vec<String>,
    owned: bool,
}
impl<R: Runner> OwnedNamespace<'_, R> {
    fn close(&mut self) -> Result<(), String> {
        if !std::mem::replace(&mut self.owned, false) {
            return Ok(());
        }
        match self.runner.run(&self.delete) {
            Ok(output) if output.exit_code == Some(0) => Ok(()),
            _ => Err("namespace cleanup failed; manual operator cleanup required".into()),
        }
    }
}
impl<R: Runner> Drop for OwnedNamespace<'_, R> {
    fn drop(&mut self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.close()));
    }
}
fn save(report: &Report, path: &Path) -> Result<(), String> {
    std::fs::write(path, report.render()).map_err(|_| "cannot write WAS-01 report".into())
}

pub fn run(runner: &impl Runner, gate: &ReportGate, directory: &Path) -> Result<PathBuf, String> {
    let namespace = Namespace::new(&super::namespace::namespace_name("was01"))?;
    let commands = plan(&namespace);
    let mut report = Report::new();
    if !plan_safe(&namespace, &commands) {
        report.record(
            "plan-namespaced-only",
            Verdict::Failed,
            Reason::RejectedPlan,
            None,
        )?;
        return Err("plan-rejected".into());
    }
    report.record(
        "plan-namespaced-only",
        Verdict::Passed,
        Reason::PlanSafe,
        Some("allow-listed-plan"),
    )?;
    std::fs::create_dir_all(directory).map_err(|_| "cannot create WAS-01 report directory")?;
    let path = directory.join(format!("{}.report", namespace.name()));
    save(&report, &path)?;
    if matches!(gate, ReportGate::Offline) {
        return Ok(path);
    }
    match runner.run(&commands[0]) {
        Ok(output) if output.exit_code == Some(0) => (),
        _ => {
            return Err(
                "FS_TEST_NAMESPACE_MODE=create: namespace creation failed; no namespace is owned"
                    .into(),
            );
        }
    }
    let mut owned = OwnedNamespace {
        runner,
        delete: commands.last().cloned().ok_or("plan-rejected")?,
        owned: true,
    };
    let result = (|| -> Result<(), String> {
        probe_version(&mut report, runner)?;
        probe_permissions(&mut report, runner, &namespace)?;
        save(&report, &path)?;
        if Report::ac3_ids().iter().any(|id| {
            report.item(id).is_ok_and(|item| {
                matches!(
                    item.reason,
                    Reason::ProbeError | Reason::Forbidden | Reason::InvalidVersion
                )
            })
        }) {
            return Err("WAS-01 probe incomplete; see fixed reason codes in report".into());
        }
        Ok(())
    })();
    let cleanup = owned.close();
    cleanup?;
    result?;
    Ok(path)
}

/// Existing dependency tables and lockfile package names, pinned to b5e7330.
pub fn dependency_guard(root: &str, worker: &str, lock: &str) -> bool {
    fn tables(
        value: &toml::Value,
        prefix: &str,
        found: &mut serde_json::Map<String, serde_json::Value>,
    ) -> Option<()> {
        for (key, entry) in value.as_table()? {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            if ["dependencies", "dev-dependencies", "build-dependencies"].contains(&key.as_str()) {
                found.insert(path, serde_json::to_value(entry).ok()?);
            } else if entry.is_table() {
                tables(entry, &path, found)?;
            }
        }
        Some(())
    }
    fn dependencies(text: &str) -> Option<serde_json::Value> {
        let manifest: toml::Value = toml::from_str(text).ok()?;
        let mut found = serde_json::Map::new();
        tables(&manifest, "", &mut found)?;
        Some(serde_json::Value::Object(found))
    }
    let check = || -> Option<bool> {
        let baseline: serde_json::Value =
            serde_json::from_str(include_str!("was01_dependencies.json")).ok()?;
        let lock: toml::Value = toml::from_str(lock).ok()?;
        let mut packages = lock
            .get("package")?
            .as_array()?
            .iter()
            .map(|p| p.get("name")?.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        packages.sort();
        Some(
            dependencies(root)? == baseline["root"]
                && dependencies(worker)? == baseline["worker"]
                && serde_json::to_value(packages).ok()? == baseline["packages"],
        )
    };
    check() == Some(true)
}
