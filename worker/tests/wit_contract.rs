//! Static shape checks only; RUN-01/RUN-02 must enforce linking and limits at runtime.
use std::{collections::BTreeSet, fs, path::Path};
use wit_parser::{PackageId, Resolve, Type, TypeDefKind, TypeId, WorldItem};

const INTERFACES: &[&str] = &[
    "storage",
    "http",
    "events",
    "workflows",
    "logging",
    "cancellation",
    "progress",
    "invocation-context",
];
const FALLIBLE: &[&str] = &["storage", "http", "events", "workflows", "progress"];
const LIMITS: &[(&str, u64)] = &[
    ("max-guest-to-host-bytes", 1_048_576),
    ("max-host-to-guest-bytes", 1_048_576),
    ("host-call-timeout-ms", 30_000),
];
const ERRORS: &[&str] = &["denied", "limit-exceeded", "timeout", "cancelled"];

fn key_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut values = text.lines().filter_map(|line| {
        let (name, value) = line.trim().split_once(':')?;
        (name == key).then_some(value.trim())
    });
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn unalias(resolve: &Resolve, mut ty: Type) -> Option<TypeId> {
    loop {
        let Type::Id(id) = ty else { return None };
        match resolve.types[id].kind {
            TypeDefKind::Type(next) => ty = next,
            _ => return Some(id),
        }
    }
}

fn check(resolve: &Resolve, package: PackageId) -> Vec<String> {
    let mut violations = Vec::new();
    let package_data = &resolve.packages[package];
    if package_data.name.to_string() != "falcone:host@0.1.0" {
        violations.push("package must be falcone:host@0.1.0".into());
    }
    let Some(world) = package_data.worlds.get("function") else {
        return vec!["missing world function".into()];
    };
    let world = &resolve.worlds[*world];
    let mut imports = BTreeSet::new();
    for item in world.imports.values() {
        let WorldItem::Interface { id, .. } = item else {
            violations.push("non-interface import".into());
            continue;
        };
        let interface = &resolve.interfaces[*id];
        let name = interface.name.as_deref().unwrap_or("<inline>");
        if name.contains("secret") {
            violations.push(format!("secret interface: {name}"));
        }
        if interface.package != Some(package) {
            violations.push(format!("non-falcone:host import: {name}"));
        }
        if !INTERFACES.contains(&name) || !imports.insert(name) {
            violations.push(format!("unexpected interface: {name}"));
        }
    }
    for name in INTERFACES {
        if !imports.contains(name) {
            violations.push(format!("missing interface: {name}"));
        }
    }

    // Check every interface in the package, including any not linked by this world.
    let mut shared_error = None;
    for id in package_data.interfaces.values() {
        let interface = &resolve.interfaces[*id];
        if let Some(error) = interface.types.get("host-error") {
            let error = unalias(resolve, Type::Id(*error)).expect("named error type");
            if let Some(shared) = shared_error {
                if error != shared {
                    violations.push("host-error must be shared".into());
                }
            } else {
                shared_error = Some(error);
            }
        }
    }
    if let Some(error) = shared_error {
        if let TypeDefKind::Variant(variant) = &resolve.types[error].kind {
            for case in ERRORS {
                if !variant.cases.iter().any(|actual| actual.name == *case) {
                    violations.push(format!("host-error missing {case}"));
                }
            }
        } else {
            violations.push("host-error must be a variant".into());
        }
    } else {
        violations.push("missing shared host-error".into());
    }
    for (name, id) in &package_data.interfaces {
        let interface = &resolve.interfaces[*id];
        let docs = interface.docs.contents.as_deref().unwrap_or_default();
        for (key, expected) in LIMITS {
            match key_value(docs, key).and_then(|value| value.parse::<u64>().ok()) {
                Some(value) if value > 0 && value == *expected => {}
                _ => violations.push(format!("{name}: invalid {key}, expected {expected}")),
            }
        }
        if key_value(docs, "cancellation").is_none_or(str::is_empty) {
            violations.push(format!("{name}: missing cancellation"));
        }
        for ty in interface.types.values() {
            let ty = &resolve.types[*ty];
            if matches!(ty.kind, TypeDefKind::Resource)
                && !ty
                    .docs
                    .contents
                    .as_deref()
                    .unwrap_or_default()
                    .contains("execution-scoped")
            {
                violations.push(format!("{name}: resource must be execution-scoped"));
            }
        }
        for function in interface.functions.values() {
            for param in &function.params {
                if param.name.contains("tenant") || param.name.contains("workspace") {
                    violations.push(format!(
                        "{name}.{}: forbidden scope parameter {}",
                        function.name, param.name
                    ));
                }
            }
            if name == "invocation-context" && !function.params.is_empty() {
                violations.push("invocation-context functions must take no parameters".into());
            }
            if FALLIBLE.contains(&name.as_str()) {
                let result = function.result.and_then(|ty| unalias(resolve, ty));
                let error = result.and_then(|id| match &resolve.types[id].kind {
                    TypeDefKind::Result(result) => result.err.and_then(|ty| unalias(resolve, ty)),
                    _ => None,
                });
                if error.is_none() || error != shared_error {
                    violations.push(format!(
                        "{name}.{}: must return result with shared host-error",
                        function.name
                    ));
                }
            }
        }
    }
    violations
}

