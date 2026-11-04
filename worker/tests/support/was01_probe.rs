//! The opt-in WAS-01 evidence run: version and namespaced-permission probes in a namespace the
//! run created, behind `FS_WAS01_CLUSTER=1`. Offline (the default) it makes no call at all.
//!
//! Nothing a cluster or kubectl prints reaches the report or an error: replies are parsed into
//! fixed reason codes, a recorded gitVersion passes a charset check first, and error text is
//! fixed. The permissions recorded are those of the probing identity only.

use super::cluster_evidence::{GitVersion, PLAN_ITEM, Reason, Report, VERSION_ITEM};
use super::was01_plan::{self, Call};
use super::{Kubeconfigs, Namespace, NamespaceMode};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Whole-run allowance; the namespace delete is not subject to it.
pub const RUN_BUDGET: Duration = Duration::from_secs(10 * 60);
const CALL_LIMIT: Duration = Duration::from_secs(30);
const DELETE_LIMIT: Duration = Duration::from_secs(90);
const OUTPUT_LIMIT: u64 = 64 * 1024;

/// What kubectl answered: its stdout and exit status. stderr is never kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub stdout: String,
    pub code: i32,
}

/// The only way the evidence run reaches a cluster. `Err` means kubectl could not be run to
/// completion (spawn failure, timeout); its text is never used.
pub trait Kubectl {
    fn call(&self, args: &[&str]) -> Result<Reply, String>;
}

// ---- parsers ---------------------------------------------------------------------------

/// The gitVersion of a `get --raw /version` body, charset-checked, or the reason it has none.
pub fn parse_version(body: &str) -> Result<String, Reason> {
    let value: Value = serde_json::from_str(body).map_err(|_| Reason::MalformedVersion)?;
    let version = value.get("gitVersion").ok_or(Reason::MissingVersion)?;
    let version = version.as_str().ok_or(Reason::MalformedVersion)?;
    GitVersion::parse(version)?;
    Ok(version.to_owned())
}

#[derive(Debug, PartialEq, Eq)]
pub enum CanI {
    Yes,
    No,
    Error,
}

/// `yes` passes only as the exact answer of a successful call. `no` is a denial whether kubectl
/// exits 0 or 1. Everything else is a probe error.
pub fn parse_can_i(reply: &Reply) -> CanI {
    let answer = reply.stdout.strip_suffix('\n').unwrap_or(&reply.stdout);
    match (reply.code, answer) {
        (0, "yes") => CanI::Yes,
        (0 | 1, "no") => CanI::No,
        _ => CanI::Error,
    }
}

// ---- the run ---------------------------------------------------------------------------

/// The namespace this run created. Dropping it deletes the namespace exactly once, also while
/// panicking; it exists only after a successful create.
struct Owned<'a, K: Kubectl + ?Sized> {
    kubectl: &'a K,
    namespace: Namespace,
    done: bool,
}

impl<K: Kubectl + ?Sized> Owned<'_, K> {
    fn close(mut self) -> Result<(), String> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if std::mem::replace(&mut self.done, true) {
            return Ok(());
        }
        let call = was01_plan::delete_call(&self.namespace);
        let args: Vec<&str> = call.iter().map(String::as_str).collect();
        match self.kubectl.call(&args) {
            Ok(reply) if reply.code == 0 => Ok(()),
            _ => Err(format!(
                "WAS-01 evidence: deleting {} failed; delete it manually",
                self.namespace.name()
            )),
        }
    }
}

impl<K: Kubectl + ?Sized> Drop for Owned<'_, K> {
    fn drop(&mut self) {
        if let Err(message) = self.cleanup() {
            eprintln!("{message}");
        }
    }
}

fn exec<K: Kubectl + ?Sized>(
    kubectl: &K,
    namespace: &Namespace,
    call: &Call,
) -> Result<Option<Reply>, String> {
    // The plan was checked as a whole; each call is checked again on its way out.
    if !was01_plan::call_allowed(namespace, call) {
        return Err("plan-namespaced-only failed: a call is outside this run's namespace".into());
    }
    let args: Vec<&str> = call.iter().map(String::as_str).collect();
    Ok(kubectl.call(&args).ok())
}

