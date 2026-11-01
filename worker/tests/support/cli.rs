use super::{Config, Kube, Namespace, namespace_name, pinned_image};
use serde_json::Value;
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Runs one external command. `deadline` bounds the whole call: an implementation must stop
/// the command and return an error at or shortly after it.
pub trait Runner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&Value>,
        deadline: Instant,
    ) -> Result<String, String>;
}

/// Total time allowed for one `execute` apart from namespace cleanup.
const DEFAULT_BUDGET: Duration = Duration::from_secs(45 * 60);
/// Own bound for namespace cleanup, which must run even when the budget is spent.
const DEFAULT_CLEANUP: Duration = Duration::from_secs(90);

pub struct Cli<R> {
    pub runner: R,
    budget: Duration,
    cleanup: Duration,
    /// End of the current `execute` budget; unset outside `execute`.
    limit: Cell<Option<Instant>>,
}

impl<R: Runner> Cli<R> {
    pub fn execute(&self, config: &Config, mode: &super::NamespaceMode) -> Result<(), String> {
        let Config::Enabled { repo } = config else {
            return Ok(());
        };
        self.limit.set(Some(Instant::now() + self.budget));
        let run = namespace_name();
        let image = self.build_push(repo, &run)?;
        let spec = super::pod_spec(&image)?;
        let session = match mode {
            super::NamespaceMode::Create => super::Session::create(self, &run, &spec)?,
            super::NamespaceMode::Shared(_) => super::Session::create_in(self, &run, &spec, mode)?,
        };
        let verified = session.verify();
        // Cleanup always runs with its own bound; its failure is reported next to any other.
        match (verified, session.close()) {
            (Ok(()), closed) => closed,
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
        }
    }

    pub fn new(runner: R) -> Self {
        Self {
            runner,
            budget: DEFAULT_BUDGET,
            cleanup: DEFAULT_CLEANUP,
            limit: Cell::new(None),
        }
    }

    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    pub fn with_cleanup_timeout(mut self, cleanup: Duration) -> Self {
        self.cleanup = cleanup;
        self
    }

    /// Deadline of one command: at most `cap` from now and never past the remaining budget.
    fn deadline(&self, cap: Duration) -> Instant {
        let deadline = Instant::now() + cap;
        self.limit
            .get()
            .map_or(deadline, |limit| limit.min(deadline))
    }

    pub fn build_push(&self, repo: &str, run: &str) -> Result<String, String> {
        Config::parse(Some("1"), Some(repo))?;
        Namespace::new(run)?;
        let rootless = self.runner.run(
            "podman",
            &["info", "--format", "{{.Host.Security.Rootless}}"],
            None,
            self.deadline(Duration::from_secs(60)),
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
            self.deadline(Duration::from_secs(60)),
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
            self.deadline(Duration::from_secs(30 * 60)),
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
            self.deadline(Duration::from_secs(10 * 60)),
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
            self.deadline(Duration::from_secs(30)),
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
            self.deadline(Duration::from_secs(30)),
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
        let deadline = self.deadline(timeout.min(Duration::from_secs(180)));
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
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
                // The request timeout is the inner bound; the process bound adds a grace.
                deadline.min(Instant::now() + Duration::from_secs(20)),
            )?;
            if matches!(phase.trim(), "Succeeded" | "Failed") {
                return Ok(phase.trim().into());
            }
            std::thread::sleep(
                Duration::from_millis(200).min(deadline.saturating_duration_since(Instant::now())),
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
            self.deadline(Duration::from_secs(30)),
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
            // Cleanup is bounded by itself, not by a budget that may already be spent.
            Instant::now() + self.cleanup,
        )?;
        Ok(())
    }
}

pub struct Process;

impl Runner for Process {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&Value>,
        deadline: Instant,
    ) -> Result<String, String> {
        use std::io::{Read, Write};
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::sync::mpsc;
        let error = || {
            format!(
                "{program} failed: verify the executable, ambient kubeconfig and rootless podman registry login; command output suppressed"
            )
        };
        let timed_out =
            || format!("{program} exceeded its deadline and was killed; command output suppressed");
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        let bytes = input
            .map(|input| serde_json::to_vec(input).map_err(|_| "cannot serialize test manifest"))
            .transpose()?;
        let mut child = Command::new(program)
            .args(args)
            // Own process group, so a timeout also reaches helpers such as credential plugins.
            .process_group(0)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            // Only info has useful stdout; in particular never capture login's username.
            .stdout(if program == "podman" && args.first() != Some(&"info") {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| error())?;
        // Pipes are served on helper threads so a full pipe cannot stall the deadline poll.
        let (wrote_tx, wrote) = mpsc::channel();
        if let (Some(bytes), Some(mut stdin)) = (bytes, child.stdin.take()) {
            std::thread::spawn(move || {
                let _ = wrote_tx.send(stdin.write_all(&bytes).is_ok());
            });
        } else {
            let _ = wrote_tx.send(true);
        }
        let (read_tx, read) = mpsc::channel();
        if let Some(mut stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let _ = read_tx.send(stdout.read_to_end(&mut buffer).map(|_| buffer));
            });
        } else {
            let _ = read_tx.send(Ok(Vec::new()));
        }
        let group = rustix::process::Pid::from_raw(child.id() as i32);
        let kill_group = |child: &mut std::process::Child| {
            if let Some(group) = group {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
            let _ = child.kill();
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    kill_group(&mut child);
                    let _ = child.wait();
                    return Err(timed_out());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    kill_group(&mut child);
                    let _ = child.wait();
                    return Err(error());
                }
            }
        };
        // A surviving helper can still hold a pipe open: bound the drain by the same deadline.
        let wait = deadline.saturating_duration_since(Instant::now());
        let output = read.recv_timeout(wait);
        let wrote = wrote.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let (Ok(output), Ok(wrote)) = (output, wrote) else {
            kill_group(&mut child);
            return Err(timed_out());
        };
        if !status.success() || !wrote {
            return Err(error());
        }
        String::from_utf8(output.map_err(|_| error())?).map_err(|_| error())
    }
}