#[test]
fn canonical_contract_resolves_and_has_no_violations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
    let mut resolve = Resolve::default();
    let (package, _) = resolve.push_dir(&root).expect("resolve worker/wit");
    let violations = check(&resolve, package);
    assert!(violations.is_empty(), "{violations:#?}");
}

fn check_readme(resolve: &Resolve, package: PackageId, readme: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let expected_package = resolve.packages[package].name.to_string();
    if key_value(readme, "package") != Some(expected_package.as_str()) {
        violations.push("README package differs from WIT".into());
    }
    for id in resolve.packages[package].interfaces.values() {
        let docs = resolve.interfaces[*id]
            .docs
            .contents
            .as_deref()
            .unwrap_or_default();
        for (key, _) in LIMITS {
            if key_value(readme, key).is_none() || key_value(readme, key) != key_value(docs, key) {
                violations.push(format!("README {key} differs from WIT"));
            }
        }
    }
    violations
}

#[test]
fn readme_agrees_with_resolved_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
    let mut resolve = Resolve::default();
    let (package, _) = resolve.push_dir(&root).expect("resolve worker/wit");
    let readme = fs::read_to_string(root.join("README.md")).expect("read WIT README");
    let violations = check_readme(&resolve, package, &readme);
    assert!(violations.is_empty(), "{violations:#?}");
}

// Inline fixtures are independent of the canonical file: contract edits cannot
// silently repair a negative fixture. Each fixture must resolve before checking.
const FIXTURE: &str = r#"
package falcone:host@0.1.0;
interface cancellation {
    variant host-error { denied, limit-exceeded, timeout, cancelled }
    is-cancelled: func() -> bool;
}
interface storage {
    use cancellation.{host-error};
    /// An execution-scoped handle, invalid after completion.
    resource object;
    read: func(key: string) -> result<list<u8>, host-error>;
}
interface http {
    use cancellation.{host-error};
    request: func(url: string) -> result<list<u8>, host-error>;
}
interface events {
    use cancellation.{host-error};
    publish: func(payload: list<u8>) -> result<_, host-error>;
}
interface workflows {
    use cancellation.{host-error};
    start: func(name: string) -> result<string, host-error>;
}
interface logging { log: func(message: string); }
interface progress {
    use cancellation.{host-error};
    report: func(completed: u64) -> result<_, host-error>;
}
interface invocation-context { invocation-key: func() -> string; }
world function {
    import storage;
    import http;
    import events;
    import workflows;
    import logging;
    import cancellation;
    import progress;
    import invocation-context;
}
"#;
const FIXTURE_DOCS: &str = "\n/// max-guest-to-host-bytes: 1048576\n/// max-host-to-guest-bytes: 1048576\n/// host-call-timeout-ms: 30000\n/// cancellation: Blocking calls are cancelled independently of fuel/epoch.\n";

