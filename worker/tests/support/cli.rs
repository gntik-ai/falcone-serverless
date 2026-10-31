use super::{Config, Kube, Namespace, NamespaceClient, namespace_name, pinned_image};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub trait Runner {
    fn run(&self, program: &str, args: &[&str], input: Option<&Value>) -> Result<String, String>;
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
        let rootless = self.runner.run(
            "podman",
            &["info", "--format", "{{.Host.Security.Rootless}}"],
            None,
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
        self.runner
            .run("podman", &["login", "--get-login", registry], None)?;
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

impl<R: Runner> NamespaceClient for Cli<R> {
    fn can_create_namespace(&self) -> Result<bool, String> {
        let response = self.runner.run(
            "kubectl",
            &["--request-timeout=15s", "create", "-f", "-", "-o", "json"],
            Some(&serde_json::json!({
                "apiVersion":"authorization.k8s.io/v1", "kind":"SelfSubjectAccessReview",
                "spec":{"resourceAttributes":{"verb":"create", "group":"", "resource":"namespaces"}}
            })),
        )?;
        let object: Value = serde_json::from_str(&response).map_err(|_| "invalid SSAR response")?;
        Ok(object["status"]["allowed"] == true && object["status"]["denied"] != true)
    }

    fn get_namespace(&self, namespace: &Namespace) -> Result<Value, String> {
        let response = self.runner.run(
            "kubectl",
            &[
                "--request-timeout=15s",
                "get",
                "namespace",
                namespace.name(),
                "-o",
                "json",
            ],
            None,
        )?;
        serde_json::from_str(&response).map_err(|_| "invalid namespace response".into())
    }

    fn cleanup_run(&self, namespace: &Namespace, run: &str) -> Result<(), String> {
        Namespace::new(run)?;
        let selector = format!("falcone.test/run={run}");
        let get = [
            "--request-timeout=15s",
            "--namespace",
            namespace.name(),
            "get",
            "pods",
            "--selector",
            &selector,
            "-o",
            "json",
        ];
        let response = self.runner.run("kubectl", &get, None)?;
        let objects: Value =
            serde_json::from_str(&response).map_err(|_| "invalid cleanup response")?;
        for object in objects["items"].as_array().ok_or("cleanup list missing")? {
            if object["metadata"]["labels"]["falcone.test/run"] != run {
                continue;
            }
            let name = object["metadata"]["name"]
                .as_str()
                .ok_or("owned pod name missing")?;
            if name != run {
                return Err("unexpected run-labelled pod name".into());
            }
            let uid = object["metadata"]["uid"]
                .as_str()
                .ok_or("owned pod UID missing")?;
            let version = object["metadata"]["resourceVersion"]
                .as_str()
                .ok_or("owned pod version missing")?;
            let path = format!("/api/v1/namespaces/{}/pods/{name}", namespace.name());
            self.runner.run("kubectl", &["--request-timeout=15s", "delete", "--raw", &path, "-f", "-"], Some(&serde_json::json!({
                "apiVersion":"v1", "kind":"DeleteOptions", "preconditions":{"uid":uid,"resourceVersion":version}
            })))?;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let response = self.runner.run("kubectl", &get, None)?;
            let objects: Value =
                serde_json::from_str(&response).map_err(|_| "invalid cleanup response")?;
            if objects["items"].as_array().is_some_and(Vec::is_empty) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err("run-labelled pods remain".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn create_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        self.runner.run(
            "kubectl",
            &["--request-timeout=15s", "create", "-f", "-"],
            Some(&namespace.manifest()),
        )?;
        Ok(())
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
        )?;
        Ok(())
    }
}

impl<R: Runner> Kube for Cli<R> {
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
        )
    }
}

pub struct Process;

impl Runner for Process {
    fn run(&self, program: &str, args: &[&str], input: Option<&Value>) -> Result<String, String> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let error = || {
            format!(
                "{program} failed: verify the executable, ambient kubeconfig and rootless podman registry login; command output suppressed"
            )
        };
        let mut child = Command::new(program)
            .args(args)
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
        if let Some(input) = input {
            let bytes = serde_json::to_vec(input).map_err(|_| "cannot serialize test manifest")?;
            let result = child.stdin.take().ok_or_else(error)?.write_all(&bytes);
            if result.is_err() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error());
            }
        }
        let output = child.wait_with_output().map_err(|_| error())?;
        if !output.status.success() {
            return Err(error());
        }
        String::from_utf8(output.stdout).map_err(|_| error())
    }
}