fn probe_version(report: &mut Report, reply: Option<Reply>) {
    let outcome = match reply {
        None => Err(Reason::ProbeError),
        Some(reply) if reply.code == 0 => parse_version(&reply.stdout).and_then(|version| {
            GitVersion::parse(&version)?
                .target()
                .map(|_| version)
                .ok_or(Reason::UnsupportedVersion)
        }),
        Some(reply) if reply.stdout.contains("Forbidden") => Err(Reason::Forbidden),
        Some(_) => Err(Reason::ProbeError),
    };
    let recorded = match outcome {
        Ok(version) => report.set_passed(VERSION_ITEM, &version),
        Err(reason) => report.set_unverified(VERSION_ITEM, reason),
    };
    recorded.expect("version item accepts the reasons it is given");
}

fn probe_can_i(report: &mut Report, item: &str, reply: Option<Reply>) {
    let recorded = match reply.as_ref().map_or(CanI::Error, parse_can_i) {
        CanI::Yes => report.set_passed(item, "yes"),
        CanI::No => report.set_unverified(item, Reason::Denied),
        CanI::Error => report.set_unverified(item, Reason::ProbeError),
    };
    recorded.expect("permission items accept the reasons they are given");
}

/// Creates `namespace`, probes, deletes it. The plan is checked before the first call. After a
/// failed create nothing is deleted (the run does not own the name); after a successful one the
/// last call is the delete, exactly once, on every path. The permissions are the kubeconfig
/// identity's, not the deployment identity's.
pub fn run<K: Kubectl + ?Sized>(
    kubectl: &K,
    namespace: &Namespace,
    budget: Duration,
) -> Result<Report, String> {
    let plan = was01_plan::build_plan(namespace);
    was01_plan::plan_namespaced_only(namespace, &plan).map_err(str::to_owned)?;
    let mut report = Report::new();
    report
        .set_passed(PLAN_ITEM, &format!("calls={}", plan.len()))
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + budget;

    match exec(kubectl, namespace, &plan[0])? {
        Some(reply) if reply.code == 0 => {}
        Some(_) => return Err("WAS-01 evidence: the namespace was not created".into()),
        None => {
            return Err(format!(
                "WAS-01 evidence: creating {} did not complete; check for it and delete it manually",
                namespace.name()
            ));
        }
    }
    let guard = Owned {
        kubectl,
        namespace: namespace.clone(),
        done: false,
    };
    let probed = (|| {
        for call in &plan[1..plan.len() - 1] {
            if Instant::now() >= deadline {
                return Err("WAS-01 evidence: the time budget ran out".to_owned());
            }
            let reply = exec(kubectl, namespace, call)?;
            match was01_plan::can_i_item_of(call) {
                Some(item) => probe_can_i(&mut report, item, reply),
                None => probe_version(&mut report, reply),
            }
        }
        Ok(())
    })();
    let closed = guard.close();
    probed?;
    closed?;
    Ok(report)
}

// ---- the gate and the report file ------------------------------------------------------

pub enum Gate {
    Offline,
    Cluster { kubeconfigs: Kubeconfigs },
}

/// Opt-in only for the exact value `1`. With the gate on, shared mode and an unusable
/// KUBECONFIG are errors: this run creates and deletes its own namespace and has no shared mode.
pub fn gate(
    flag: Option<&str>,
    kubeconfig: Option<&str>,
    mode: Option<&str>,
    namespace: Option<&str>,
) -> Result<Gate, String> {
    if flag != Some("1") {
        return Ok(Gate::Offline);
    }
    if NamespaceMode::parse(mode, namespace)? != NamespaceMode::Create {
        return Err(
            "UNVERIFIED: FS_WAS01_CLUSTER=1 creates its own namespace and does not run with FS_TEST_NAMESPACE_MODE=shared"
                .into(),
        );
    }
    Ok(Gate::Cluster {
        kubeconfigs: Kubeconfigs::parse(kubeconfig)?,
    })
}