fn fixture() -> String {
    FIXTURE.replace("interface ", &format!("{FIXTURE_DOCS}interface "))
}

fn resolve_fixture(source: &str) -> (Resolve, PackageId) {
    let mut resolve = Resolve::default();
    let package = resolve
        .push_str("fixture.wit", source)
        .expect("fixture must parse and resolve, not fail at syntax");
    (resolve, package)
}

fn rejects(source: &str, expected: &str) {
    let (resolve, package) = resolve_fixture(source);
    let violations = check(&resolve, package);
    assert!(
        violations
            .iter()
            .any(|violation| violation.contains(expected)),
        "expected {expected:?}, got {violations:#?}"
    );
}

#[test]
fn inline_fixture_is_a_valid_control() {
    let (resolve, package) = resolve_fixture(&fixture());
    assert_eq!(check(&resolve, package), Vec::<String>::new());
}

#[test]
fn package_version_is_required() {
    rejects(
        &fixture().replace("@0.1.0", ""),
        "package must be falcone:host@0.1.0",
    );
    rejects(
        &fixture().replace("@0.1.0", "@0.2.0"),
        "package must be falcone:host@0.1.0",
    );
}

#[test]
fn world_requires_exactly_the_eight_interfaces() {
    rejects(
        &fixture().replace("import events;", ""),
        "missing interface: events",
    );
    rejects(
        &fixture().replace(
            "world function {",
            "interface extra {}\nworld function { import extra;",
        ),
        "unexpected interface: extra",
    );
    rejects(
        &fixture().replace(
            "world function {",
            "interface secret-store {}\nworld function { import secret-store;",
        ),
        "secret interface: secret-store",
    );
    rejects(
        &fixture().replace(
            "world function {",
            "world function { import ambient-call: func();",
        ),
        "non-interface import",
    );
    rejects(
        &fixture().replace("world function", "world other"),
        "missing world function",
    );
}

#[test]
fn foreign_authority_is_rejected_after_successful_resolution() {
    for (dependency, import) in [
        (
            "package wasi:filesystem@0.2.0; interface types {}",
            "wasi:filesystem/types@0.2.0",
        ),
        (
            "package wasi:sockets@0.2.0; interface network {}",
            "wasi:sockets/network@0.2.0",
        ),
        (
            "package wasi:http@0.2.0; interface outgoing-handler {}",
            "wasi:http/outgoing-handler@0.2.0",
        ),
        (
            "package other:host@0.1.0; interface storage {}",
            "other:host/storage@0.1.0",
        ),
        (
            "package falcone:host@0.2.0; interface storage {}",
            "falcone:host/storage@0.2.0",
        ),
    ] {
        let mut resolve = Resolve::default();
        resolve
            .push_str("dependency.wit", dependency)
            .expect("resolve foreign fixture dependency");
        let source = fixture().replace(
            "world function {",
            &format!("world function {{ import {import};"),
        );
        let package = resolve
            .push_str("fixture.wit", &source)
            .expect("resolve fixture with foreign import");
        let violations = check(&resolve, package);
        assert!(
            violations
                .iter()
                .any(|violation| violation.starts_with("non-falcone:host import:")),
            "{import}: {violations:#?}"
        );
    }
}

#[test]
fn each_interface_requires_positive_exact_limits_and_cancellation() {
    for name in INTERFACES {
        let original = format!("{FIXTURE_DOCS}interface {name} ");
        assert!(fixture().contains(&original), "fixture contains {name}");
        for (before, after, rule) in [
            (
                "/// max-host-to-guest-bytes: 1048576\n",
                "",
                "max-host-to-guest-bytes",
            ),
            (
                "host-call-timeout-ms: 30000",
                "host-call-timeout-ms: 0",
                "host-call-timeout-ms",
            ),
            (
                "host-call-timeout-ms: 30000",
                "host-call-timeout-ms: nope",
                "host-call-timeout-ms",
            ),
            (
                "max-guest-to-host-bytes: 1048576",
                "max-guest-to-host-bytes: 1048575",
                "max-guest-to-host-bytes",
            ),
            (
                "/// cancellation: Blocking calls are cancelled independently of fuel/epoch.\n",
                "",
                "cancellation",
            ),
        ] {
            let changed = original.replace(before, after);
            rejects(
                &fixture().replace(&original, &changed),
                &format!(
                    "{name}: {} {rule}",
                    if rule == "cancellation" {
                        "missing"
                    } else {
                        "invalid"
                    }
                ),
            );
        }
    }
}

