//! Static shape checks only; RUN-01/RUN-02 must enforce linking and limits at runtime.
use std::{collections::BTreeSet, fs, path::Path};
use wit_parser::{Handle, PackageId, Resolve, Type, TypeDefKind, TypeId, TypeOwner, WorldItem};

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
// Literal requirements for falcone:host@0.1.0, independent of the canonical WIT.
// Resource ownership is explicit even where WIT spells an own handle implicitly.
const RESOURCES: &[(&str, &str)] = &[
    ("storage", "object"),
    ("storage", "checkpoint"),
    ("workflows", "execution"),
];
const OPERATIONS: &[(&str, &str, &str)] = &[
    ("cancellation", "is-cancelled", "() -> bool"),
    (
        "storage",
        "open-object",
        "(key: string) -> result<own<falcone:host@0.1.0/storage.object>, host-error>",
    ),
    (
        "storage",
        "read-object",
        "(handle: borrow<falcone:host@0.1.0/storage.object>) -> result<list<u8>, host-error>",
    ),
    (
        "storage",
        "write-object",
        "(handle: borrow<falcone:host@0.1.0/storage.object>, data: list<u8>) -> result<_, host-error>",
    ),
    (
        "storage",
        "open-checkpoint",
        "(key: string) -> result<own<falcone:host@0.1.0/storage.checkpoint>, host-error>",
    ),
    (
        "storage",
        "read-checkpoint",
        "(handle: borrow<falcone:host@0.1.0/storage.checkpoint>) -> result<list<u8>, host-error>",
    ),
    (
        "storage",
        "write-checkpoint",
        "(handle: borrow<falcone:host@0.1.0/storage.checkpoint>, data: list<u8>) -> result<_, host-error>",
    ),
    (
        "http",
        "request",
        "(url: string, method: string, headers: list<header>, body: list<u8>) -> result<response, host-error>",
    ),
    (
        "events",
        "publish",
        "(destination: string, payload: list<u8>) -> result<_, host-error>",
    ),
    (
        "workflows",
        "start",
        "(workflow: string, input: list<u8>) -> result<own<falcone:host@0.1.0/workflows.execution>, host-error>",
    ),
    (
        "workflows",
        "signal",
        "(handle: borrow<falcone:host@0.1.0/workflows.execution>, name: string, payload: list<u8>) -> result<_, host-error>",
    ),
    (
        "logging",
        "log",
        "(severity: level, message: string, fields: list<field>) -> result<_, host-error>",
    ),
    (
        "progress",
        "report",
        "(completed: u64, total: u64, message: string) -> result<_, host-error>",
    ),
    (
        "progress",
        "report-checkpoint",
        "(handle: borrow<falcone:host@0.1.0/storage.checkpoint>) -> result<_, host-error>",
    ),
    ("invocation-context", "invocation-key", "() -> string"),
    ("invocation-context", "attempt-id", "() -> string"),
    ("invocation-context", "idempotency-key", "() -> string"),
];
const FALLIBLE: &[&str] = &[
    "storage",
    "http",
    "events",
    "workflows",
    "logging",
    "progress",
];
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

fn render_optional_type(resolve: &Resolve, ty: Option<Type>) -> String {
    ty.map(|ty| render_type(resolve, ty))
        .unwrap_or_else(|| "_".into())
}

