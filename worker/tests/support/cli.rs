use super::{Config, Kube, Namespace, namespace_name, pinned_image};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Whole-flow allowance for build and push; each step is further capped below.
const BUILD_PUSH_BUDGET: Duration = Duration::from_secs(45 * 60);
const PODMAN_QUERY: Duration = Duration::from_secs(60);
const PODMAN_BUILD: Duration = Duration::from_secs(30 * 60);
const PODMAN_PUSH: Duration = Duration::from_secs(10 * 60);
/// Above kubectl's own `--request-timeout`, so kubectl normally reports its own timeout first.
const KUBECTL_CALL: Duration = Duration::from_secs(30);
/// Cleanup has its own allowance and never inherits what an earlier timeout left over.
const KUBECTL_CLEANUP: Duration = Duration::from_secs(90);

pub trait Runner {
    /// Runs one subprocess. `timeout` is the remaining budget for this call: the process
    /// and its descendants are killed when it passes, and the call returns an error.
    fn run(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&Value>,
        timeout: Duration,
    ) -> Result<String, String>;
}

pub struct Cli<R> {
    pub runner: R,
}

impl<R: Runner> Cli<R> {
    pub fn execute(&self, config: &Config, mode: &super::NamespaceMode) -> Result<(), String> {
        let Config::Enabled { repo } = config else {
            return Ok(());
        };
        let run = namespace_name();
        let image = self.build_push(repo, &run)?;
        let spec = super::pod_spec(&image)?;
        let session = match mode {
            super::NamespaceMode::Create => super::Session::create(self, &run, &spec)?,
            super::NamespaceMode::Shared(_) => super::Session::create_in(self, &run, &spec, mode)?,
        };
        session.verify()?;
        session.close()
    }

    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    pub fn build_push(&self, repo: &str, run: &str) -> Result<String, String> {
        Config::parse(Some("1"), Some(repo))?;
        Namespace::new(run)?;
        let deadline = std::time::Instant::now() + BUILD_PUSH_BUDGET;
        let step = |cap: Duration| {
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(cap)
        };
        let rootless = self.runner.run(
            "podman",
            &["info", "--format", "{{.Host.Security.Rootless}}"],
            None,
            step(PODMAN_QUERY),
        )?;
        if rootless.trim() != "true" {
            return Err("rootless podman is required".into());
        }
        let (registry, _) = repo
            .split_once('/')
            .ok_or("FS_TEST_IMAGE_REPO must include an explicit registry")?;
        if !registry.contains('.') && !registry.contains(':') && registry != "localhost" {
            return Err("FS_TEST_IMAGE_REPO must include an explicit registry".into());
        }
        self.runner.run(
            "podman",
            &["login", "--get-login", registry],
            None,
            step(PODMAN_QUERY),
        )?;
        let tag = format!("{repo}:{run}");
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        self.runner.run(
            "podman",
            &[
                "build",
                "--file",
                root.join("worker/Containerfile")
                    .to_str()
                    .ok_or("non-UTF8 path")?,
                "--tag",
                &tag,
                root.to_str().ok_or("non-UTF8 path")?,
            ],
            None,
            step(PODMAN_BUILD),
        )?;
        let scratch = Scratch::new()?;
        let digestfile = scratch.0.join("digest");
        self.runner.run(
            "podman",
            &[
                "push",
                "--quiet",
                "--digestfile",
                digestfile.to_str().ok_or("non-UTF8 path")?,
                &tag,
                &format!("docker://{tag}"),
            ],
            None,
            step(PODMAN_PUSH),
        )?;
        let digest =
            fs::read_to_string(&digestfile).map_err(|_| "podman push did not produce a digest")?;
        let image = format!("{repo}@{}", digest.trim());
        if !pinned_image(&image) {
            return Err("podman push returned an invalid digest".into());
        }
        Ok(image)
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        let path = std::env::temp_dir().join(namespace_name());
        fs::create_dir(&path).map_err(|_| "cannot create private test scratch directory")?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl<R: Runner> Kube for Cli<R> {
    fn create_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        self.runner.run(
            "kubectl",
            &["--request-timeout=15s", "create", "-f", "-"],
            Some(&namespace.manifest()),
            KUBECTL_CALL,
        )?;
        Ok(())
    }

    fn create_pod(&self, namespace: &Namespace, pod: &Value) -> Result<(), String> {
        let name = pod["metadata"]["name"]
            .as_str()
            .ok_or("pod name is missing")?;
        Namespace::new(name)?;
        if pod["apiVersion"] != "v1"
            || pod["kind"] != "Pod"
            || pod["metadata"]["namespace"] != namespace.name()
            || pod["metadata"]["labels"]["falcone.test/run"] != name
        {
            return Err("pod identity must match its guarded namespace and run label".into());
        }
        super::validate_spec(&pod["spec"])?;
        self.runner.run(
            "kubectl",
            &[
                "--request-timeout=15s",
                "--namespace",
                namespace.name(),
                "create",
                "-f",
                "-",
            ],
            Some(pod),
            KUBECTL_CALL,
        )?;
        Ok(())
    }

    fn wait_terminal(
        &self,
        namespace: &Namespace,
        pod: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        Namespace::new(pod)?;
        let deadline = std::time::Instant::now() + timeout.min(Duration::from_secs(180));
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("worker pod terminal-phase wait timed out".into());
            }
            let request_timeout = format!(
                "--request-timeout={}ms",
                remaining.as_millis().clamp(1, 15000)
            );
            let phase = self.runner.run(
                "kubectl",
                &[
                    &request_timeout,
                    "--namespace",
                    namespace.name(),
                    "get",
                    "pod",
                    pod,
                    "-o",
                    "jsonpath={.status.phase}",
                ],
                None,
                remaining,
            )?;
            if matches!(phase.trim(), "Succeeded" | "Failed") {
                return Ok(phase.trim().into());
            }
            std::thread::sleep(
                Duration::from_millis(200)
                    .min(deadline.saturating_duration_since(std::time::Instant::now())),
            );
        }
    }

    fn logs(&self, namespace: &Namespace, pod: &str) -> Result<String, String> {
        Namespace::new(pod)?;
        self.runner.run(
            "kubectl",
            &[
                "--request-timeout=15s",
                "--namespace",
                namespace.name(),
                "logs",
                pod,
                "--container",
                "worker",
                "--limit-bytes=65536",
                "--tail=100",
            ],
            None,
            KUBECTL_CALL,
        )
    }

    fn delete_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        self.runner.run(
            "kubectl",
            &[
                "--request-timeout=60s",
                "delete",
                "namespace",
                namespace.name(),
                "--ignore-not-found=true",
                "--wait=true",
                "--timeout=60s",
            ],
            None,
            KUBECTL_CLEANUP,
        )?;
        Ok(())
    }
}