#[test]
fn fallible_operations_require_the_shared_error_result() {
    for (before, after, expected) in [
        (
            "read: func(key: string) -> result<list<u8>, host-error>",
            "read: func(key: string) -> list<u8>",
            "storage.read: must return result with shared host-error",
        ),
        (
            "request: func(url: string) -> result<list<u8>, host-error>",
            "request: func(url: string)",
            "http.request: must return result with shared host-error",
        ),
        (
            "publish: func(payload: list<u8>) -> result<_, host-error>",
            "publish: func(payload: list<u8>)",
            "events.publish: must return result with shared host-error",
        ),
        (
            "start: func(name: string) -> result<string, host-error>",
            "start: func(name: string) -> string",
            "workflows.start: must return result with shared host-error",
        ),
        (
            "report: func(completed: u64) -> result<_, host-error>",
            "report: func(completed: u64)",
            "progress.report: must return result with shared host-error",
        ),
        (
            "timeout, cancelled",
            "timeout",
            "host-error missing cancelled",
        ),
        (
            "denied, limit-exceeded",
            "limit-exceeded",
            "host-error missing denied",
        ),
        (
            "denied, limit-exceeded",
            "denied",
            "host-error missing limit-exceeded",
        ),
        (
            "timeout, cancelled",
            "cancelled",
            "host-error missing timeout",
        ),
        (
            "read: func(key: string) -> result<list<u8>, host-error>",
            "variant other-error { cancelled }\nread: func(key: string) -> result<list<u8>, other-error>",
            "storage.read: must return result with shared host-error",
        ),
    ] {
        let source = fixture().replace(before, after);
        rejects(&source, expected);
    }
}

#[test]
fn callers_cannot_choose_scope_or_parameterize_invocation_context() {
    for (before, after, expected) in [
        (
            "read: func(key: string)",
            "read: func(tenant: string)",
            "forbidden scope parameter tenant",
        ),
        (
            "read: func(key: string)",
            "read: func(workspace: string)",
            "forbidden scope parameter workspace",
        ),
        (
            "invocation-key: func()",
            "invocation-key: func(key: string)",
            "invocation-context functions must take no parameters",
        ),
    ] {
        rejects(&fixture().replace(before, after), expected);
    }
}

#[test]
fn resource_lifetime_is_part_of_the_contract() {
    rejects(
        &fixture().replace(
            "/// An execution-scoped handle, invalid after completion.",
            "",
        ),
        "storage: resource must be execution-scoped",
    );
}

#[test]
fn readme_only_version_and_limit_changes_are_rejected() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
    let mut resolve = Resolve::default();
    let (package, _) = resolve.push_dir(&root).expect("resolve canonical contract");
    let readme = fs::read_to_string(root.join("README.md")).expect("read WIT README");
    for (before, after, expected) in [
        (
            "falcone:host@0.1.0",
            "falcone:host@0.2.0",
            "README package differs from WIT",
        ),
        (
            "max-guest-to-host-bytes: 1048576",
            "max-guest-to-host-bytes: 2097152",
            "README max-guest-to-host-bytes differs from WIT",
        ),
        (
            "max-host-to-guest-bytes: 1048576",
            "max-host-to-guest-bytes: 2097152",
            "README max-host-to-guest-bytes differs from WIT",
        ),
        (
            "host-call-timeout-ms: 30000",
            "host-call-timeout-ms: 60000",
            "README host-call-timeout-ms differs from WIT",
        ),
    ] {
        let violations = check_readme(&resolve, package, &readme.replace(before, after));
        assert!(
            violations.iter().any(|violation| violation == expected),
            "{violations:#?}"
        );
    }
}
