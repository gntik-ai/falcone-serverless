use super::{Config, Kube, Namespace, namespace_name, pinned_image};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub trait Runner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&Value>,
        deadline: Instant,
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
        let deadline = Instant::now() + Duration::from_secs(1800);
        let rootless = self.runner.run(
            "podman",
            &["info", "--format", "{{.Host.Security.Rootless}}"],
            None,
            deadline,
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
            deadline,
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
            deadline,
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
            deadline,
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
    fn create_namespace(&self, namespace: &Namespace, deadline: Instant) -> Result<(), String> {
        let request_timeout = request_timeout(deadline, Duration::from_secs(15))?;
        self.runner.run(
            "kubectl",
            &[&request_timeout, "create", "-f", "-"],
            Some(&namespace.manifest()),
            deadline,
        )?;
        Ok(())
    }

    fn create_pod(
        &self,
        namespace: &Namespace,
        pod: &Value,
        deadline: Instant,
    ) -> Result<(), String> {
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
        let request_timeout = request_timeout(deadline, Duration::from_secs(15))?;
        self.runner.run(
            "kubectl",
            &[
                &request_timeout,
                "--namespace",
                namespace.name(),
                "create",
                "-f",
                "-",
            ],
            Some(pod),
            deadline,
        )?;
        Ok(())
    }

    fn wait_terminal(
        &self,
        namespace: &Namespace,
        pod: &str,
        deadline: Instant,
    ) -> Result<String, String> {
        Namespace::new(pod)?;
        let deadline = deadline.min(Instant::now() + Duration::from_secs(180));
        loop {
            let request_timeout = request_timeout(deadline, Duration::from_secs(15))?;
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
                deadline,
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

    fn logs(&self, namespace: &Namespace, pod: &str, deadline: Instant) -> Result<String, String> {
        Namespace::new(pod)?;
        let request_timeout = request_timeout(deadline, Duration::from_secs(15))?;
        self.runner.run(
            "kubectl",
            &[
                &request_timeout,
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
            deadline,
        )
    }

    fn delete_namespace(&self, namespace: &Namespace, deadline: Instant) -> Result<(), String> {
        let request_timeout = request_timeout(deadline, Duration::from_secs(60))?;
        self.runner.run(
            "kubectl",
            &[
                &request_timeout,
                "delete",
                "namespace",
                namespace.name(),
                "--ignore-not-found=true",
                "--wait=true",
                "--timeout=60s",
            ],
            None,
            deadline,
        )?;
        Ok(())
    }
}

fn request_timeout(deadline: Instant, maximum: Duration) -> Result<String, String> {
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .min(maximum);
    // kubectl interprets zero as unbounded; sub-millisecond budgets cannot be used.
    if remaining.as_millis() == 0 {
        return Err("test command deadline expired".into());
    }
    Ok(format!("--request-timeout={}ms", remaining.as_millis()))
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
        use rustix::process::{Pid, set_child_subreaper};
        use std::io::{Read, Write};
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let error = || {
            format!(
                "{program} failed: verify the executable, ambient kubeconfig and rootless podman registry login; command output suppressed"
            )
        };
        if Instant::now() >= deadline {
            return Err(error());
        }
        let bytes = input
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| error())?;
        // Reap orphaned grandchildren, scoped to this child's new process group.
        set_child_subreaper(Some(Pid::from_raw(1).unwrap())).map_err(|_| error())?;
        if Instant::now() >= deadline {
            return Err(error());
        }
        let child = Command::new(program)
            .args(args)
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
        let mut group = ProcessGroup {
            pid: Pid::from_child(&child),
            child,
        };
        let reader = group.child.stdout.take().map(|mut stdout| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).map(|_| bytes)
            })
        });
        // A credential helper may also hang without reading stdin. Writing on a
        // thread keeps manifest delivery under the same deadline as output/wait.
        let writer = group
            .child
            .stdin
            .take()
            .zip(bytes)
            .map(|(mut stdin, bytes)| std::thread::spawn(move || stdin.write_all(&bytes)));
        let result = loop {
            if Instant::now() >= deadline {
                break Err(error());
            }
            match group.child.try_wait() {
                Ok(Some(status))
                    if reader.as_ref().is_none_or(|r| r.is_finished())
                        && writer.as_ref().is_none_or(|w| w.is_finished()) =>
                {
                    break Ok(status);
                }
                Ok(_) => {}
                Err(_) => break Err(error()),
            }
            std::thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        };
        // Kill the group before joining pipe threads: descendants may retain a
        // pipe even after the direct child exits. Always reap before returning.
        drop(group);
        let output = reader
            .map(|r| r.join().map_err(|_| error())?.map_err(|_| error()))
            .transpose();
        let written = writer
            .map(|w| w.join().map_err(|_| error())?.map_err(|_| error()))
            .transpose();
        let status = result?;
        written?;
        let output = output?.unwrap_or_default();
        if !status.success() {
            return Err(error());
        }
        String::from_utf8(output).map_err(|_| error())
    }
}

struct ProcessGroup {
    child: std::process::Child,
    pid: rustix::process::Pid,
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        use rustix::process::{Signal, WaitOptions, kill_process_group, waitpgid};
        let _ = kill_process_group(self.pid, Signal::KILL);
        let _ = self.child.wait();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match waitpgid(self.pid, WaitOptions::NOHANG) {
                Ok(Some(_)) => continue,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(rustix::io::Errno::INTR) if Instant::now() < deadline => continue,
                _ => break,
            }
        }
    }
}