pub struct Process;

/// One allowance, counted from the moment a deadline passes, for everything `run` does
/// afterwards: finding descendants, signalling them, letting the pipe readers see EOF and
/// reaping the child. `run` therefore returns within `timeout` plus this grace and one poll.
const CLEANUP_GRACE: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(5);
/// Environment marker inherited by every descendant, used to find processes that left the
/// process group or were reparented. Its value is unique to one `run` call.
const TAG: &str = "FS_TEST_RUN_TAG";

impl Runner for Process {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&Value>,
        timeout: Duration,
    ) -> Result<String, String> {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        use std::io::{Read, Write};
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::sync::mpsc;
        use std::time::Instant;
        let error = || {
            format!(
                "{program} failed: verify the executable, ambient kubeconfig and rootless podman registry login; command output suppressed"
            )
        };
        let started = Instant::now();
        let deadline = started + timeout;
        let bytes = match input {
            Some(input) => {
                Some(serde_json::to_vec(input).map_err(|_| "cannot serialize test manifest")?)
            }
            None => None,
        };
        let piped_stdout = !(program == "podman" && args.first() != Some(&"info"));
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let tag = format!(
            "{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            started.elapsed().as_nanos()
        );
        let mut child = Command::new(program)
            .args(args)
            .env(TAG, &tag)
            // Own process group so the whole group can be signalled on timeout.
            .process_group(0)
            .stdin(if bytes.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            // Only info has useful stdout; in particular never capture login's username.
            .stdout(if piped_stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| error())?;
        let root = Pid::from_child(&child);
        // Pipe I/O runs on detached threads so that no read or write can outlive the deadline
        // by blocking `run`; a failed write shows up as the child's own failure.
        if let (Some(bytes), Some(mut stdin)) = (bytes, child.stdin.take()) {
            std::thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
            });
        }
        let (sender, output) = mpsc::channel();
        if let Some(mut stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let _ = stdout.read_to_end(&mut buffer);
                let _ = sender.send(buffer);
            });
        } else {
            drop(sender);
        }
        let timed_out = || {
            format!(
                "{program} timed out after {timeout:?}: process tree killed; command output suppressed"
            )
        };
        // Exit is observed without reaping (WNOWAIT): the zombie keeps the pid, and with it
        // the process group id, reserved until cleanup is done, so a group signal can never
        // reach an unrelated process that reused the number.
        let exit = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        loop {
            if waitid(WaitId::Pid(root), exit)
                .map_err(|_| error())?
                .is_some()
            {
                break;
            }
            if Instant::now() >= deadline {
                kill_tree(&mut child, &tag, Instant::now() + CLEANUP_GRACE);
                return Err(timed_out());
            }
            std::thread::sleep(POLL);
        }
        // The child exited, but a descendant may still hold the output pipe.
        let wait = deadline.saturating_duration_since(Instant::now());
        let stdout = match output.recv_timeout(wait) {
            Ok(stdout) => stdout,
            // No reader without a piped stdout.
            Err(mpsc::RecvTimeoutError::Disconnected) if !piped_stdout => Vec::new(),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.wait();
                return Err(error());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let cleanup = Instant::now() + CLEANUP_GRACE;
                // The unreaped child still pins its group, so the group is killed even though
                // the leader is gone; this reaches helpers that cleared their environment.
                kill_tree(&mut child, &tag, cleanup);
                // Give the readers a moment to see EOF; otherwise abandon them. They are
                // detached and end when the last pipe holder dies.
                let _ = output.recv_timeout(cleanup.saturating_duration_since(Instant::now()));
                return Err(timed_out());
            }
        };
        let status = child.wait().map_err(|_| error())?;
        if !status.success() {
            return Err(error());
        }
        String::from_utf8(stdout).map_err(|_| error())
    }
}