/// Writes the report to a new, uniquely named file in `dir` and returns its path.
pub fn write_report(dir: &Path, report: &Report) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "was01-evidence-{}-{}.report",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    std::fs::create_dir_all(dir).map_err(|_| "cannot create the report directory")?;
    let path = dir.join(name);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| file.write_all(report.to_text().as_bytes()))
        .map_err(|_| "cannot write the report")?;
    Ok(path)
}

/// Offline: the all-unverified report, with no runner built. With the gate on: the probe run
/// through the runner `runner` builds from the validated kubeconfig. Either way the report is
/// written under `dir`.
pub fn execute<K: Kubectl>(
    gate: Gate,
    dir: &Path,
    runner: impl FnOnce(&Kubeconfigs) -> K,
) -> Result<PathBuf, String> {
    let report = match gate {
        Gate::Offline => Report::new(),
        Gate::Cluster { kubeconfigs } => run(
            &runner(&kubeconfigs),
            &was01_plan::run_namespace()?,
            RUN_BUDGET,
        )?,
    };
    write_report(dir, &report)
}

// ---- the real runner -------------------------------------------------------------------

/// kubectl as a subprocess: no shell, only the merged kubeconfig of `Kubeconfigs`, stdin closed,
/// stderr discarded, stdout bounded, and a hard deadline for the whole process group.
pub struct ProcessKubectl {
    command: Vec<String>,
    kubeconfigs: Kubeconfigs,
    limit: Duration,
    delete_limit: Duration,
}

impl ProcessKubectl {
    pub fn new(kubeconfigs: &Kubeconfigs) -> Self {
        Self::with_command(&["kubectl"], kubeconfigs.clone(), CALL_LIMIT, DELETE_LIMIT)
    }

    /// `command` is the program and its leading arguments.
    pub fn with_command(
        command: &[&str],
        kubeconfigs: Kubeconfigs,
        limit: Duration,
        delete_limit: Duration,
    ) -> Self {
        Self {
            command: command.iter().map(|part| (*part).to_owned()).collect(),
            kubeconfigs,
            limit,
            delete_limit,
        }
    }
}

impl Kubectl for ProcessKubectl {
    fn call(&self, args: &[&str]) -> Result<Reply, String> {
        use std::os::unix::process::CommandExt;
        use std::sync::mpsc;
        let delete = args.first() == Some(&"delete");
        let (request, limit) = if delete {
            ("--request-timeout=60s", self.delete_limit)
        } else {
            ("--request-timeout=15s", self.limit)
        };
        let flags = self.kubeconfigs.kubectl_flags()?;
        let (program, leading) = self.command.split_first().ok_or("no kubectl command")?;
        let mut child = Command::new(program)
            .args(leading)
            .args(&flags)
            .arg(request)
            .args(args)
            // Own process group, so a timeout can take down credential plugins too.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "kubectl could not be started")?;
        let (sender, output) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let _ = stdout.take(OUTPUT_LIMIT).read_to_end(&mut buffer);
                let _ = sender.send(buffer);
            });
        }
        let deadline = Instant::now() + limit;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => {
                    kill_group(&mut child);
                    return Err("kubectl timed out".into());
                }
            }
        };
        // A descendant may still hold the pipe; wait only for what is left of the allowance.
        let wait = deadline.saturating_duration_since(Instant::now());
        let stdout = output
            .recv_timeout(wait.max(Duration::from_millis(250)))
            .map_err(|_| "kubectl output was not closed")?;
        Ok(Reply {
            stdout: String::from_utf8(stdout).map_err(|_| "kubectl output is not UTF-8")?,
            code: status.code().unwrap_or(-1),
        })
    }
}

fn kill_group(child: &mut std::process::Child) {
    use rustix::process::{Pid, Signal, kill_process_group};
    let _ = kill_process_group(Pid::from_child(child), Signal::KILL);
    let _ = child.kill();
    let _ = child.wait();
}
