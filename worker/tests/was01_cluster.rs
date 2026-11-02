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
use std::path::PathBuf;
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