/// Kills the process group (also when only descendants are left), descendants found through
/// `/proc` and processes carrying the run tag, then reaps the child; all of it before
/// `cleanup`, the one absolute deadline for discovery, signalling and reaping.
///
/// Gaps (no privileges, no cgroup or subreaper): a descendant that leaves the process group,
/// clears its environment and is reparented before the scan escapes. So does one that forks
/// after the scan or while the scan runs out of time on a very large process table; a killed
/// one that sits in uninterruptible sleep stays until the kernel releases it. Such a process
/// can keep the detached pipe readers alive until it exits; this is accepted for test-only
/// code and bounded by never joining them.
pub fn kill_tree(child: &mut std::process::Child, tag: &str, cleanup: std::time::Instant) {
    use rustix::process::{Pid, Signal, kill_process, kill_process_group};
    let root = Pid::from_child(child);
    // The tail of the budget is reserved for reaping, so that a slow or oversized process
    // table can exhaust discovery without leaving the killed child unreaped. Descendants are
    // listed while their parents are alive, since killing the parents reparents them out of
    // reach.
    let reap = cleanup
        .checked_sub(CLEANUP_GRACE / 4)
        .unwrap_or_else(std::time::Instant::now);
    let scan = (std::time::Instant::now() + CLEANUP_GRACE / 4).min(reap);
    let descendants = descendants(root.as_raw_nonzero().get(), scan);
    let _ = kill_process_group(root, Signal::KILL);
    let _ = child.kill();
    for pid in descendants {
        if let Some(pid) = Pid::from_raw(pid) {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
    kill_tagged(tag, Some(root), reap);
    // SIGKILL was sent above, so the child is reaped at once; try at least once even when the
    // budget is already spent, and wait out the rest of it only for an unkillable child.
    loop {
        if !matches!(child.try_wait(), Ok(None)) || std::time::Instant::now() >= cleanup {
            return;
        }
        std::thread::sleep(POLL);
    }
}

/// Pids below `root` according to `/proc/<pid>/task/*/children` (best effort).
fn descendants(root: i32, until: std::time::Instant) -> Vec<i32> {
    let mut found = Vec::new();
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        if std::time::Instant::now() >= until {
            break;
        }
        let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for task in tasks.flatten() {
            if std::time::Instant::now() >= until {
                return found;
            }
            let Ok(children) = fs::read_to_string(task.path().join("children")) else {
                continue;
            };
            for child in children.split_whitespace().filter_map(|c| c.parse().ok()) {
                if !found.contains(&child) {
                    found.push(child);
                    queue.push(child);
                }
            }
        }
    }
    found
}

/// Kills every other process of this user whose environment carries the run tag.
fn kill_tagged(tag: &str, skip: Option<rustix::process::Pid>, until: std::time::Instant) {
    use rustix::process::{Pid, Signal, kill_process};
    let marker = format!("{TAG}={tag}");
    let Ok(entries) = fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        if std::time::Instant::now() >= until {
            return;
        }
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id()
            || skip.is_some_and(|skip| skip.as_raw_nonzero().get() as u32 == pid)
        {
            continue;
        }
        let Ok(environment) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if environment
            .split(|b| *b == 0)
            .any(|entry| entry == marker.as_bytes())
            && let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw)
        {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
}
