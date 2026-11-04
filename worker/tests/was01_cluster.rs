//! WAS-01 ac1/ac2 on the shared cluster, and the offline qualification of its harness.
//!
//! Operator evidence (never a CI gate): FS_TEST_CLUSTER=1 KUBECONFIG=<files>
//! FS_TEST_WAS01_IMAGE=<repo>@sha256:<digest> cargo test --workspace --locked, optionally with
//! FS_TEST_NAMESPACE_MODE=shared and FS_TEST_NAMESPACE=<existing fs-test-* namespace>. Without
//! FS_TEST_CLUSTER the cluster test prints UNVERIFIED and makes no client or kubectl call.
//! The credential is never printed. SIGKILL can bypass cleanup.

#[path = "support/was01_mod.rs"]
mod support;

use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use support::kube_cluster::{KubeCluster, load_config, pod_view};
use support::observation::{
    Admission, Collector, Condition, ContainerView, Exit, Observation, Review,
};
use support::render::{self, BOOTSTRAP, FIXTURE, Phase, Run};
use support::was01::{
    DENIED, Evidence, Identity, Observed, Settings, Token, Was01Cluster, guarded_apply, qualify,
};
use support::{
    Kubeconfigs, Namespace, NamespaceApi, NamespaceMode, RUN_LABEL, final_uid, pinned_image,
    test_namespace,
};

const CONTAINERFILE: &str = include_str!("../qualification/Containerfile");
const IMAGE_UID: u64 = 65532;

fn image() -> String {
    format!("registry.invalid/was01@sha256:{}", "a".repeat(64))
}

// ---- the opt-in gate -------------------------------------------------------------------

#[derive(Debug)]
enum Gate {
    Disabled,
    Enabled {
        kubeconfigs: Kubeconfigs,
        image: String,
        mode: NamespaceMode,
    },
}