fn render_type(resolve: &Resolve, ty: Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::U8 => "u8".into(),
        Type::U16 => "u16".into(),
        Type::U32 => "u32".into(),
        Type::U64 => "u64".into(),
        Type::S8 => "s8".into(),
        Type::S16 => "s16".into(),
        Type::S32 => "s32".into(),
        Type::S64 => "s64".into(),
        Type::F32 => "f32".into(),
        Type::F64 => "f64".into(),
        Type::Char => "char".into(),
        Type::String => "string".into(),
        Type::ErrorContext => "error-context".into(),
        Type::Id(id) => {
            let def = &resolve.types[id];
            match &def.kind {
                TypeDefKind::Type(ty) => render_type(resolve, *ty),
                TypeDefKind::List(ty) => format!("list<{}>", render_type(resolve, *ty)),
                TypeDefKind::Result(result) => format!(
                    "result<{}, {}>",
                    render_optional_type(resolve, result.ok),
                    render_optional_type(resolve, result.err),
                ),
                TypeDefKind::Handle(handle) => {
                    let (mode, resource) = match handle {
                        Handle::Borrow(id) => ("borrow", *id),
                        Handle::Own(id) => ("own", *id),
                    };
                    let resource = unalias(resolve, Type::Id(resource)).expect("resolved resource");
                    let resource = &resolve.types[resource];
                    let TypeOwner::Interface(owner) = resource.owner else {
                        return "<unowned-resource>".into();
                    };
                    let interface = &resolve.interfaces[owner];
                    let package = interface
                        .package
                        .map(|id| resolve.packages[id].name.to_string())
                        .unwrap_or_default();
                    format!(
                        "{mode}<{package}/{}.{}>",
                        interface.name.as_deref().unwrap_or("<inline>"),
                        resource.name.as_deref().unwrap_or("<unnamed>"),
                    )
                }
                // Named records, enums and variants are compared by their WIT name.
                _ => def
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("<{}>", def.kind.as_str())),
            }
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
        for (owner, resource) in RESOURCES {
            if owner == name
                && !interface
                    .types
                    .get(*resource)
                    .is_some_and(|id| matches!(resolve.types[*id].kind, TypeDefKind::Resource))
            {
                violations.push(format!("missing resource: {name}.{resource}"));
            }
        }
        for (owner, operation, expected) in OPERATIONS {
            if owner != name {
                continue;
            }
            let Some(function) = interface.functions.get(*operation) else {
                violations.push(format!("missing operation: {name}.{operation}"));
                continue;
            };
            let params = function
                .params
                .iter()
                .map(|param| format!("{}: {}", param.name, render_type(resolve, param.ty)))
                .collect::<Vec<_>>()
                .join(", ");
            let actual = format!(
                "({params}) -> {}",
                render_optional_type(resolve, function.result)
            );
            if actual != *expected {
                violations.push(format!(
                    "{name}.{operation}: wrong signature, expected {expected}, got {actual}"
                ));
            }
        }
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
    /// An execution-scoped handle, invalid after completion.
    resource checkpoint;
    open-object: func(key: string) -> result<object, host-error>;
    read-object: func(handle: borrow<object>) -> result<list<u8>, host-error>;
    write-object: func(handle: borrow<object>, data: list<u8>) -> result<_, host-error>;
    open-checkpoint: func(key: string) -> result<checkpoint, host-error>;
    read-checkpoint: func(handle: borrow<checkpoint>) -> result<list<u8>, host-error>;
    write-checkpoint: func(handle: borrow<checkpoint>, data: list<u8>) -> result<_, host-error>;
}
interface http {
    use cancellation.{host-error};
    record header { name: string, value: string }
    record response { status: u16, headers: list<header>, body: list<u8> }
    request: func(url: string, method: string, headers: list<header>, body: list<u8>) -> result<response, host-error>;
}
interface events {
    use cancellation.{host-error};
    publish: func(destination: string, payload: list<u8>) -> result<_, host-error>;
}
interface workflows {
    use cancellation.{host-error};
    /// An execution-scoped handle, invalid after completion.
    resource execution;
    start: func(workflow: string, input: list<u8>) -> result<execution, host-error>;
    signal: func(handle: borrow<execution>, name: string, payload: list<u8>) -> result<_, host-error>;
}
interface logging {
    use cancellation.{host-error};
    enum level { debug, info, warn, error }
    record field { name: string, value: string }
    log: func(severity: level, message: string, fields: list<field>) -> result<_, host-error>;
}
interface progress {
    use cancellation.{host-error};
    use storage.{checkpoint};
    report: func(completed: u64, total: u64, message: string) -> result<_, host-error>;
    report-checkpoint: func(handle: borrow<checkpoint>) -> result<_, host-error>;
}
interface invocation-context {
    invocation-key: func() -> string;
    attempt-id: func() -> string;
    idempotency-key: func() -> string;
}
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
            "read-object: func(handle: borrow<object>) -> result<list<u8>, host-error>",
            "read-object: func(handle: borrow<object>) -> list<u8>",
            "storage.read-object: must return result with shared host-error",
        ),
        (
            "request: func(url: string, method: string, headers: list<header>, body: list<u8>) -> result<response, host-error>",
            "request: func(url: string)",
            "http.request: must return result with shared host-error",
        ),
        (
            "publish: func(destination: string, payload: list<u8>) -> result<_, host-error>",
            "publish: func(payload: list<u8>)",
            "events.publish: must return result with shared host-error",
        ),
        (
            "start: func(workflow: string, input: list<u8>) -> result<execution, host-error>",
            "start: func(workflow: string, input: list<u8>) -> string",
            "workflows.start: must return result with shared host-error",
        ),
        (
            "report: func(completed: u64, total: u64, message: string) -> result<_, host-error>",
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
            "read-object: func(handle: borrow<object>) -> result<list<u8>, host-error>",
            "variant other-error { cancelled }\nread-object: func(handle: borrow<object>) -> result<list<u8>, other-error>",
            "storage.read-object: must return result with shared host-error",
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
            "open-object: func(key: string)",
            "open-object: func(tenant: string)",
            "forbidden scope parameter tenant",
        ),
        (
            "open-object: func(key: string)",
            "open-object: func(workspace: string)",
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
    let original = fixture();
    for (interface, resource) in RESOURCES {
        let before = format!(
            "/// An execution-scoped handle, invalid after completion.\n    resource {resource};"
        );
        let source = original.replace(&before, &format!("resource {resource};"));
        assert_ne!(
            source, original,
            "mutation must change {interface}.{resource}"
        );
        rejects(
            &source,
            &format!("{interface}: resource must be execution-scoped"),
        );
    }
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

#[test]
fn required_operation_cannot_be_removed() {
    let original = fixture();
    for (interface, operation, _) in OPERATIONS {
        let prefix = format!("{operation}: func(");
        let declarations: Vec<_> = original
            .lines()
            .filter(|line| line.trim_start().starts_with(&prefix))
            .collect();
        assert_eq!(
            declarations.len(),
            1,
            "fixture contains {interface}.{operation}"
        );
        let source = original.replace(&format!("{}\n", declarations[0]), "");
        assert_ne!(
            source, original,
            "mutation must remove {interface}.{operation}"
        );
        rejects(
            &source,
            &format!("missing operation: {interface}.{operation}"),
        );
    }
}

#[test]
fn required_resource_cannot_be_renamed() {
    let original = fixture();
    for (interface, resource) in RESOURCES {
        let renamed = format!("other-{resource}");
        let source = original
            .replace(
                &format!("resource {resource};"),
                &format!("resource {renamed};"),
            )
            .replace(&format!("<{resource}>"), &format!("<{renamed}>"))
            .replace(&format!("<{resource},"), &format!("<{renamed},"))
            .replace(&format!(".{{{resource}}}"), &format!(".{{{renamed}}}"));
        assert_ne!(
            source, original,
            "mutation must rename {interface}.{resource}"
        );
        rejects(
            &source,
            &format!("missing resource: {interface}.{resource}"),
        );
    }
}

#[test]
fn required_operation_signatures_cannot_change() {
    let original = fixture();
    for (operation, before, after) in [
        (
            "cancellation.is-cancelled",
            "is-cancelled: func() -> bool",
            "is-cancelled: func() -> u8",
        ),
        (
            "storage.open-object",
            "open-object: func(key: string) -> result<object, host-error>",
            "open-object: func(key: string) -> result<checkpoint, host-error>",
        ),
        (
            "storage.read-object",
            "read-object: func(handle: borrow<object>)",
            "read-object: func(handle: own<object>)",
        ),
        (
            "storage.write-object",
            "write-object: func(handle: borrow<object>, data: list<u8>)",
            "write-object: func(handle: borrow<object>, data: string)",
        ),
        (
            "storage.open-checkpoint",
            "open-checkpoint: func(key: string)",
            "open-checkpoint: func(key: u64)",
        ),
        (
            "storage.read-checkpoint",
            "read-checkpoint: func(handle: borrow<checkpoint>)",
            "read-checkpoint: func(handle: borrow<object>)",
        ),
        (
            "storage.write-checkpoint",
            "write-checkpoint: func(handle: borrow<checkpoint>, data: list<u8>)",
            "write-checkpoint: func(handle: borrow<checkpoint>)",
        ),
        (
            "http.request",
            "request: func(url: string, method: string,",
            "request: func(method: string, url: string,",
        ),
        (
            "events.publish",
            "publish: func(destination: string,",
            "publish: func(destination: list<u8>,",
        ),
        (
            "workflows.start",
            "start: func(workflow: string, input: list<u8>)",
            "start: func(workflow: string, input: list<u16>)",
        ),
        (
            "workflows.signal",
            "signal: func(handle: borrow<execution>,",
            "signal: func(handle: own<execution>,",
        ),
        (
            "logging.log",
            "log: func(severity: level, message: string, fields: list<field>) -> result<_, host-error>",
            "log: func(severity: level, message: string, fields: list<field>)",
        ),
        (
            "progress.report",
            "report: func(completed: u64,",
            "report: func(completed: u32,",
        ),
        (
            "progress.report-checkpoint",
            "use storage.{checkpoint};",
            "use storage.{object as checkpoint};",
        ),
        (
            "invocation-context.invocation-key",
            "invocation-key: func() -> string",
            "invocation-key: func() -> bool",
        ),
        (
            "invocation-context.attempt-id",
            "attempt-id: func() -> string",
            "attempt-id: func() -> u64",
        ),
        (
            "invocation-context.idempotency-key",
            "idempotency-key: func() -> string",
            "idempotency-key: func() -> list<u8>",
        ),
        (
            "http.request",
            "method: string, headers: list<header>,",
            "method: string, headers: list<response>,",
        ),
        (
            "logging.log",
            "log: func(severity: level,",
            "log: func(priority: level,",
        ),
        (
            "logging.log",
            "log: func(severity: level,",
            "log: func(severity: string,",
        ),
        (
            "storage.read-object",
            "read-object: func(handle: borrow<object>) -> result<list<u8>, host-error>",
            "read-object: func(handle: borrow<object>) -> result<list<u16>, host-error>",
        ),
        (
            "events.publish",
            "publish: func(destination: string, payload: list<u8>) -> result<_, host-error>",
            "publish: func(destination: string, payload: list<u8>) -> result<string, host-error>",
        ),
    ] {
        let source = original.replace(before, after);
        assert_ne!(source, original, "mutation must change {operation}");
        rejects(&source, &format!("{operation}: wrong signature"));
    }
}

#[test]
fn every_fallible_operation_requires_the_shared_error_identity() {
    let original = fixture();
    for (interface, operation, signature) in OPERATIONS {
        if !signature.contains(" -> result<") {
            continue;
        }
        let prefix = format!("{operation}: func(");
        let declaration = original
            .lines()
            .find(|line| line.trim_start().starts_with(&prefix))
            .expect("required operation in fixture");
        let source = original
            .replace(declaration, &declaration.replace("host-error", "other-error"))
            .replace(
                &format!("interface {interface} {{"),
                &format!("interface {interface} {{\n    variant other-error {{ denied, limit-exceeded, timeout, cancelled }}"),
            );
        assert_ne!(
            source, original,
            "mutation must change {interface}.{operation}"
        );
        rejects(
            &source,
            &format!("{interface}.{operation}: must return result with shared host-error"),
        );
    }
}