/// Every input is a parameter, so the gate needs no process environment and no client.
fn gate(
    cluster: Option<&str>,
    kubeconfig: Option<&str>,
    image: Option<&str>,
    mode: Option<&str>,
    namespace: Option<&str>,
) -> Result<Gate, String> {
    if cluster != Some("1") {
        return Ok(Gate::Disabled);
    }
    let image = image
        .filter(|image| pinned_image(image))
        .ok_or("UNVERIFIED: FS_TEST_WAS01_IMAGE must be <repo>@sha256:<digest>")?;
    let mode = NamespaceMode::parse(mode, namespace)?;
    let kubeconfigs = Kubeconfigs::parse(kubeconfig)?;
    Ok(Gate::Enabled {
        kubeconfigs,
        image: image.to_owned(),
        mode,
    })
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fs-test-was01-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn kubeconfig_text(server: &str, user: &str) -> String {
    json!({
        "apiVersion": "v1", "kind": "Config", "current-context": "ctx",
        "clusters": [{"name": "c", "cluster": {"server": server}}],
        "contexts": [{"name": "ctx", "context": {"cluster": "c", "user": user}}],
        "users": [{"name": user, "user": {"token": "placeholder-not-a-credential"}}]
    })
    .to_string()
}

#[test]
fn disabled_cluster_test_prints_unverified_and_builds_no_client() {
    for cluster in [None, Some("0"), Some("true")] {
        assert!(matches!(
            gate(cluster, Some(":"), None, None, None).unwrap(),
            Gate::Disabled
        ));
    }
}

#[test]
fn enabled_without_kubeconfig_or_image_is_unverified() {
    let dir = scratch("gate");
    let file = dir.join("config");
    std::fs::write(&file, kubeconfig_text("https://127.0.0.1:1", "u")).unwrap();
    let file = file.to_str().unwrap();
    for (kubeconfig, image) in [
        (None, Some(image())),
        (Some(file), None),
        (Some(file), Some("registry.invalid/was01:latest".to_owned())),
    ] {
        let error = gate(Some("1"), kubeconfig, image.as_deref(), None, None).unwrap_err();
        assert!(error.starts_with("UNVERIFIED"), "{error}");
    }
    assert!(matches!(
        gate(Some("1"), Some(file), Some(&image()), None, None).unwrap(),
        Gate::Enabled { .. }
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- the KUBECONFIG loader -------------------------------------------------------------

#[test]
fn kubeconfig_lists_with_empty_or_unreadable_entries_are_unverified() {
    let dir = scratch("loader");
    let existing = dir.join("config");
    std::fs::write(&existing, kubeconfig_text("https://127.0.0.1:1", "u")).unwrap();
    let existing = existing.to_str().unwrap().to_owned();
    let trailing = format!("{existing}:");
    let leading = format!(":{existing}");
    let directory = dir.to_str().unwrap().to_owned();
    let missing = format!("{existing}.missing");
    for value in [
        None,
        Some(""),
        Some("  "),
        Some(":"),
        Some("::"),
        Some(trailing.as_str()),
        Some(leading.as_str()),
        Some(directory.as_str()),
        Some(missing.as_str()),
    ] {
        let error = Kubeconfigs::parse(value).unwrap_err();
        assert!(error.starts_with("UNVERIFIED"), "{value:?}: {error}");
        assert!(!error.contains(&existing), "paths are not echoed: {error}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_unparseable_kubeconfig_is_unverified_and_not_echoed() {
    let dir = scratch("invalid");
    let file = dir.join("config");
    std::fs::write(
        &file,
        "clusters: [\n  not: [valid yaml: SECRET-LOOKING-VALUE",
    )
    .unwrap();
    let error = Kubeconfigs::parse(file.to_str()).unwrap_err();
    assert!(error.starts_with("UNVERIFIED"), "{error}");
    assert!(!error.contains("SECRET-LOOKING-VALUE"), "{error}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_readable_file_without_a_usable_cluster_is_unverified_at_the_loader() {
    let dir = scratch("unusable");
    let cases = [
        ("empty", String::new()),
        (
            "no-context",
            json!({"apiVersion": "v1", "kind": "Config",
                "clusters": [{"name": "c", "cluster": {"server": "https://x.invalid:6443"}}]})
            .to_string(),
        ),
        (
            "dangling-context",
            json!({"apiVersion": "v1", "kind": "Config", "current-context": "ctx",
                "contexts": [{"name": "ctx", "context": {"cluster": "gone", "user": "gone"}}]})
            .to_string(),
        ),
    ];
    for (name, text) in cases {
        let file = dir.join(name);
        std::fs::write(&file, text).unwrap();
        let value = file.to_str().unwrap();
        let error = Kubeconfigs::parse(Some(value)).unwrap_err();
        assert!(error.starts_with("UNVERIFIED"), "{name}: {error}");
        assert!(!error.contains(value), "paths are not echoed: {error}");
        assert!(gate(Some("1"), Some(value), Some(&image()), None, None).is_err());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Child half of the relative-path test: runs with a working directory the parent chose.
#[test]
fn relative_path_child() {
    let Some(dir) = std::env::var_os("FS_TEST_RELATIVE_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let value = std::env::var("KUBECONFIG").unwrap();
    assert!(Path::new(&value).is_relative(), "the list is relative");
    let list = Kubeconfigs::parse(Some(&value)).unwrap();
    let flags = list.kubectl_flags().unwrap();
    let on_disk = std::fs::read_to_string(flags[0].strip_prefix("--kubeconfig=").unwrap()).unwrap();
    // The client's view and the file kubectl reads must both point beneath the original
    // directory, whatever the merged file's own location is.
    for view in [
        serde_json::to_value(list.merged()).unwrap(),
        serde_json::from_str::<Value>(&on_disk).unwrap(),
    ] {
        let cluster = &view["clusters"][0]["cluster"];
        let user = &view["users"][0]["user"];
        let expected = |name: &str| dir.join("configs").join(name).to_str().unwrap().to_owned();
        assert_eq!(cluster["certificate-authority"], expected("ca.crt"));
        assert_eq!(user["client-certificate"], expected("client.crt"));
        assert_eq!(user["client-key"], expected("client.key"));
        assert_eq!(user["tokenFile"], expected("token"));
    }
    println!("relative-ok");
}

#[test]
fn relative_file_references_resolve_the_same_for_the_client_and_kubectl() {
    let dir = scratch("relative").canonicalize().unwrap();
    std::fs::create_dir_all(dir.join("configs")).unwrap();
    // Placeholder files: the loader must find each of them where the kubeconfig says.
    for name in ["ca.crt", "client.crt", "client.key", "token"] {
        std::fs::write(dir.join("configs").join(name), "placeholder\n").unwrap();
    }
    std::fs::write(
        dir.join("configs/config"),
        json!({"apiVersion": "v1", "kind": "Config", "current-context": "ctx",
            "clusters": [{"name": "c", "cluster": {
                "server": "https://rel.invalid:6443", "certificate-authority": "ca.crt"}}],
            "contexts": [{"name": "ctx", "context": {"cluster": "c", "user": "u"}}],
            "users": [{"name": "u", "user": {
                "client-certificate": "client.crt", "client-key": "client.key",
                "tokenFile": "token"}}]})
        .to_string(),
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "relative_path_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FS_TEST_RELATIVE_DIR", &dir)
        .env("KUBECONFIG", "configs/config")
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("relative-ok"), "{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// Two files that only work together: the first selects the context and names the cluster
/// and the user, but defines no user; the second defines that user and a conflicting cluster.
fn split_pair(dir: &std::path::Path) -> (String, String) {
    let first = dir.join("first");
    let second = dir.join("second");
    std::fs::write(
        &first,
        json!({"apiVersion": "v1", "kind": "Config", "current-context": "ctx",
            "clusters": [{"name": "c", "cluster": {"server": "https://first.invalid:6443"}}],
            "contexts": [{"name": "ctx", "context": {"cluster": "c", "user": "u"}}]})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &second,
        json!({"apiVersion": "v1", "kind": "Config",
            "clusters": [{"name": "c", "cluster": {"server": "https://second.invalid:6443"}}],
            "contexts": [{"name": "ctx", "context": {"cluster": "c", "user": "other"}}],
            "users": [{"name": "u", "user": {"token": "second-file-token"}}]})
        .to_string(),
    )
    .unwrap();
    (
        first.to_str().unwrap().to_owned(),
        second.to_str().unwrap().to_owned(),
    )
}

fn identity(config: &kube::Config) -> Value {
    serde_json::to_value(&config.auth_info).unwrap()
}

#[test]
fn every_listed_file_is_loaded_through_read_from_and_custom_config() {
    let dir = scratch("merge");
    let (first, second) = split_pair(&dir);
    let both = Kubeconfigs::parse(Some(&format!("{first}:{second}"))).unwrap();
    assert_eq!(both.paths().len(), 2);
    let config = block_on(load_config(&both)).unwrap();
    // The first file wins every name it defines, and the identity it selects only exists in
    // the second file: ignoring either file would change one of these.
    assert_eq!(config.cluster_url.host(), Some("first.invalid"));
    assert_eq!(identity(&config)["token"], "second-file-token");
    // The first file alone has no user, so its identity differs: the second file matters.
    let only_first = Kubeconfigs::parse(Some(&first)).unwrap();
    let alone = block_on(load_config(&only_first)).unwrap();
    assert_eq!(alone.cluster_url.host(), Some("first.invalid"));
    assert_ne!(identity(&alone), identity(&config));
    // The order is the precedence.
    let reversed = Kubeconfigs::parse(Some(&format!("{second}:{first}"))).unwrap();
    let config = block_on(load_config(&reversed)).unwrap();
    assert_eq!(config.cluster_url.host(), Some("second.invalid"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn kubectl_is_given_the_same_merged_configuration_as_the_client() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("kubectl");
    let (first, second) = split_pair(&dir);
    let list = Kubeconfigs::parse(Some(&format!("{first}:{second}"))).unwrap();
    let flags = list.kubectl_flags().unwrap();
    // One flag: repeating it would make kubectl use only the last file.
    assert_eq!(flags.len(), 1);
    assert_eq!(list.kubectl_flags().unwrap(), flags);
    let merged = PathBuf::from(flags[0].strip_prefix("--kubeconfig=").unwrap());
    assert!(merged.to_str() != Some(first.as_str()) && merged.to_str() != Some(second.as_str()));
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&merged), 0o600);
    assert_eq!(mode(merged.parent().unwrap()), 0o700);

    // The file selects the very cluster and identity the client does.
    let client = block_on(load_config(&list)).unwrap();
    let from_file = Kubeconfigs::parse(merged.to_str()).unwrap();
    let kubectl = block_on(load_config(&from_file)).unwrap();
    assert_eq!(kubectl.cluster_url, client.cluster_url);
    assert_eq!(identity(&kubectl), identity(&client));
    assert_eq!(kubectl.default_namespace, client.default_namespace);

    // And kubectl itself agrees, where it is installed.
    if let Ok(output) = std::process::Command::new("kubectl")
        .args(["config", "view", "--raw", "--minify", "-o", "json"])
        .arg(&flags[0])
        .env_remove("KUBECONFIG")
        .output()
        && output.status.success()
    {
        let view: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            view["clusters"][0]["cluster"]["server"],
            "https://first.invalid:6443"
        );
        assert_eq!(view["users"][0]["user"]["token"], "second-file-token");
    }

    drop(from_file);
    drop(list);
    assert!(
        !merged.exists(),
        "the merged file is removed with its owner"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Child half of the HOME canary: runs in a subprocess whose environment the parent controls.
#[test]
fn loader_canary_child() {
    if std::env::var_os("FS_TEST_LOADER_CANARY").is_none() {
        return;
    }
    let value = std::env::var("KUBECONFIG").ok();
    let error = Kubeconfigs::parse(value.as_deref()).unwrap_err();
    assert!(error.starts_with("UNVERIFIED"), "{error}");
    // Even the full cluster path never reaches an API call with an invalid list.
    assert!(gate(Some("1"), value.as_deref(), Some(&image()), None, None).is_err());
    println!("canary-ok");
}

#[test]
fn an_invalid_kubeconfig_never_loads_home_dot_kube_config() {
    let home = scratch("home");
    std::fs::create_dir_all(home.join(".kube")).unwrap();
    std::fs::write(
        home.join(".kube/config"),
        kubeconfig_text("https://ambient.invalid:6443", "ambient"),
    )
    .unwrap();
    for kubeconfig in [Some(":"), Some("::"), Some(""), None] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "loader_canary_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("FS_TEST_LOADER_CANARY", "1")
            .env("HOME", &home)
            .env_remove("KUBECONFIG");
        if let Some(value) = kubeconfig {
            command.env("KUBECONFIG", value);
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{kubeconfig:?}: {stdout}");
        assert!(stdout.contains("canary-ok"), "{kubeconfig:?}: {stdout}");
    }
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn the_loader_never_references_ambient_configuration() {
    let strip = |source: &str| -> String {
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let reader = strip(include_str!("support/kubeconfig.rs"));
    let client = strip(include_str!("support/kube_cluster.rs"));
    for banned in [
        "Config::infer",
        "Config::incluster",
        "Config::from_kubeconfig",
        "Kubeconfig::read(",
        "Client::try_default",
        "Config::from_cluster_env",
    ] {
        assert!(!reader.contains(banned), "{banned}");
        assert!(!client.contains(banned), "{banned}");
    }
    assert!(reader.contains("Kubeconfig::read_from"));
    assert!(client.contains("Config::from_custom_kubeconfig"));
}

// ---- the fake cluster ------------------------------------------------------------------

fn restricted() -> Option<BTreeMap<String, String>> {
    Some(BTreeMap::from([(
        "pod-security.kubernetes.io/enforce".to_owned(),
        "restricted".to_owned(),
    )]))
}

fn good_log(uid: u64) -> String {
    let mut lines = Vec::new();
    for check in [
        "engine_config",
        "compile_valid",
        "load_executable_memory",
        "run_valid",
        "interrupt_epoch",
        "interrupt_fuel",
        "root_fs_read_only",
        "tmp_writable",
        "non_root_uid",
    ] {
        let reason = if check == "non_root_uid" {
            format!("uid={uid} gid={uid} (effective)")
        } else {
            "ok".to_owned()
        };
        lines.push(
            json!({"check": check, "status": "passed", "reason": reason, "error_class": null})
                .to_string(),
        );
    }
    lines.join("\n") + "\n"
}

fn ready(status: &str) -> Vec<Condition> {
    vec![
        Condition {
            kind: "Initialized".into(),
            status: "True".into(),
        },
        Condition {
            kind: "Ready".into(),
            status: status.into(),
        },
    ]
}

#[derive(Default)]
struct Fake {
    events: RefCell<Vec<String>>,
    applied: RefCell<Vec<Value>>,
    deleted_runs: RefCell<Vec<String>>,
    uid: Cell<u64>,
    labels: Option<BTreeMap<String, String>>,
    deny_review: bool,
    forbid_bootstrap: bool,
    forbid_token: bool,
    panic_on_fixture: bool,
    conditions: Option<Vec<Condition>>,
}

impl Fake {
    fn shared() -> Self {
        Self {
            labels: restricted(),
            ..Self::default()
        }
    }

    fn log(&self, event: impl Into<String>) {
        self.events.borrow_mut().push(event.into());
    }

    fn events(&self) -> Vec<String> {
        self.events.borrow().clone()
    }
}

impl NamespaceApi for Fake {
    fn can_create_namespaces(&self) -> Result<bool, String> {
        self.log("admin:review namespaces");
        Ok(!self.deny_review)
    }
    fn create_namespace(&self, _: &Namespace) -> Result<(), String> {
        self.log("admin:create namespace");
        Ok(())
    }
    fn namespace_labels(&self, _: &Namespace) -> Result<Option<BTreeMap<String, String>>, String> {
        self.log("admin:get namespace");
        Ok(self.labels.clone())
    }
    fn delete_namespace(&self, _: &Namespace) -> Result<(), String> {
        self.log("admin:delete namespace");
        Ok(())
    }
    fn delete_run_objects(&self, _: &Namespace, run: &str) -> Result<(), String> {
        self.log("admin:delete run objects");
        self.deleted_runs.borrow_mut().push(run.into());
        Ok(())
    }
}

impl Was01Cluster for Fake {
    fn apply(
        &self,
        identity: &Identity<'_>,
        _: &Namespace,
        object: &Value,
    ) -> Result<Admission, String> {
        let kind = object["kind"].as_str().unwrap().to_owned();
        self.log(format!("{}:apply {kind}", identity.name()));
        match identity {
            Identity::Admin if self.forbid_bootstrap => return Err("forbidden".into()),
            Identity::Installer(token) => {
                assert_eq!(token.expose(), "installer-token");
                if self.panic_on_fixture {
                    panic!("exercise unwinding");
                }
            }
            Identity::Admin => {}
        }
        if kind == "Deployment" {
            self.uid.set(
                object["spec"]["template"]["spec"]["securityContext"]["runAsUser"]
                    .as_u64()
                    .unwrap(),
            );
        }
        self.applied.borrow_mut().push(object.clone());
        Ok(Admission {
            identity: identity.name(),
            kind,
            name: object["metadata"]["name"].as_str().unwrap().to_owned(),
            admitted: true,
            message: "created".into(),
        })
    }
    fn token_request(&self, _: &Namespace, service_account: &str) -> Result<Token, String> {
        self.log(format!("admin:token {service_account}"));
        if self.forbid_token {
            Err("forbidden".into())
        } else {
            Ok(Token::new("installer-token".into()))
        }
    }
    fn review_create(&self, token: &Token, _: &str, resource: &str) -> Result<bool, String> {
        assert_eq!(token.expose(), "installer-token");
        self.log(format!("installer:review {resource}"));
        Ok(false)
    }
    fn observe(&self, token: &Token, _: &Namespace, _: &str) -> Result<Observed, String> {
        assert_eq!(token.expose(), "installer-token");
        self.log("installer:observe");
        Ok(Observed {
            conditions: self.conditions.clone().unwrap_or_else(|| ready("True")),
            log: good_log(self.uid.get()),
            startup: None,
        })
    }
}

fn settings(mode: NamespaceMode) -> Settings {
    Settings {
        image: image(),
        mode,
        image_uid: IMAGE_UID,
    }
}

fn shared_mode() -> NamespaceMode {
    NamespaceMode::parse(Some("shared"), Some("fs-test-shared")).unwrap()
}

fn rng() -> impl FnMut() -> u64 {
    let mut next = 0;
    move || {
        next += 7919;
        next
    }
}

// ---- phases and identities -------------------------------------------------------------

#[test]
fn phases_run_in_order_with_the_right_identity() {
    let fake = Fake::default();
    let evidence = support::was01::run(&fake, &settings(NamespaceMode::Create), rng()).unwrap();
    qualify(&evidence).unwrap();
    let events = fake.events();
    let installer = events
        .iter()
        .find(|event| event.starts_with("admin:token"))
        .unwrap();
    assert!(
        installer.starts_with("admin:token was01-installer-"),
        "{installer}"
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event
                .split_once(' ')
                .map_or(event.as_str(), |(head, _)| head))
            .collect::<Vec<_>>(),
        [
            "admin:review",
            "admin:create",
            "admin:apply",
            "admin:apply",
            "admin:apply",
            "admin:token",
            "installer:apply",
            "installer:apply",
            "installer:review",
            "installer:review",
            "installer:review",
            "installer:review",
            "installer:observe",
            "admin:delete",
        ]
    );
    // Bootstrap objects only as admin, fixture objects only as the installer.
    let applied: Vec<_> = events
        .iter()
        .filter(|e| e.contains(":apply"))
        .cloned()
        .collect();
    assert_eq!(
        applied,
        [
            "admin:apply ServiceAccount",
            "admin:apply Role",
            "admin:apply RoleBinding",
            "installer:apply ServiceAccount",
            "installer:apply Deployment",
        ]
    );
    assert!(evidence.uid > 0 && evidence.uid != IMAGE_UID);
}

#[test]
fn wrong_phase_identity_is_refused_without_a_client_call() {
    let fake = Fake::default();
    let namespace = Namespace::new("fs-test-was01-x").unwrap();
    let token = Token::new("installer-token".into());
    let run = ctx();
    let bootstrap = render::render(Phase::Bootstrap, BOOTSTRAP[0], &run).unwrap();
    let fixture = render::render(Phase::Fixture, FIXTURE[0], &run).unwrap();
    let installer = Identity::Installer(&token);
    assert!(guarded_apply(&fake, Phase::Bootstrap, &installer, &namespace, &bootstrap).is_err());
    assert!(
        guarded_apply(
            &fake,
            Phase::Fixture,
            &Identity::Admin,
            &namespace,
            &fixture
        )
        .is_err()
    );
    assert!(fake.events().is_empty());
    assert!(guarded_apply(&fake, Phase::Fixture, &installer, &namespace, &fixture).is_ok());
    assert_eq!(format!("{token:?}"), "Token(<REDACTED>)");
}

#[test]
fn a_panic_in_phase_four_still_deletes_the_namespace() {
    let fake = Fake {
        panic_on_fixture: true,
        ..Fake::default()
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        support::was01::run(&fake, &settings(NamespaceMode::Create), rng())
    }));
    assert!(result.is_err());
    assert_eq!(fake.events().last().unwrap(), "admin:delete namespace");
}

#[test]
fn a_forbidden_bootstrap_or_token_request_is_unverified_with_no_fixture_applied() {
    for fake in [
        Fake {
            forbid_bootstrap: true,
            ..Fake::default()
        },
        Fake {
            forbid_token: true,
            ..Fake::default()
        },
    ] {
        let error =
            support::was01::run(&fake, &settings(NamespaceMode::Create), rng()).unwrap_err();
        assert!(error.starts_with("UNVERIFIED"), "{error}");
        assert!(
            !fake
                .events()
                .iter()
                .any(|event| event.starts_with("installer:"))
        );
        assert!(
            fake.applied
                .borrow()
                .iter()
                .all(|o| o["kind"] != "Deployment")
        );
        assert_eq!(fake.events().last().unwrap(), "admin:delete namespace");
    }
}

// ---- namespace modes -------------------------------------------------------------------

#[test]
fn create_mode_without_namespace_permission_fails_before_any_mutation() {
    let fake = Fake {
        deny_review: true,
        ..Fake::default()
    };
    let error = support::was01::run(&fake, &settings(NamespaceMode::Create), rng()).unwrap_err();
    assert!(error.starts_with("UNVERIFIED"), "{error}");
    assert!(error.contains("FS_TEST_NAMESPACE_MODE=shared"), "{error}");
    assert_eq!(fake.events(), ["admin:review namespaces"]);
}

#[test]
fn shared_mode_never_creates_or_deletes_a_namespace_and_labels_every_object() {
    let fake = Fake::shared();
    let evidence = support::was01::run(&fake, &settings(shared_mode()), rng()).unwrap();
    qualify(&evidence).unwrap();
    let events = fake.events();
    assert!(
        !events
            .iter()
            .any(|e| e.contains("create namespace") || e.contains("delete namespace"))
    );
    assert!(!events.iter().any(|e| e.starts_with("admin:review")));
    let runs = fake.deleted_runs.borrow();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    let applied = fake.applied.borrow();
    assert_eq!(applied.len(), 5);
    for object in applied.iter() {
        assert_eq!(object["metadata"]["namespace"], "fs-test-shared");
        assert_eq!(object["metadata"]["labels"][RUN_LABEL], run.as_str());
        assert!(
            object["metadata"]["name"]
                .as_str()
                .unwrap()
                .ends_with(&format!("-{run}")),
            "{object}"
        );
    }
    let deployment = applied.iter().find(|o| o["kind"] == "Deployment").unwrap();
    assert_eq!(
        deployment["spec"]["template"]["metadata"]["labels"][RUN_LABEL],
        run.as_str()
    );
    assert_eq!(
        deployment["spec"]["selector"]["matchLabels"][RUN_LABEL],
        run.as_str()
    );
}

#[test]
fn shared_mode_without_a_usable_namespace_applies_nothing() {
    for (mode, namespace) in [
        (Some("shared"), None),
        (Some("shared"), Some("")),
        (Some("shared"), Some("in-falcone-staging")),
        (Some("shared"), Some("kube-system")),
        (Some("shared"), Some("default")),
    ] {
        let error = NamespaceMode::parse(mode, namespace).unwrap_err();
        assert!(error.starts_with("UNVERIFIED"), "{namespace:?}: {error}");
    }
    for labels in [
        None,
        Some(BTreeMap::new()),
        Some(BTreeMap::from([(
            "pod-security.kubernetes.io/enforce".to_owned(),
            "privileged".to_owned(),
        )])),
    ] {
        let fake = Fake {
            labels,
            ..Fake::default()
        };
        let error = support::was01::run(&fake, &settings(shared_mode()), rng()).unwrap_err();
        assert!(error.starts_with("UNVERIFIED"), "{error}");
        assert_eq!(fake.events(), ["admin:get namespace"]);
        assert!(fake.applied.borrow().is_empty());
    }
}

#[test]
fn test_namespace_is_the_shared_entry_point_for_both_modes() {
    let fake = Fake::default();
    {
        let guard = test_namespace("suite", &fake, &NamespaceMode::Create).unwrap();
        assert!(guard.namespace().name().starts_with("fs-test-suite-"));
    }
    assert_eq!(
        fake.events(),
        [
            "admin:review namespaces",
            "admin:create namespace",
            "admin:delete namespace"
        ]
    );
    let fake = Fake::shared();
    let guard = test_namespace("suite", &fake, &shared_mode()).unwrap();
    let run = guard.run().to_owned();
    guard.close().unwrap();
    assert_eq!(
        fake.events(),
        ["admin:get namespace", "admin:delete run objects"]
    );
    assert_eq!(*fake.deleted_runs.borrow(), [run]);
}

// ---- qualification ---------------------------------------------------------------------

fn evidence_with(edit: impl FnOnce(&mut Evidence)) -> Result<(), String> {
    let uid = 1_000_770_000;
    let mut evidence = Evidence {
        uid,
        namespace: "fs-test-was01-x".into(),
        observation: Observation {
            conditions: ready("True"),
            log: good_log(uid),
            startup: None,
            admissions: (0..5)
                .map(|n| Admission {
                    identity: if n < 3 { "admin" } else { "installer" },
                    kind: "Kind".into(),
                    name: format!("n{n}"),
                    admitted: true,
                    message: "created".into(),
                })
                .collect(),
            reviews: DENIED
                .iter()
                .map(|(_, resource)| Review {
                    resource: (*resource).into(),
                    allowed: false,
                })
                .collect(),
        },
    };
    edit(&mut evidence);
    qualify(&evidence)
}

#[test]
fn qualification_requires_pod_ready_true() {
    evidence_with(|_| {}).unwrap();
    for conditions in [
        vec![],
        vec![Condition {
            kind: "Initialized".into(),
            status: "True".into(),
        }],
        ready("False"),
        ready("Unknown"),
    ] {
        let error = evidence_with(|e| e.observation.conditions = conditions).unwrap_err();
        assert!(error.contains("Ready"), "{error}");
    }
}

#[test]
fn qualification_requires_a_passed_tmp_round_trip_record() {
    for status in ["failed", "unverified"] {
        let error = evidence_with(|e| {
            e.observation.log = e
                .observation
                .log
                .lines()
                .map(|line| {
                    if line.contains("tmp_writable") {
                        line.replace("passed", status)
                    } else {
                        line.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
        })
        .unwrap_err();
        assert!(error.contains("tmp_writable"), "{error}");
    }
    let error = evidence_with(|e| {
        e.observation.log = e
            .observation
            .log
            .lines()
            .filter(|line| !line.contains("tmp_writable"))
            .collect::<Vec<_>>()
            .join("\n");
    })
    .unwrap_err();
    assert!(error.contains("tmp_writable: no record"), "{error}");
}

#[test]
fn qualification_checks_uid_admissions_and_the_denied_reviews() {
    let error = evidence_with(|e| e.uid += 1).unwrap_err();
    assert!(error.contains("non_root_uid"), "{error}");
    let error = evidence_with(|e| e.observation.admissions[4].admitted = false).unwrap_err();
    assert!(error.contains("admitted"), "{error}");
    let error = evidence_with(|e| e.observation.admissions.pop().map(drop).unwrap()).unwrap_err();
    assert!(error.contains("admitted"), "{error}");
    let error = evidence_with(|e| e.observation.reviews[2].allowed = true).unwrap_err();
    assert!(error.contains("runtimeclasses"), "{error}");
    let error = evidence_with(|e| e.observation.reviews.clear()).unwrap_err();
    assert!(error.contains("clusterroles"), "{error}");
    let error = evidence_with(|e| e.observation.log = "not json\n".into()).unwrap_err();
    assert!(error.contains("not JSON"), "{error}");
}

#[test]
fn a_failed_runner_check_fails_with_its_recorded_message() {
    let error = evidence_with(|e| {
        e.observation.log = e.observation.log.replace(
            r#"{"check":"load_executable_memory","error_class":null,"reason":"ok","status":"passed"}"#,
            r#"{"check":"load_executable_memory","error_class":"load","reason":"mmap denied","status":"failed"}"#,
        );
    })
    .unwrap_err();
    assert!(error.contains("mmap denied"), "{error}");
}

// ---- refusals --------------------------------------------------------------------------

fn ctx() -> Run {
    Run {
        namespace: "fs-test-was01-x".into(),
        run: "abc123".into(),
        image: image(),
        uid: 1_000_770_000,
        image_uid: IMAGE_UID,
    }
}

fn edited(template: &str, edit: impl FnOnce(&mut Value)) -> String {
    let mut value: Value = serde_json::from_str(template).unwrap();
    edit(&mut value);
    value.to_string()
}

fn remove(value: &mut Value, pointer: &str) {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    let key = key.replace("~1", "/").replace("~0", "~");
    value
        .pointer_mut(parent)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove(&key);
}

const CONTAINER: &str = "/spec/template/spec/containers/0/securityContext";
const POD: &str = "/spec/template/spec/securityContext";

#[test]
fn a_fixture_that_omits_or_overrides_a_posture_field_is_refused() {
    for pointer in [
        format!("{POD}/runAsNonRoot"),
        format!("{POD}/seccompProfile"),
        format!("{CONTAINER}/readOnlyRootFilesystem"),
        format!("{CONTAINER}/allowPrivilegeEscalation"),
        format!("{CONTAINER}/capabilities"),
        "/spec/template/spec/automountServiceAccountToken".to_owned(),
    ] {
        let template = edited(FIXTURE[1], |v| remove(v, &pointer));
        assert!(
            render::render(Phase::Fixture, &template, &ctx()).is_err(),
            "omitted {pointer}"
        );
    }
    for (pointer, value) in [
        (format!("{POD}/runAsNonRoot"), json!(false)),
        (format!("{POD}/runAsUser"), json!(0)),
        (format!("{POD}/runAsUser"), json!(1_000_770_000_u64)),
        (format!("{CONTAINER}/runAsUser"), json!(IMAGE_UID)),
        (format!("{CONTAINER}/runAsUser"), json!(1_000_770_000_u64)),
        (format!("{CONTAINER}/runAsUser"), json!(0)),
        (
            format!("{POD}/seccompProfile"),
            json!({"type": "Unconfined"}),
        ),
        (format!("{CONTAINER}/readOnlyRootFilesystem"), json!(false)),
        (format!("{CONTAINER}/allowPrivilegeEscalation"), json!(true)),
        (
            format!("{CONTAINER}/capabilities"),
            json!({"drop": ["NET_RAW"]}),
        ),
        (
            format!("{CONTAINER}/capabilities"),
            json!({"drop": ["ALL"], "add": ["NET_ADMIN"]}),
        ),
        (format!("{CONTAINER}/privileged"), json!(true)),
        (
            "/spec/template/spec/runtimeClassName".to_owned(),
            json!("gvisor"),
        ),
        ("/spec/template/spec/hostNetwork".to_owned(), json!(true)),
        ("/spec/template/spec/hostPID".to_owned(), json!(true)),
        ("/spec/template/spec/hostIPC".to_owned(), json!(true)),
        (
            "/spec/template/spec/volumes".to_owned(),
            json!([{"name": "h", "hostPath": {"path": "/"}}]),
        ),
        (
            "/spec/template/spec/volumes".to_owned(),
            json!([{"name": "tmp", "emptyDir": {}}]),
        ),
        (
            "/spec/template/spec/containers/0/image".to_owned(),
            json!("registry.invalid/was01:latest"),
        ),
        ("/metadata/namespace".to_owned(), json!("kube-system")),
        ("/spec/replicas".to_owned(), json!(3)),
    ] {
        let template = edited(FIXTURE[1], |v| {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            v.pointer_mut(parent).unwrap()[key] = value.clone();
        });
        assert!(
            render::render(Phase::Fixture, &template, &ctx()).is_err(),
            "{pointer}"
        );
    }
    // The control: the committed template renders.
    render::render(Phase::Fixture, FIXTURE[1], &ctx()).unwrap();
}

#[test]
fn the_guard_checks_the_rendered_uid_and_namespace() {
    let run = ctx();
    let rendered = render::render(Phase::Fixture, FIXTURE[1], &run).unwrap();
    render::guard(Phase::Fixture, &rendered, &run).unwrap();
    for uid in [json!(0), json!(IMAGE_UID), Value::Null] {
        let mut object = rendered.clone();
        if uid.is_null() {
            remove(&mut object, &format!("{POD}/runAsUser"));
        } else {
            object["spec"]["template"]["spec"]["securityContext"]["runAsUser"] = uid;
        }
        assert!(render::guard(Phase::Fixture, &object, &run).is_err());
    }
    let mut container_uid = rendered.clone();
    container_uid["spec"]["template"]["spec"]["containers"][0]["securityContext"]["runAsUser"] =
        json!(IMAGE_UID);
    assert!(render::guard(Phase::Fixture, &container_uid, &run).is_err());
    let mut elsewhere = rendered.clone();
    elsewhere["metadata"]["namespace"] = json!("in-falcone-staging");
    assert!(render::guard(Phase::Fixture, &elsewhere, &run).is_err());
    let mut unlabelled = rendered.clone();
    remove(&mut unlabelled, "/metadata/labels/falcone.test~1run");
    assert!(render::guard(Phase::Fixture, &unlabelled, &run).is_err());
}

#[test]
fn cluster_scoped_and_unlisted_kinds_are_refused() {
    let run = ctx();
    for (api, kind) in [
        ("rbac.authorization.k8s.io/v1", "ClusterRole"),
        ("rbac.authorization.k8s.io/v1", "ClusterRoleBinding"),
        ("node.k8s.io/v1", "RuntimeClass"),
        ("v1", "Namespace"),
        ("v1", "Pod"),
        ("v1", "Secret"),
    ] {
        for phase in [Phase::Bootstrap, Phase::Fixture] {
            let template =
                json!({"apiVersion": api, "kind": kind, "metadata": {"name": "x"}}).to_string();
            assert!(render::render(phase, &template, &run).is_err(), "{kind}");
        }
    }
    // A Role is a bootstrap kind only, a Deployment a fixture kind only.
    assert!(render::render(Phase::Fixture, BOOTSTRAP[1], &run).is_err());
    assert!(render::render(Phase::Bootstrap, FIXTURE[1], &run).is_err());
}

#[test]
fn refusals_fire_before_any_client_call() {
    for bad in [
        "registry.invalid/was01:latest".to_owned(),
        format!("registry.invalid/was01@sha256:{}", "a".repeat(63)),
    ] {
        for mode in [NamespaceMode::Create, shared_mode()] {
            let fake = Fake::shared();
            let error = support::was01::run(
                &fake,
                &Settings {
                    image: bad.clone(),
                    mode,
                    image_uid: IMAGE_UID,
                },
                rng(),
            )
            .unwrap_err();
            assert!(error.starts_with("refused"), "{error}");
            assert!(fake.events().is_empty());
        }
    }
}

// ---- committed files and the UID chooser -------------------------------------------------

#[test]
fn the_containerfile_is_digest_pinned_non_root_and_exec_form() {
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let mut froms = 0;
    for line in CONTAINERFILE.lines() {
        let line = words(line);
        match line
            .first()
            .map(|word| word.to_ascii_uppercase())
            .as_deref()
        {
            Some("FROM") => {
                froms += 1;
                let sha = line[1].split_once("@sha256:").map(|(_, hex)| hex);
                assert!(
                    sha.is_some_and(
                        |hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
                    ),
                    "{line:?}"
                );
            }
            Some("ENTRYPOINT") => {
                let rest = line[1..].join(" ");
                let parsed: Value = serde_json::from_str(&rest).expect("exec form");
                assert!(
                    parsed
                        .as_array()
                        .is_some_and(|a| a.len() == 1 && a[0].is_string())
                );
            }
            Some("RUN") if line.contains(&"cargo".to_owned()) => {
                assert!(line.contains(&"--locked".to_owned()), "{line:?}");
            }
            _ => {}
        }
    }
    assert_eq!(froms, 2);
    let uid = final_uid(CONTAINERFILE).unwrap();
    assert!(uid > 0);
    assert_eq!(uid, IMAGE_UID);
    assert!(CONTAINERFILE.contains("qualification_runner"));
    assert!(CONTAINERFILE.contains("--chmod=0555"));
    assert_eq!(
        CONTAINERFILE
            .lines()
            .filter(|l| l.starts_with("ENTRYPOINT"))
            .count(),
        1
    );
}

#[test]
fn the_committed_bootstrap_role_is_narrow() {
    let role: Value = serde_json::from_str(BOOTSTRAP[1]).unwrap();
    assert!(render::role_violations(&role).is_empty());
    let rules: Vec<(String, String, String)> = role["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| {
            let join = |key: &str| {
                rule[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            (join("apiGroups"), join("resources"), join("verbs"))
        })
        .collect();
    assert_eq!(
        rules,
        [
            (
                "".into(),
                "serviceaccounts".into(),
                "create,get,delete".into()
            ),
            (
                "apps".into(),
                "deployments".into(),
                "create,get,delete".into()
            ),
            ("".into(), "pods".into(), "get,list,watch".into()),
            ("".into(), "pods/log".into(), "get".into()),
        ]
    );
    for bad in [
        json!({"apiGroups": [""], "resources": ["*"], "verbs": ["get"]}),
        json!({"apiGroups": [""], "resources": ["pods"], "verbs": ["*"]}),
        json!({"apiGroups": ["*"], "resources": ["pods"], "verbs": ["get"]}),
        json!({"apiGroups": ["rbac.authorization.k8s.io"], "resources": ["roles"], "verbs": ["create"]}),
        json!({"apiGroups": ["rbac.authorization.k8s.io"], "resources": ["rolebindings"], "verbs": ["create"]}),
        json!({"apiGroups": [""], "resources": ["serviceaccounts/token"], "verbs": ["create"]}),
        json!({"apiGroups": [""], "resources": ["pods"], "verbs": ["bind"]}),
        json!({"apiGroups": [""], "resources": ["pods"], "verbs": ["escalate"]}),
    ] {
        let widened = json!({"kind": "Role", "rules": [bad]});
        assert!(!render::role_violations(&widened).is_empty(), "{widened}");
    }
}

#[test]
fn committed_manifests_render_and_the_workload_cannot_use_the_api() {
    let run = ctx();
    for template in BOOTSTRAP {
        render::render(Phase::Bootstrap, template, &run).unwrap();
    }
    for template in FIXTURE {
        let rendered = render::render(Phase::Fixture, template, &run).unwrap();
        if rendered["kind"] == "ServiceAccount" {
            assert_eq!(rendered["automountServiceAccountToken"], false);
        }
    }
    let binding = render::render(Phase::Bootstrap, BOOTSTRAP[2], &run).unwrap();
    assert_eq!(binding["subjects"][0]["name"], "was01-installer-abc123");
    assert_eq!(binding["roleRef"]["name"], "was01-installer-abc123");
}

#[test]
fn the_uid_chooser_never_returns_zero_or_the_image_user() {
    for _ in 0..1000 {
        let uid = render::choose_uid(IMAGE_UID, render::entropy);
        assert!(uid > 0 && uid != IMAGE_UID);
    }
    // Entropy that lands on the image USER is retried; the range never includes 0.
    let mut draws = [0_u64, 7].into_iter();
    let uid = render::choose_uid(1_000_000_000, move || draws.next().unwrap());
    assert_eq!(uid, 1_000_000_007);
    let mut draws = [7_u64, 8].into_iter();
    assert_eq!(
        render::choose_uid(1_000_000_007, move || draws.next().unwrap()),
        1_000_000_008
    );
}

// ---- the cluster test ------------------------------------------------------------------

#[test]
fn was01_restricted_cluster() -> Result<(), String> {
    let env = |key: &str| std::env::var(key).ok();
    let gate = gate(
        env("FS_TEST_CLUSTER").as_deref(),
        env("KUBECONFIG").as_deref(),
        env("FS_TEST_WAS01_IMAGE").as_deref(),
        env("FS_TEST_NAMESPACE_MODE").as_deref(),
        env("FS_TEST_NAMESPACE").as_deref(),
    )?;
    let Gate::Enabled {
        kubeconfigs,
        image,
        mode,
    } = gate
    else {
        println!("UNVERIFIED: FS_TEST_CLUSTER=1 is not set; the WAS-01 cluster test did not run");
        return Ok(());
    };
    let image_uid = final_uid(CONTAINERFILE)?;
    let cluster = KubeCluster::connect(&kubeconfigs)?;
    let settings = Settings {
        image,
        mode,
        image_uid,
    };
    let evidence = support::was01::run(&cluster, &settings, render::entropy)?;
    qualify(&evidence)?;
    // Afterwards the namespace is NotFound or Terminating (shared mode never owned it).
    if settings.mode == NamespaceMode::Create {
        let namespace = Namespace::new(&evidence.namespace)?;
        if !cluster.namespace_gone_or_terminating(&namespace)? {
            return Err("namespace still exists after cleanup".into());
        }
    }
    Ok(())
}

// ---- evidence kept when the runner fails before the pod is Ready -------------------------

fn crashed(code: i32) -> ContainerView {
    ContainerView {
        waiting_reason: Some("CrashLoopBackOff".into()),
        last_exit: Some(Exit {
            reason: "Error".into(),
            code,
        }),
        ..ContainerView::default()
    }
}

#[test]
fn a_runner_that_fails_before_ready_keeps_its_recorded_message() {
    let partial = [
        json!({"check": "engine_config", "status": "passed", "reason": "ok", "error_class": null}),
        json!({"check": "compile_valid", "status": "failed",
            "reason": "cannot compile the component", "error_class": "compile"}),
    ]
    .map(|record| record.to_string())
    .join("\n");
    let mut collector = Collector::default();
    // Not Ready, the container is already crash looping: the log is read all the same.
    collector.pod(ready("False"), crashed(1));
    assert!(collector.finished(9), "a failed container ends the wait");
    collector.log(partial.clone());
    // A later, emptier read of the restarted instance never discards it.
    collector.log(String::new());
    collector.log("{\"check\": \"engine_config\", \"status\": \"passed\"}".into());
    let observed = collector.into_observed();
    assert_eq!(observed.log, partial);

    let error = evidence_with(|evidence| {
        evidence.observation.conditions = observed.conditions.clone();
        evidence.observation.log = observed.log.clone();
        evidence.observation.startup = observed.startup.clone();
    })
    .unwrap_err();
    assert!(
        error.contains("compile_valid failed: cannot compile the component (compile)"),
        "{error}"
    );
    assert!(error.contains("pod Ready condition is False"), "{error}");
    assert!(error.contains("CrashLoopBackOff"), "{error}");
    assert!(error.contains("exit code 1"), "{error}");
}

#[test]
fn a_container_that_never_starts_is_reported_with_its_waiting_reason() {
    let mut collector = Collector::default();
    collector.pod(
        vec![],
        ContainerView {
            waiting_reason: Some("ImagePullBackOff".into()),
            ..ContainerView::default()
        },
    );
    assert!(!collector.container().has_output());
    assert!(collector.finished(9));
    let observed = collector.into_observed();
    let error = evidence_with(|evidence| {
        evidence.observation.conditions = observed.conditions.clone();
        evidence.observation.log = observed.log.clone();
        evidence.observation.startup = observed.startup.clone();
    })
    .unwrap_err();
    assert!(error.contains("ImagePullBackOff"), "{error}");
    assert!(error.contains("engine_config: no record"), "{error}");
}

#[test]
fn polling_continues_while_the_container_is_still_starting() {
    let mut collector = Collector::default();
    collector.pod(
        ready("False"),
        ContainerView {
            waiting_reason: Some("ContainerCreating".into()),
            ..ContainerView::default()
        },
    );
    assert!(!collector.finished(9));
    collector.pod(
        ready("False"),
        ContainerView {
            running: true,
            ..ContainerView::default()
        },
    );
    collector.log(good_log(1));
    // Complete, but the pod has not been Ready and the container still runs: keep waiting.
    assert!(!collector.finished(9));
    collector.pod(
        ready("True"),
        ContainerView {
            running: true,
            ..ContainerView::default()
        },
    );
    assert!(collector.finished(9));
}

#[test]
fn the_ready_snapshot_survives_the_runner_exiting() {
    let mut collector = Collector::default();
    collector.pod(
        ready("True"),
        ContainerView {
            running: true,
            ..ContainerView::default()
        },
    );
    collector.pod(
        ready("False"),
        ContainerView {
            terminated: Some(Exit {
                reason: "Completed".into(),
                code: 0,
            }),
            ..ContainerView::default()
        },
    );
    collector.log(good_log(1));
    let observed = collector.into_observed();
    assert!(
        observed
            .conditions
            .iter()
            .any(|c| c.kind == "Ready" && c.status == "True")
    );
}

#[test]
fn the_runner_container_state_is_read_from_the_pod_status() {
    let pod: k8s_openapi::api::core::v1::Pod = serde_json::from_value(json!({
        "metadata": {"name": "p"},
        "status": {
            "conditions": [{"type": "Ready", "status": "False"}],
            "containerStatuses": [
                {"name": "sidecar", "image": "i", "imageID": "", "ready": false,
                 "restartCount": 0, "state": {"running": {}}},
                {"name": "runner", "image": "i", "imageID": "", "ready": false, "restartCount": 3,
                 "state": {"waiting": {"reason": "CrashLoopBackOff"}},
                 "lastState": {"terminated": {"exitCode": 2, "reason": "Error"}}}
            ]
        }
    }))
    .unwrap();
    let (conditions, view) = pod_view(&pod);
    assert_eq!(conditions.len(), 1);
    assert_eq!(view, crashed(2));
}

// ---- WAS-01 ac1/ac3: version and namespaced-permission evidence report -----------------

mod evidence {
    use super::support::cluster_evidence::{
        self as model, ITEM_IDS, ModelError, PLAN_ITEM, RESOURCES, Reason, Report, Target, VERBS,
        VERSION_ITEM, Verdict,
    };
    use super::support::was01_deps;
    use super::support::was01_plan::{self, Call};
    use super::support::was01_probe::{
        self, CanI, Gate, Kubectl, ProcessKubectl, RUN_BUDGET, Reply,
    };
    use super::{Kubeconfigs, Namespace, kubeconfig_text, scratch};
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::time::Duration;

    const ENV_VARS: [&str; 4] = [
        "FS_WAS01_CLUSTER",
        "KUBECONFIG",
        "FS_TEST_NAMESPACE_MODE",
        "FS_TEST_NAMESPACE",
    ];
    const README: &str = include_str!("../../docs/qualification/was-01/README.md");

    fn ns(suffix: &str) -> Namespace {
        was01_plan::checked_namespace(&format!("fs-test-was01-{suffix}")).unwrap()
    }

    fn call(parts: &[&str]) -> Call {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn reply(stdout: &str, code: i32) -> Result<Reply, String> {
        Ok(Reply {
            stdout: stdout.to_owned(),
            code,
        })
    }

    fn version_body(git_version: &str) -> String {
        serde_json::json!({"major": "1", "minor": "37", "gitVersion": git_version}).to_string()
    }

    // ---- a fake kubectl at the system boundary ------------------------------------------

    type Responder = Box<dyn Fn(&[&str]) -> Result<Reply, String>>;

    struct Fake {
        calls: RefCell<Vec<Vec<String>>>,
        responder: Responder,
        panic_on: Option<&'static str>,
        count: Cell<usize>,
    }

    impl Fake {
        fn new(responder: impl Fn(&[&str]) -> Result<Reply, String> + 'static) -> Self {
            Self {
                calls: RefCell::default(),
                responder: Box::new(responder),
                panic_on: None,
                count: Cell::new(0),
            }
        }

        /// A cluster that is a healthy 1.37 and answers every probe `yes`.
        fn healthy() -> Self {
            Self::new(|args| match args {
                ["get", "--raw", "/version"] => reply(&version_body("v1.37.2"), 0),
                ["auth", "can-i", ..] => reply("yes\n", 0),
                _ => reply("", 0),
            })
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl Kubectl for Fake {
        fn call(&self, args: &[&str]) -> Result<Reply, String> {
            self.count.set(self.count.get() + 1);
            self.calls
                .borrow_mut()
                .push(args.iter().map(|a| (*a).to_owned()).collect());
            if self.panic_on == Some(args[0]) {
                panic!("exercise unwinding");
            }
            (self.responder)(args)
        }
    }

    fn deleted_last(fake: &Fake, namespace: &Namespace) {
        let calls = fake.calls();
        assert_eq!(calls.last().unwrap(), &was01_plan::delete_call(namespace));
        assert_eq!(
            calls.last().unwrap(),
            &call(&["delete", "namespace", namespace.name(), "--wait"])
        );
        assert_eq!(
            calls.iter().filter(|c| c[0] == "delete").count(),
            1,
            "exactly one delete"
        );
    }

    // ---- the model ----------------------------------------------------------------------

    #[test]
    fn a_new_item_is_unverified_and_passed_needs_evidence() {
        let mut report = Report::new();
        for id in ITEM_IDS {
            assert_eq!(
                report.item(id).unwrap().verdict,
                Verdict::Unverified,
                "{id}"
            );
        }
        assert_eq!(
            report.set_passed(VERSION_ITEM, ""),
            Err(ModelError::InvalidEvidence)
        );
        assert_eq!(
            report.set_passed("can-i-get-services", "no"),
            Err(ModelError::InvalidEvidence)
        );
        assert_eq!(
            report.set_passed("can-i-get-services", ""),
            Err(ModelError::InvalidEvidence)
        );
        assert_eq!(
            report.set_passed(PLAN_ITEM, "ok"),
            Err(ModelError::InvalidEvidence)
        );
        assert_eq!(
            report.set_passed("can-i-get-pods", "yes"),
            Err(ModelError::UnknownItem)
        );
        assert_eq!(
            report.set_unverified("nope", Reason::NotProbed),
            Err(ModelError::UnknownItem)
        );
        assert_eq!(
            report.set_failed("", Reason::CheckFailed),
            Err(ModelError::UnknownItem)
        );
        assert_eq!(report, Report::new(), "rejected sets change nothing");
        report.set_passed("can-i-get-services", "yes").unwrap();
        assert_eq!(
            report.item("can-i-get-services").unwrap().verdict,
            Verdict::Passed
        );
    }

    #[test]
    fn a_cell_is_failed_passed_or_unverified_from_its_ac3_items() {
        let mut report = Report::new();
        report.set_passed(VERSION_ITEM, "v1.37.2").unwrap();
        assert_eq!(report.target(), Some(Target::K8s137));
        let cell = report.cell(Target::K8s137);
        assert_eq!(
            (cell.verdict, cell.reason),
            (Verdict::Unverified, Some(Reason::ItemUnverified))
        );
        for verb in VERBS {
            for resource in RESOURCES {
                let id = model::can_i_item(verb, resource).unwrap();
                report.set_passed(id, "yes").unwrap();
            }
        }
        let cell = report.cell(Target::K8s137);
        assert_eq!((cell.verdict, cell.reason), (Verdict::Passed, None));
        // The other line has no server behind it.
        let other = report.cell(Target::K8s136);
        assert_eq!(
            (other.verdict, other.reason),
            (Verdict::Unverified, Some(Reason::NoMatchingServer))
        );
        report
            .set_failed("can-i-delete-services", Reason::CheckFailed)
            .unwrap();
        let cell = report.cell(Target::K8s137);
        assert_eq!(
            (cell.verdict, cell.reason),
            (Verdict::Failed, Some(Reason::ItemFailed))
        );
        // One unverified item keeps a cell from passing.
        report
            .set_unverified("can-i-delete-services", Reason::Denied)
            .unwrap();
        assert_eq!(report.cell(Target::K8s137).verdict, Verdict::Unverified);
    }

    #[test]
    fn versions_are_recorded_verbatim_and_map_to_their_cell() {
        for (version, target) in [
            ("v1.37.2", Target::K8s137),
            ("v1.36.4+k3s1", Target::K8s136),
        ] {
            let mut report = Report::new();
            assert_eq!(
                was01_probe::parse_version(&version_body(version)).as_deref(),
                Ok(version)
            );
            report.set_passed(VERSION_ITEM, version).unwrap();
            assert_eq!(
                report.item(VERSION_ITEM).unwrap().evidence.as_deref(),
                Some(version)
            );
            assert_eq!(report.target(), Some(target));
            assert!(
                report
                    .to_text()
                    .contains(&format!("item kubernetes-version passed - {version}\n"))
            );
        }
        assert_eq!(Target::K8s137.code(), "k8s-1.37");
        assert_eq!(Target::K8s136.code(), "k8s-1.36");
        // Valid versions of no cell: another line, a bigger number, a pre-release.
        for version in ["v1.35.0", "v1.370.1", "v1.37.0-rc.1", "v2.37.0", "v1.38.0"] {
            let mut report = Report::new();
            assert_eq!(
                report.set_passed(VERSION_ITEM, version),
                Err(ModelError::InvalidEvidence),
                "{version}"
            );
            assert_eq!(report.target(), None);
        }
    }

    #[test]
    fn a_missing_malformed_forbidden_or_failed_version_is_unverified_with_its_code() {
        let cases: [(Result<Reply, String>, Reason); 14] = [
            (
                reply(&version_body("v1.37.2+"), 0),
                Reason::MalformedVersion,
            ),
            (
                reply(&version_body("v1.37.2+k3s..1"), 0),
                Reason::MalformedVersion,
            ),
            (
                reply(&version_body("v1.37.2-.rc"), 0),
                Reason::MalformedVersion,
            ),
            (
                reply(&version_body("v1.37.2-rc.1+"), 0),
                Reason::MalformedVersion,
            ),
            (
                reply(&version_body("v1.37.2+k3s_1"), 0),
                Reason::MalformedVersion,
            ),
            (reply("{}", 0), Reason::MissingVersion),
            (reply("{\"gitVersion\": 137}", 0), Reason::MalformedVersion),
            (reply("not json", 0), Reason::MalformedVersion),
            (reply(&version_body("1.37.2"), 0), Reason::MalformedVersion),
            (reply(&version_body("v1.37"), 0), Reason::MalformedVersion),
            (
                reply(&version_body("v1.37.2\nx"), 0),
                Reason::MalformedVersion,
            ),
            (
                reply(&version_body("v1.35.0"), 0),
                Reason::UnsupportedVersion,
            ),
            (
                reply("Error from server (Forbidden): nope", 1),
                Reason::Forbidden,
            ),
            (Err("boom".into()), Reason::ProbeError),
        ];
        for (answer, reason) in cases {
            let fake = Fake::new(move |args| match args {
                ["get", ..] => answer.clone(),
                _ => reply("yes\n", 0),
            });
            let report = was01_probe::run(&fake, &ns("v"), RUN_BUDGET).unwrap();
            let item = report.item(VERSION_ITEM).unwrap();
            assert_eq!(
                (item.verdict, item.reason, item.evidence.as_deref()),
                (Verdict::Unverified, Some(reason), None)
            );
            assert_eq!(report.target(), None);
        }
        let fake = Fake::new(|args| match args {
            ["get", ..] => reply("garbage", 1),
            _ => reply("yes\n", 0),
        });
        let report = was01_probe::run(&fake, &ns("v"), RUN_BUDGET).unwrap();
        assert_eq!(
            report.item(VERSION_ITEM).unwrap().reason,
            Some(Reason::ProbeError)
        );
    }

    #[test]
    fn can_i_passes_only_on_an_exact_yes() {
        let answers = [
            (reply("yes\n", 0), CanI::Yes),
            (reply("yes", 0), CanI::Yes),
            (reply("no\n", 1), CanI::No),
            (reply("no\n", 0), CanI::No),
            (reply("yes\n", 1), CanI::Error),
            (reply("Yes\n", 0), CanI::Error),
            (reply("yes please\n", 0), CanI::Error),
            (reply("yes\nyes\n", 0), CanI::Error),
            (reply("", 1), CanI::Error),
            (reply("no\n", 2), CanI::Error),
            (reply("no, but\n", 1), CanI::Error),
        ];
        for (answer, expected) in answers {
            assert_eq!(was01_probe::parse_can_i(&answer.unwrap()), expected);
        }
        // Per resource and verb, through the run.
        let healthy = Fake::healthy();
        let report = was01_probe::run(&healthy, &ns("yes"), RUN_BUDGET).unwrap();
        for verb in VERBS {
            for resource in RESOURCES {
                let item = report
                    .item(model::can_i_item(verb, resource).unwrap())
                    .unwrap();
                assert_eq!(
                    (item.verdict, item.evidence.as_deref()),
                    (Verdict::Passed, Some("yes"))
                );
            }
        }
        assert_eq!(report.cell(Target::K8s137).verdict, Verdict::Passed);
        for (answer, reason) in [
            (reply("no\n", 1), Reason::Denied),
            (reply("no\n", 0), Reason::Denied),
            (reply("maybe\n", 0), Reason::ProbeError),
            (reply("", 1), Reason::ProbeError),
            (Err("runner exploded".into()), Reason::ProbeError),
        ] {
            let fake = Fake::new(move |args| match args {
                ["get", ..] => reply(&version_body("v1.36.4+k3s1"), 0),
                ["auth", ..] => answer.clone(),
                _ => reply("", 0),
            });
            let report = was01_probe::run(&fake, &ns("no"), RUN_BUDGET).unwrap();
            for verb in VERBS {
                for resource in RESOURCES {
                    let item = report
                        .item(model::can_i_item(verb, resource).unwrap())
                        .unwrap();
                    assert_eq!(
                        (item.verdict, item.reason, item.evidence.as_deref()),
                        (Verdict::Unverified, Some(reason), None)
                    );
                }
            }
            let cell = report.cell(Target::K8s136);
            assert_eq!(
                (cell.verdict, cell.reason),
                (Verdict::Unverified, Some(Reason::ItemUnverified))
            );
        }
    }

    // ---- the plan -----------------------------------------------------------------------

    #[test]
    fn the_run_namespace_is_an_fs_test_was01_dns_label() {
        for _ in 0..20 {
            let namespace = was01_plan::run_namespace().unwrap();
            let name = namespace.name();
            assert!(name.starts_with("fs-test-was01-"), "{name}");
            assert!(name.len() <= 63, "{name}");
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            );
            assert!(name.as_bytes().last().unwrap().is_ascii_alphanumeric());
        }
        for bad in [
            "fs-test-other-1",
            "fs-test-was01",
            "default",
            "in-falcone-staging",
            "fs-test-was01-UPPER",
            "fs-test-was01-x-",
            &format!("fs-test-was01-{}", "a".repeat(50)),
        ] {
            assert!(was01_plan::checked_namespace(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_built_plan_including_version_is_namespaced_only() {
        let namespace = ns("plan");
        let plan = was01_plan::build_plan(&namespace);
        assert_eq!(plan.len(), 2 + 12 + 1);
        assert_eq!(plan[0], call(&["create", "namespace", namespace.name()]));
        assert_eq!(plan[1], call(&["get", "--raw", "/version"]));
        assert_eq!(
            plan[2],
            call(&[
                "auth",
                "can-i",
                "get",
                "deployments.apps",
                "-n",
                namespace.name()
            ])
        );
        assert_eq!(
            plan.last().unwrap(),
            &call(&["delete", "namespace", namespace.name(), "--wait"])
        );
        assert_eq!(was01_plan::plan_namespaced_only(&namespace, &plan), Ok(()));
        assert!(was01_plan::plan_namespaced_only(&namespace, &[]).is_err());
    }

    #[test]
    fn plan_namespaced_only_fails_each_out_of_namespace_call() {
        let namespace = ns("guard");
        let name = namespace.name();
        let other = "fs-test-was01-other";
        let fixtures: Vec<(&str, Call)> = vec![
            (
                "-n default",
                call(&["auth", "can-i", "get", "services", "-n", "default"]),
            ),
            (
                "-n in-falcone-staging",
                call(&[
                    "auth",
                    "can-i",
                    "get",
                    "services",
                    "-n",
                    "in-falcone-staging",
                ]),
            ),
            ("-A", call(&["auth", "can-i", "get", "services", "-A"])),
            (
                "--all-namespaces",
                call(&["get", "services", "--all-namespaces"]),
            ),
            ("get without -n", call(&["get", "services"])),
            ("get nodes", call(&["get", "nodes"])),
            ("-n <run> get nodes", call(&["-n", name, "get", "nodes"])),
            ("get nodes -n <run>", call(&["get", "nodes", "-n", name])),
            ("clusterversion", call(&["get", "clusterversion"])),
            ("clusterrole write", call(&["create", "clusterrole", "x"])),
            (
                "clusterrolebinding write",
                call(&["create", "clusterrolebinding", "x"]),
            ),
            (
                "crd write",
                call(&["create", "customresourcedefinition", "x"]),
            ),
            ("runtimeclass write", call(&["create", "runtimeclass", "x"])),
            (
                "scc write",
                call(&["create", "securitycontextconstraints", "x"]),
            ),
            (
                "runtimeClassName",
                call(&[
                    "run",
                    "p",
                    "--overrides",
                    "{\"spec\":{\"runtimeClassName\":\"x\"}}",
                ]),
            ),
            (
                "--namespace=other",
                call(&[
                    "auth",
                    "can-i",
                    "get",
                    "services",
                    &format!("--namespace={other}"),
                ]),
            ),
            (
                "--namespace=<run> form",
                call(&[
                    "auth",
                    "can-i",
                    "get",
                    "services",
                    &format!("--namespace={name}"),
                ]),
            ),
            (
                "repeated -n",
                call(&[
                    "auth", "can-i", "get", "services", "-n", name, "-n", "default",
                ]),
            ),
            (
                "repeated -n, other first",
                call(&[
                    "auth", "can-i", "get", "services", "-n", "default", "-n", name,
                ]),
            ),
            ("raw other path", call(&["get", "--raw", "/api/v1/nodes"])),
            (
                "raw other path with -n",
                call(&["get", "--raw", "/api/v1/nodes", "-n", name]),
            ),
            (
                "raw /version with extra",
                call(&["get", "--raw", "/version", "-n", "default"]),
            ),
            (
                "delete other namespace",
                call(&["delete", "namespace", other, "--wait"]),
            ),
            (
                "delete default",
                call(&["delete", "namespace", "default", "--wait"]),
            ),
            ("delete without wait", call(&["delete", "namespace", name])),
            (
                "delete all namespaces",
                call(&["delete", "namespace", "--all", "--wait"]),
            ),
            (
                "create other namespace",
                call(&["create", "namespace", other]),
            ),
            ("apply -f", call(&["apply", "-f", "-"])),
            ("apply -f with -n", call(&["apply", "-f", "-", "-n", name])),
            (
                "unlisted verb",
                call(&["auth", "can-i", "patch", "services", "-n", name]),
            ),
            (
                "unlisted resource",
                call(&["auth", "can-i", "get", "nodes", "-n", name]),
            ),
            (
                "unlisted resource secrets",
                call(&["auth", "can-i", "get", "secrets", "-n", name]),
            ),
            (
                "trailing flag",
                call(&["auth", "can-i", "get", "services", "-n", name, "--as=x"]),
            ),
            ("empty", call(&[])),
        ];
        for (label, bad) in fixtures {
            assert!(!was01_plan::call_allowed(&namespace, &bad), "{label}");
            let mut plan = was01_plan::build_plan(&namespace);
            assert!(was01_plan::plan_namespaced_only(&namespace, &plan).is_ok());
            plan.insert(3, bad);
            assert!(
                was01_plan::plan_namespaced_only(&namespace, &plan).is_err(),
                "{label}"
            );
        }
        // A namespace outside the harness prefix is refused whatever the calls say.
        let foreign = Namespace::new("fs-test-other-x").unwrap();
        assert!(
            was01_plan::plan_namespaced_only(&foreign, &was01_plan::build_plan(&foreign)).is_err()
        );
    }

    // ---- create, cleanup ------------------------------------------------------------------

    #[test]
    fn a_healthy_run_creates_probes_and_deletes_in_order() {
        let namespace = ns("order");
        let fake = Fake::healthy();
        let report = was01_probe::run(&fake, &namespace, RUN_BUDGET).unwrap();
        let n = namespace.name();
        let mut expected = vec![
            call(&["create", "namespace", n]),
            call(&["get", "--raw", "/version"]),
        ];
        for resource in ["deployments.apps", "services", "leases.coordination.k8s.io"] {
            for verb in ["get", "create", "update", "delete"] {
                expected.push(call(&["auth", "can-i", verb, resource, "-n", n]));
            }
        }
        expected.push(call(&["delete", "namespace", n, "--wait"]));
        assert_eq!(fake.calls(), expected);
        deleted_last(&fake, &namespace);
        let plan = report.item(PLAN_ITEM).unwrap();
        assert_eq!(
            (plan.verdict, plan.evidence.as_deref()),
            (Verdict::Passed, Some("calls=15"))
        );
    }

    #[test]
    fn a_failed_create_including_already_exists_deletes_nothing_and_errors() {
        for answer in [
            reply("", 1),
            reply(
                "Error from server (AlreadyExists): namespaces \"x\" already exists",
                1,
            ),
            Err("Bearer eyJhbGciOi.token-like-secret".into()),
        ] {
            let namespace = ns("exists");
            let fake = Fake::new(move |_| answer.clone());
            let error = was01_probe::run(&fake, &namespace, RUN_BUDGET).unwrap_err();
            assert_eq!(fake.calls(), vec![was01_plan::create_call(&namespace)]);
            assert!(error.contains("FS_TEST_NAMESPACE_MODE"), "{error}");
            assert!(
                !error.contains("eyJ") && !error.contains("AlreadyExists"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_failure_after_create_deletes_once_as_the_last_call() {
        let namespace = ns("late");
        // Failure: the time budget runs out after the create.
        let fake = Fake::healthy();
        let error = was01_probe::run(&fake, &namespace, Duration::ZERO).unwrap_err();
        assert!(error.contains("budget"), "{error}");
        assert_eq!(fake.calls().len(), 2);
        deleted_last(&fake, &namespace);
        // Panic in a probe.
        let mut fake = Fake::healthy();
        fake.panic_on = Some("get");
        let namespace = ns("panic");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = was01_probe::run(&fake, &namespace, RUN_BUDGET);
        }));
        assert!(outcome.is_err());
        let calls = fake.calls();
        assert_eq!(calls[0], was01_plan::create_call(&namespace));
        deleted_last(&fake, &namespace);
    }

    #[test]
    fn a_failed_delete_makes_the_run_fail() {
        for failure in [reply("", 1), Err("token-like-stderr".into())] {
            let namespace = ns("delete");
            let fake = Fake::new(move |args| match args {
                ["delete", ..] => failure.clone(),
                ["get", ..] => reply(&version_body("v1.37.2"), 0),
                ["auth", ..] => reply("yes\n", 0),
                _ => reply("", 0),
            });
            let error = was01_probe::run(&fake, &namespace, RUN_BUDGET).unwrap_err();
            assert!(error.contains(namespace.name()), "{error}");
            assert!(!error.contains("token-like"), "{error}");
            deleted_last(&fake, &namespace);
        }
    }

    // ---- the gate -----------------------------------------------------------------------

    fn counting_fake() -> Fake {
        Fake::healthy()
    }

    #[test]
    fn offline_makes_no_runner_call_and_writes_an_all_unverified_report() {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("was01-offline");
        for flag in [
            None,
            Some("0"),
            Some("true"),
            Some("yes"),
            Some(" 1"),
            Some(""),
        ] {
            // Not even a bad KUBECONFIG or shared mode matters while the gate is off.
            let gate = was01_probe::gate(flag, Some(":"), Some("shared"), None).unwrap();
            let made = Cell::new(0);
            let path = was01_probe::execute(gate, &dir, |_| {
                made.set(made.get() + 1);
                counting_fake()
            })
            .unwrap();
            assert_eq!(made.get(), 0, "the runner is never built offline");
            assert!(path.starts_with(env!("CARGO_TARGET_TMPDIR")));
            let report = Report::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(report, Report::new());
            for id in ITEM_IDS {
                let item = report.item(id).unwrap();
                assert_eq!(item.verdict, Verdict::Unverified, "{id}");
            }
            for verb in VERBS {
                for resource in RESOURCES {
                    let item = report
                        .item(model::can_i_item(verb, resource).unwrap())
                        .unwrap();
                    assert_eq!(item.reason, Some(Reason::NotProbed));
                }
            }
            assert_eq!(
                report.item(VERSION_ITEM).unwrap().reason,
                Some(Reason::NotProbed)
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_gate_on_refuses_shared_mode_and_an_unusable_kubeconfig_before_any_call() {
        let dir = scratch("evidence-gate");
        let file = dir.join("config");
        std::fs::write(&file, kubeconfig_text("https://127.0.0.1:1", "u")).unwrap();
        let file = file.to_str().unwrap();
        let shared = ("shared", Some("fs-test-shared"));
        for (kubeconfig, mode) in [
            (Some(file), Some(shared)),
            (Some(file), Some(("shared", None))),
            (Some(file), Some(("bogus", None))),
            (None, None),
            (Some(""), None),
            (Some(":"), None),
            (Some("/nonexistent/kubeconfig"), None),
        ] {
            let result = was01_probe::gate(
                Some("1"),
                kubeconfig,
                mode.map(|m| m.0),
                mode.and_then(|m| m.1),
            );
            assert!(result.is_err());
        }
        assert!(matches!(
            was01_probe::gate(Some("1"), Some(file), None, None).unwrap(),
            Gate::Cluster { .. }
        ));
        assert!(matches!(
            was01_probe::gate(Some("1"), Some(file), Some("create"), None).unwrap(),
            Gate::Cluster { .. }
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_gate_on_runs_through_the_runner_and_writes_a_report() {
        let dir = scratch("evidence-run");
        let file = dir.join("config");
        std::fs::write(&file, kubeconfig_text("https://127.0.0.1:1", "u")).unwrap();
        let gate = was01_probe::gate(Some("1"), Some(file.to_str().unwrap()), None, None).unwrap();
        let out = dir.join("out");
        let fake = std::rc::Rc::new(Fake::healthy());
        struct Shared(std::rc::Rc<Fake>);
        impl Kubectl for Shared {
            fn call(&self, args: &[&str]) -> Result<Reply, String> {
                self.0.call(args)
            }
        }
        let handle = fake.clone();
        let path = was01_probe::execute(gate, &out, move |_| Shared(handle)).unwrap();
        let report = Report::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(report.cell(Target::K8s137).verdict, Verdict::Passed);
        let calls = fake.calls();
        assert_eq!(calls.len(), 15);
        assert!(calls[0][2].starts_with("fs-test-was01-"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn report_file_names_are_unique() {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("was01-names");
        let paths: Vec<_> = (0..5)
            .map(|_| was01_probe::write_report(&dir, &Report::new()).unwrap())
            .collect();
        let mut sorted = paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    // ---- fixed items and the summary ------------------------------------------------------

    #[test]
    fn openshift_pins_and_guest_items_are_fixed_and_the_summary_is_never_go() {
        let fixed = [
            ("openshift-4.20", Reason::OpenshiftNotAvailable),
            ("pin-engine", Reason::NoPinSelected),
            ("pin-architecture", Reason::NoPinSelected),
            ("pin-wasi", Reason::NoPinSelected),
            ("pin-compiler", Reason::NoPinSelected),
            ("guest-install", Reason::NotCollected),
            ("guest-execute", Reason::NotCollected),
            ("guest-interrupt", Reason::NotCollected),
        ];
        let healthy = Fake::healthy();
        let mut reports = vec![
            Report::new(),
            was01_probe::run(&healthy, &ns("fixed"), RUN_BUDGET).unwrap(),
        ];
        let mut tried = Report::new();
        for (id, _) in fixed {
            assert_eq!(tried.set_passed(id, "yes"), Err(ModelError::FixedItem));
            assert_eq!(
                tried.set_failed(id, Reason::CheckFailed),
                Err(ModelError::FixedItem)
            );
            assert_eq!(
                tried.set_unverified(id, Reason::NotProbed),
                Err(ModelError::FixedItem)
            );
        }
        reports.push(tried);
        for report in reports {
            for (id, reason) in fixed {
                let item = report.item(id).unwrap();
                assert_eq!(
                    (item.verdict, item.reason),
                    (Verdict::Unverified, Some(reason)),
                    "{id}"
                );
                assert!(report.non_passed().contains(&id));
            }
            let text = report.to_text();
            let summary = text.lines().last().unwrap();
            assert!(
                summary.starts_with("summary not-go non-passed="),
                "{summary}"
            );
            assert!(!text.contains("summary go"));
            for id in report.non_passed() {
                assert!(summary.contains(id), "{id}");
            }
            let listed = summary.strip_prefix("summary not-go non-passed=").unwrap();
            assert_eq!(listed.split(',').count(), report.non_passed().len());
        }
        // A fully healthy 1.37 still lists the eight items it cannot know.
        let report = was01_probe::run(&Fake::healthy(), &ns("fixed2"), RUN_BUDGET).unwrap();
        assert_eq!(report.non_passed().len(), 8);
    }

    // ---- the report text ------------------------------------------------------------------

    #[test]
    fn a_report_round_trips_and_malformed_input_is_rejected() {
        let healthy = Fake::healthy();
        let mut reports = vec![
            Report::new(),
            was01_probe::run(&healthy, &ns("round"), RUN_BUDGET).unwrap(),
        ];
        let mut mixed = Report::new();
        mixed.set_passed(VERSION_ITEM, "v1.36.4+k3s1").unwrap();
        mixed.set_passed("can-i-get-services", "yes").unwrap();
        mixed
            .set_failed("can-i-create-services", Reason::CheckFailed)
            .unwrap();
        mixed
            .set_unverified("can-i-update-services", Reason::Denied)
            .unwrap();
        mixed.set_failed(PLAN_ITEM, Reason::PlanViolation).unwrap();
        reports.push(mixed);
        for report in reports {
            let text = report.to_text();
            assert_eq!(Report::parse(&text), Ok(report.clone()));
            assert_eq!(Report::parse(&text).unwrap().to_text(), text);
        }
        let good = Report::new().to_text();
        let first_item = "item kubernetes-version unverified not-probed -";
        let bad: Vec<(&str, String)> = vec![
            ("empty", String::new()),
            ("no trailing newline", good.trim_end().to_owned()),
            ("crlf", good.replace('\n', "\r\n")),
            (
                "wrong header",
                good.replacen("was01-cluster-evidence 1", "was01-cluster-evidence 2", 1),
            ),
            (
                "missing item",
                good.replacen(&format!("{first_item}\n"), "", 1),
            ),
            (
                "duplicate item",
                good.replacen(first_item, &format!("{first_item}\n{first_item}"), 1),
            ),
            ("extra line", format!("{good}extra line\n")),
            ("blank line", format!("{good}\n")),
            (
                "extra field",
                good.replacen(first_item, &format!("{first_item} x"), 1),
            ),
            (
                "missing field",
                good.replacen(
                    first_item,
                    "item kubernetes-version unverified not-probed",
                    1,
                ),
            ),
            (
                "double space",
                good.replacen(
                    first_item,
                    "item  kubernetes-version unverified not-probed -",
                    1,
                ),
            ),
            (
                "unknown verdict",
                good.replacen("unverified not-probed", "go not-probed", 1),
            ),
            ("unknown reason", good.replacen("not-probed", "because", 1)),
            (
                "passed without evidence",
                good.replacen(first_item, "item kubernetes-version passed - -", 1),
            ),
            (
                "passed with a reason",
                good.replacen(
                    first_item,
                    "item kubernetes-version passed not-probed v1.37.2",
                    1,
                ),
            ),
            (
                "passed with an unsupported version",
                good.replacen(first_item, "item kubernetes-version passed - v1.35.0", 1),
            ),
            (
                "unverified with evidence",
                good.replacen(
                    first_item,
                    "item kubernetes-version unverified not-probed v1.37.2",
                    1,
                ),
            ),
            (
                "passed fixed item",
                good.replacen(
                    "item pin-wasi unverified no-pin-selected -",
                    "item pin-wasi passed - yes",
                    1,
                ),
            ),
            (
                "fixed item with another reason",
                good.replacen(
                    "item pin-wasi unverified no-pin-selected -",
                    "item pin-wasi unverified not-probed -",
                    1,
                ),
            ),
            (
                "reason not allowed for the item",
                good.replacen(first_item, "item kubernetes-version unverified denied -", 1),
            ),
            (
                "items out of order",
                good.replacen("item pin-wasi", "item pin-engine", 1),
            ),
            (
                "tag",
                good.replacen(
                    first_item,
                    "items kubernetes-version unverified not-probed -",
                    1,
                ),
            ),
            ("go summary", good.replace("summary not-go", "summary go")),
            ("short summary", good.replace(",guest-interrupt", "")),
            (
                "forged cell",
                good.replace(
                    "cell k8s-1.37 unverified no-matching-server",
                    "cell k8s-1.37 passed -",
                ),
            ),
        ];
        for (label, text) in bad {
            assert!(Report::parse(&text).is_err(), "{label}");
        }
        let unknown = good.replacen(
            first_item,
            "item kubernetes-versioon unverified not-probed -",
            1,
        );
        assert_eq!(Report::parse(&unknown), Err(ModelError::UnknownItem));
        let hostile = good.replacen(first_item, "item rm-rf-$(id) unverified not-probed -", 1);
        let error = Report::parse(&hostile).unwrap_err();
        assert!(!error.to_string().contains("rm-rf"));
    }

    #[test]
    fn a_failed_item_is_not_forgeable_into_a_passed_cell_by_editing_the_text() {
        let mut report = Report::new();
        report.set_passed(VERSION_ITEM, "v1.37.2").unwrap();
        for verb in VERBS {
            for resource in RESOURCES {
                report
                    .set_passed(model::can_i_item(verb, resource).unwrap(), "yes")
                    .unwrap();
            }
        }
        let text = report.to_text();
        assert!(text.contains("cell k8s-1.37 passed -"));
        let edited = text.replace(
            "item can-i-get-services passed - yes",
            "item can-i-get-services unverified denied -",
        );
        assert_eq!(Report::parse(&edited), Err(ModelError::Inconsistent));
    }

    #[test]
    fn runner_output_never_reaches_the_report_or_an_error() {
        let secret = "Bearer eyJhbGciOiJSUzI1NiJ9.SECRET-TOKEN";
        let hostile = format!("v1.37.2\n{secret}");
        let fake = Fake::new(move |args| match args {
            ["get", ..] => reply(&version_body(&hostile), 0),
            ["auth", "can-i", "get", ..] => Err(secret.to_owned()),
            ["auth", ..] => reply(&format!("no\n{secret}"), 1),
            _ => reply("", 0),
        });
        let report = was01_probe::run(&fake, &ns("leak"), RUN_BUDGET).unwrap();
        let text = report.to_text();
        assert!(!text.contains("SECRET") && !text.contains("eyJ") && !text.contains("Bearer"));
        assert_eq!(
            report.item(VERSION_ITEM).unwrap().reason,
            Some(Reason::MalformedVersion)
        );
        // The stdout of a failed create or delete is not echoed either.
        let fake = Fake::new(move |_| reply(secret, 1));
        let error = was01_probe::run(&fake, &ns("leak2"), RUN_BUDGET).unwrap_err();
        assert!(!error.contains("SECRET"), "{error}");
        assert!(was01_probe::parse_version(&version_body(&format!("v1.37.2\n{secret}"))).is_err());
    }

    // ---- the real runner ------------------------------------------------------------------

    fn process(script: &str, limit: Duration, dir: &Path) -> ProcessKubectl {
        let config = dir.join("config");
        std::fs::write(&config, kubeconfig_text("https://127.0.0.1:1", "u")).unwrap();
        let kubeconfigs = Kubeconfigs::parse(config.to_str()).unwrap();
        let script_file = dir.join("fake-kubectl.sh");
        std::fs::write(&script_file, script).unwrap();
        ProcessKubectl::with_command(
            &["/bin/sh", script_file.to_str().unwrap()],
            kubeconfigs,
            limit,
            limit,
        )
    }

    #[test]
    fn the_process_runner_returns_stdout_and_status_and_drops_stderr() {
        let dir = scratch("evidence-process");
        let kubectl = process(
            "echo \"$@\" >&2; echo token-in-stderr >&2; echo no; exit 1\n",
            Duration::from_secs(20),
            &dir,
        );
        let answer = kubectl.call(&["auth", "can-i", "get", "services"]).unwrap();
        assert_eq!(
            answer,
            Reply {
                stdout: "no\n".into(),
                code: 1
            }
        );
        let kubectl = process(
            "for a in \"$@\"; do echo \"$a\"; done\n",
            Duration::from_secs(20),
            &dir,
        );
        let answer = kubectl.call(&["get", "--raw", "/version"]).unwrap();
        let lines: Vec<&str> = answer.stdout.lines().collect();
        assert!(lines[0].starts_with("--kubeconfig="), "{lines:?}");
        assert!(!lines[0].contains("fs-test-was01"));
        assert_eq!(lines[1], "--request-timeout=15s");
        assert_eq!(&lines[2..], ["get", "--raw", "/version"]);
        let answer = kubectl
            .call(&["delete", "namespace", "x", "--wait"])
            .unwrap();
        assert!(answer.stdout.contains("--request-timeout=60s"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_process_runner_kills_a_hung_kubectl_at_the_deadline() {
        let dir = scratch("evidence-hang");
        let kubectl = process("sleep 30\n", Duration::from_millis(300), &dir);
        let started = std::time::Instant::now();
        let error = kubectl.call(&["get", "--raw", "/version"]).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!error.contains("sleep"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    // ---- README -------------------------------------------------------------------------

    #[test]
    fn the_readme_names_every_item_reason_target_and_env_var() {
        for id in ITEM_IDS {
            assert!(README.contains(id), "{id}");
        }
        for reason in Reason::ALL {
            assert!(README.contains(reason.code()), "{}", reason.code());
        }
        for target in Target::ALL {
            assert!(README.contains(target.code()), "{}", target.code());
        }
        for var in ENV_VARS {
            assert!(README.contains(var), "{var}");
        }
        assert!(README.contains("49.0.5"));
        assert!(README.contains("not a selected pin"));
    }

    // ---- model and dependency guards --------------------------------------------------------

    #[test]
    fn the_model_imports_only_std() {
        let source = include_str!("../src/qualification/cluster_evidence.rs");
        let mut uses = 0;
        for line in source.lines().map(str::trim_start) {
            if let Some(path) = line.strip_prefix("use ") {
                uses += 1;
                assert!(path.starts_with("std::"), "{line}");
            }
            assert!(!line.starts_with("extern crate"), "{line}");
        }
        assert!(uses > 0);
        for banned in [
            "serde",
            "kube::",
            "tokio",
            "wasmtime",
            "crate::",
            "super::",
            "Status::Passed",
        ] {
            assert!(!source.contains(banned), "{banned}");
        }
    }

    const MANIFEST: &str = include_str!("../Cargo.toml");
    const LOCK: &str = include_str!("../../Cargo.lock");

    #[test]
    fn the_dependency_guard_passes_on_the_real_manifests() {
        assert_eq!(
            was01_deps::check(was01_deps::BASELINE, MANIFEST, LOCK),
            Ok(())
        );
    }

    #[test]
    fn the_dependency_guard_fails_on_an_added_entry_or_package() {
        let check =
            |manifest: &str, lock: &str| was01_deps::check(was01_deps::BASELINE, manifest, lock);
        for fixture in [
            MANIFEST.replacen("[dependencies]\n", "[dependencies]\nextra = \"1\"\n", 1),
            MANIFEST.replacen(
                "[dev-dependencies]\n",
                "[dev-dependencies]\nextra = \"1\"\n",
                1,
            ),
            format!("{MANIFEST}\n[build-dependencies]\nextra = \"1\"\n"),
            format!("{MANIFEST}\n[target.'cfg(unix)'.dependencies]\nextra = \"1\"\n"),
        ] {
            assert_ne!(fixture, MANIFEST);
            let error = check(&fixture, LOCK).unwrap_err();
            assert!(error.contains("extra"), "{error}");
            assert!(error.contains("was01_dependency_baseline.txt"), "{error}");
            assert!(error.contains("--ignored"), "{error}");
        }
        let lock = format!("{LOCK}\n[[package]]\nname = \"extra\"\nversion = \"1.0.0\"\n");
        let error = check(MANIFEST, &lock).unwrap_err();
        assert!(error.contains("package extra 1.0.0"), "{error}");
        // A package that vanished, or a changed version, differs from the baseline as well.
        let lock = LOCK.replacen(
            "name = \"sha2\"\nversion = \"0.10.9\"",
            "name = \"sha2\"\nversion = \"0.10.8\"",
            1,
        );
        assert_ne!(lock, LOCK);
        assert!(check(MANIFEST, &lock).is_err());
        assert!(check("not = [toml", LOCK).is_err());
        assert!(check(MANIFEST, "").is_err());
    }

    /// Prints the baseline for the current manifests; see the failure message of the guard.
    #[test]
    #[ignore = "prints the dependency baseline; run on purpose"]
    fn print_dependency_baseline() {
        print!("{}", was01_deps::render(MANIFEST, LOCK).unwrap());
    }

    /// The real offline-by-default evidence run. Operator-only with FS_WAS01_CLUSTER=1 and
    /// KUBECONFIG: it creates fs-test-was01-<random>, probes, deletes it and writes the report
    /// under the target directory. The report is never committed.
    #[test]
    fn was01_cluster_evidence() -> Result<(), String> {
        let env = |key: &str| std::env::var(key).ok();
        let gate = was01_probe::gate(
            env("FS_WAS01_CLUSTER").as_deref(),
            env("KUBECONFIG").as_deref(),
            env("FS_TEST_NAMESPACE_MODE").as_deref(),
            env("FS_TEST_NAMESPACE").as_deref(),
        )?;
        let online = matches!(gate, Gate::Cluster { .. });
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("was01-evidence");
        let path = was01_probe::execute(gate, &dir, ProcessKubectl::new)?;
        if online {
            println!("WAS-01 evidence report: {}", path.display());
            println!(
                "{}",
                std::fs::read_to_string(&path).map_err(|_| "cannot read the report")?
            );
        } else {
            println!("UNVERIFIED: FS_WAS01_CLUSTER=1 is not set; no cluster was contacted");
        }
        Ok(())
    }
}
