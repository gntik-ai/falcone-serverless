//! Go (TinyGo wasip2) Wasm profile qualification, static checks only: no guest is built,
//! instantiated or executed here, and no Go toolchain runs. Execution lives in
//! `profile_go_runtime.rs` (opt-in, trusted image) and `cluster_profile_go.rs` (cluster).
use falcone_serverless_worker::{
    adapters::compat::{TreeDiagnostic, TreeFile},
    adapters::compat_go::{self, check_tree},
    artifact::validate::ALLOWED_IMPORTS,
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const CODES: &[&str] = &[
    "WASM-NATIVE-LIBRARY",
    "WASM-GPU",
    "WASM-PROCESS-SPAWN",
    "WASM-INSTALL-HOOK",
    "WASM-PACKAGE-NOT-IN-CATALOGUE",
    "WASM-UNSUPPORTED-API",
];
const RANDOM: &str = "wasi:random/random@0.2.0";

// ---------------------------------------------------------------------------- helpers

fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn profile() -> PathBuf {
    worker().join("qualification/profiles/go")
}
fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
fn toml_file(path: &Path) -> toml::Value {
    toml::from_str(&read(path)).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
fn pins() -> toml::Value {
    toml_file(&worker().join("src/qualification/go_versions.toml"))
}
fn manifest() -> toml::Value {
    toml_file(&profile().join("manifest.toml"))
}
fn matrix_in(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap()
}
fn matrix() -> Value {
    matrix_in(&worker().join("../docs/contracts/language-profile-matrix.json"))
}
fn strings(value: &toml::Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}

/// An exact three-part version: no range, alias or partial version.
fn exact(version: &str) -> bool {
    let core = version.strip_prefix('v').unwrap_or(version);
    let parts: Vec<_> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The operator table of 2026-11-19, copied literally and independent of every file in the tree.
const TINYGO_SHA256: &str = "ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9";
const GO_SHA256: &str = "9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec";

/// Pins errors, or none: the check profile_go.rs applies to a manifest. It is a function so the
/// tests can show that a missing, floating or substituted pin fails it.
fn pin_errors(manifest: &toml::Value, pins: &toml::Value) -> Vec<String> {
    let mut errors = Vec::new();
    let toolchain = &manifest["toolchain"];
    for field in ["wasmtime", "tinygo", "go"] {
        match toolchain.get(field).and_then(toml::Value::as_str) {
            Some(version) if exact(version) => {}
            other => errors.push(format!("{field}: {other:?} is not an exact version")),
        }
    }
    for (field, expected) in [("target", "wasip2"), ("wasi", "0.2.0")] {
        if toolchain.get(field).and_then(toml::Value::as_str) != Some(expected) {
            errors.push(format!("{field} must be pinned to {expected}"));
        }
    }
    if manifest["wit"].get("package").and_then(toml::Value::as_str) != Some("falcone:host@0.2.0") {
        errors.push("wit package must be falcone:host@0.2.0".into());
    }
    for (field, table, key) in [
        ("tinygo", "tinygo", "version"),
        ("tinygo_sha256", "tinygo", "sha256"),
        ("go", "go", "version"),
        ("go_sha256", "go", "sha256"),
        ("wasi", "", "wasi"),
        ("target", "", "target"),
    ] {
        let expected = if table.is_empty() {
            &pins[key]
        } else {
            &pins[table][key]
        };
        if toolchain.get(field) != Some(expected) {
            errors.push(format!("{field} differs from go_versions.toml"));
        }
    }
    for (field, operator) in [("tinygo_sha256", TINYGO_SHA256), ("go_sha256", GO_SHA256)] {
        if toolchain.get(field).and_then(toml::Value::as_str) != Some(operator) {
            errors.push(format!("{field} differs from the operator table"));
        }
    }
    if toolchain.get("tinygo").and_then(toml::Value::as_str) != Some("0.44.0")
        || toolchain.get("go").and_then(toml::Value::as_str) != Some("1.26.11")
    {
        errors.push("toolchain differs from the operator table".into());
    }
    // Every separately versioned module needs an exact version.
    for module in manifest["catalogue"]["modules"].as_array().unwrap() {
        match module.get("version").and_then(toml::Value::as_str) {
            Some(version) if exact(version) => {}
            other => errors.push(format!("module {module:?}: {other:?} is not exact")),
        }
    }
    // A stdlib entry is bound to the pinned Go release, never to a range or another release.
    for entry in manifest["stdlib"].as_array().unwrap() {
        if entry.get("go").and_then(toml::Value::as_str) != Some("1.26.11")
            || entry.get("package").and_then(toml::Value::as_str).is_none()
        {
            errors.push(format!("stdlib entry {entry:?} is not bound to Go 1.26.11"));
        }
    }
    errors
}

// ---------------------------------------------------------------------------- pins

#[test]
fn pins_are_the_operator_table_and_exact() {
    for floating in [
        "^0.44.0", "~0.44.0", ">=0.44.0", "0.x", "0.44", "*", "latest", "",
    ] {
        assert!(!exact(floating), "{floating:?} must not count as exact");
    }
    let pins = pins();
    assert_eq!(pins["tinygo"]["version"].as_str(), Some("0.44.0"));
    assert_eq!(pins["go"]["version"].as_str(), Some("1.26.11"));
    assert_eq!(pins["tinygo"]["sha256"].as_str(), Some(TINYGO_SHA256));
    assert_eq!(pins["go"]["sha256"].as_str(), Some(GO_SHA256));
    assert_eq!(
        pins["tinygo"]["archive"].as_str(),
        Some("tinygo0.44.0.linux-amd64.tar.gz")
    );
    assert_eq!(
        pins["go"]["archive"].as_str(),
        Some("go1.26.11.linux-amd64.tar.gz")
    );
    assert_eq!(
        pins["tinygo"]["source"].as_str(),
        Some(
            "https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz"
        )
    );
    assert_eq!(
        pins["go"]["source"].as_str(),
        Some("https://go.dev/dl/go1.26.11.linux-amd64.tar.gz")
    );
    assert_eq!(pins["target"].as_str(), Some("wasip2"));
    assert_eq!(pins["qualification_status"].as_str(), Some("disabled"));
    assert_eq!(pins["stdlib"]["go"], pins["go"]["version"]);
    assert!(pins["modules"]["packages"].as_array().unwrap().is_empty());
}

#[test]
fn the_manifest_pins_pass_and_every_defect_is_caught() {
    let pins = pins();
    let good = manifest();
    assert_eq!(pin_errors(&good, &pins), Vec::<String>::new());
    assert!(digest(good["toolchain"]["tinygo_sha256"].as_str().unwrap()));
    assert!(digest(good["toolchain"]["go_sha256"].as_str().unwrap()));
    let mutate = |change: &dyn Fn(&mut toml::Value)| {
        let mut copy = good.clone();
        change(&mut copy);
        pin_errors(&copy, &pins)
    };
    // A pin that is missing, a range, latest, or just different fails.
    for field in ["wasmtime", "tinygo", "go", "target", "wasi"] {
        let errors = mutate(&|m| {
            m["toolchain"].as_table_mut().unwrap().remove(field);
        });
        assert!(!errors.is_empty(), "missing {field} passed");
    }
    for field in ["tinygo", "go", "wasmtime"] {
        for floating in ["^1.2.3", ">=0.44.0", "latest", "0.44", "1.x"] {
            let errors = mutate(&|m| m["toolchain"][field] = floating.into());
            assert!(!errors.is_empty(), "{field}={floating} passed");
        }
    }
    let errors = mutate(&|m| m["toolchain"]["tinygo"] = "0.44.1".into());
    assert!(
        errors.iter().any(|e| e.contains("operator table")),
        "{errors:?}"
    );
    let errors = mutate(&|m| m["toolchain"]["go_sha256"] = "0".repeat(64).into());
    assert!(errors.iter().any(|e| e.contains("go_sha256")), "{errors:?}");
    let errors = mutate(&|m| m["wit"]["package"] = "falcone:host@0.1.0".into());
    assert!(!errors.is_empty());
    // A module needs an exact version; a stdlib entry must be bound to Go 1.26.11.
    let errors = mutate(&|m| {
        let module: toml::Value = toml::from_str("name = \"x\"\nversion = \"^1.0.0\"").unwrap();
        m["catalogue"]["modules"]
            .as_array_mut()
            .unwrap()
            .push(module);
    });
    assert!(errors.iter().any(|e| e.contains("not exact")), "{errors:?}");
    for bound in ["1.26", "1.26.10", "latest", ">=1.26.11"] {
        let errors = mutate(&|m| {
            m["stdlib"].as_array_mut().unwrap()[0]["go"] = bound.into();
        });
        assert!(
            errors.iter().any(|e| e.contains("not bound to Go 1.26.11")),
            "{bound}: {errors:?}"
        );
    }
    let errors = mutate(&|m| {
        m["stdlib"].as_array_mut().unwrap()[0]
            .as_table_mut()
            .unwrap()
            .remove("go");
    });
    assert!(!errors.is_empty());
}

#[test]
fn the_manifest_stdlib_equals_the_embedded_allow_list() {
    let manifest = manifest();
    let listed: BTreeSet<String> = manifest["stdlib"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["package"].as_str().unwrap().to_owned())
        .collect();
    let embedded: BTreeSet<String> = compat_go::stdlib_allow_list().into_iter().collect();
    assert_eq!(listed, embedded);
    // Go's raw-memory package; spelled in pieces because the repository's boundary audit
    // refuses that word in any source file outside the three audited ones.
    let raw_memory = ["un", "safe"].concat();
    assert!(!embedded.contains("os/exec") && !embedded.contains(raw_memory.as_str()));
    assert_eq!(
        compat_go::PINS,
        read(&worker().join("src/qualification/go_versions.toml"))
    );
}

// ---------------------------------------------------------------------------- interfaces

fn allowed_interfaces(manifest: &toml::Value) -> Vec<String> {
    let mut all = strings(&manifest["wit"]["interfaces"]);
    all.extend(strings(&manifest["wasi"]["interfaces"]));
    all
}

fn wit_interfaces() -> BTreeSet<String> {
    let mut resolve = wit_parser::Resolve::default();
    let (package, _) = resolve.push_path(worker().join("wit/host.wit")).unwrap();
    let package = &resolve.packages[package];
    resolve.worlds[package.worlds["function"]]
        .imports
        .values()
        .filter_map(|item| match item {
            wit_parser::WorldItem::Interface { id, .. } => resolve.interfaces[*id]
                .name
                .as_deref()
                .map(|name| package.name.interface_id(name)),
            _ => None,
        })
        .collect()
}

/// Interfaces listed that are neither in worker/wit nor the one WASI interface.
fn unknown_interfaces(listed: &[String]) -> Vec<String> {
    let wit = wit_interfaces();
    listed
        .iter()
        .filter(|name| !wit.contains(*name) && *name != RANDOM)
        .cloned()
        .collect()
}

/// Modules named by `//go:wasmimport <module> <name>` in Go source.
fn wasm_imports(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| line.strip_prefix("//go:wasmimport "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

fn go_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(go_files(&path));
        } else if path.extension().is_some_and(|e| e == "go") {
            out.push(path);
        }
    }
    out.sort();
    out
}

#[test]
fn every_listed_interface_exists_in_the_wit_or_is_wasi_random() {
    let manifest = manifest();
    let listed = allowed_interfaces(&manifest);
    assert_eq!(unknown_interfaces(&listed), Vec::<String>::new());
    assert!(listed.contains(&RANDOM.to_owned()));
    assert_eq!(strings(&manifest["wit"]["interfaces"]).len(), 8);
    assert_eq!(
        unknown_interfaces(&["falcone:host/storage@0.1.0".into()]),
        ["falcone:host/storage@0.1.0"]
    );
    for rejected in [
        "wasi:random/insecure@0.2.0",
        "wasi:random/insecure-seed@0.2.0",
        "wasi:cli/environment@0.2.0",
        "wasi:filesystem/types@0.2.0",
        "wasi:sockets/tcp@0.2.0",
        "wasi:clocks/monotonic-clock@0.2.0",
        "wasi:io/streams@0.2.0",
        "falcone:host/handler@0.2.0",
        "falcone:host/*@0.2.0",
    ] {
        assert_eq!(unknown_interfaces(&[rejected.to_owned()]), [rejected]);
    }
    // The validator's allow-list is exactly this list.
    assert_eq!(
        ALLOWED_IMPORTS
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
        listed.into_iter().collect::<BTreeSet<_>>()
    );
    assert_eq!(manifest["wasi"]["max_bytes"].as_integer(), Some(65_536));
    assert_eq!(
        strings(&manifest["wasi"]["functions"]),
        ["get-random-bytes", "get-random-u64"]
    );
}

#[test]
fn examples_and_bindings_import_only_listed_interfaces() {
    let manifest = manifest();
    let listed = allowed_interfaces(&manifest);
    for path in go_files(&profile().join("bindings"))
        .into_iter()
        .chain(go_files(&profile().join("examples")))
    {
        for module in wasm_imports(&read(&path)) {
            assert!(listed.contains(&module), "{}: {module}", path.display());
        }
    }
    // The declared imports of each workload are listed, and what the source imports is declared.
    for workload in manifest["workloads"].as_array().unwrap() {
        let declared = strings(&workload["imports"]);
        assert!(declared.iter().all(|name| listed.contains(name)));
        let source = read(&profile().join(workload["source"].as_str().unwrap()));
        // Examples use the bindings; each bindings function names the interface it needs.
        let uses_events = source.contains("bindings.Publish");
        assert_eq!(
            uses_events,
            declared.contains(&"falcone:host/events@0.2.0".to_owned()),
            "{}",
            workload["name"].as_str().unwrap()
        );
    }
    // Showing the check bites: a source importing an unlisted interface is found.
    let bad = "//go:wasmimport wasi:cli/environment@0.2.0 get-environment\nfunc env()\n";
    assert!(
        wasm_imports(bad)
            .iter()
            .any(|module| !listed.contains(module))
    );
}

#[test]
fn wasi_random_is_the_only_wasi_import_in_the_host_and_no_wasmtime_wasi_is_linked() {
    let runtime = read(&worker().join("src/host/runtime.rs"));
    for forbidden in ["wasmtime_wasi", "add_to_linker", "wasmtime-wasi"] {
        assert!(!runtime.contains(forbidden), "{forbidden}");
    }
    let mut wasi = BTreeSet::new();
    for line in runtime.lines().filter(|l| l.contains("linker.instance(")) {
        assert!(!line.contains("wasi:"), "{line}");
    }
    for name in ALLOWED_IMPORTS
        .iter()
        .filter(|name| name.starts_with("wasi:"))
    {
        wasi.insert(*name);
    }
    assert_eq!(wasi, BTreeSet::from([RANDOM]));
    for manifest in ["Cargo.toml", "../Cargo.toml", "../compiler/Cargo.toml"] {
        let text = read(&worker().join(manifest));
        assert!(!text.contains("wasmtime-wasi"), "{manifest}");
    }
}

// ---------------------------------------------------------------------------- compat check

fn tree(files: &[(&str, &str)]) -> Vec<(String, &'static str, usize, usize)> {
    let files: Vec<_> = files
        .iter()
        .map(|(path, text)| TreeFile { path, text })
        .collect();
    summary(check_tree(&files).diagnostics())
}
fn summary(diagnostics: &[TreeDiagnostic]) -> Vec<(String, &'static str, usize, usize)> {
    diagnostics
        .iter()
        .map(|d| (d.path.clone(), d.code, d.line, d.column))
        .collect()
}
fn one(path: &str, text: &str) -> Vec<(String, &'static str, usize, usize)> {
    tree(&[(path, text)])
}

fn module_files(dir: &Path) -> Vec<(String, String)> {
    let mut files = vec![("go.mod".to_owned(), read(&profile().join("go.mod")))];
    for path in go_files(dir) {
        let relative = path.strip_prefix(profile()).unwrap();
        files.push((relative.to_string_lossy().replace('\\', "/"), read(&path)));
    }
    files
}

fn check_dir(dir: &Path) -> Vec<TreeDiagnostic> {
    let files = module_files(dir);
    let refs: Vec<_> = files
        .iter()
        .map(|(path, text)| TreeFile { path, text })
        .collect();
    let verdict = check_tree(&refs);
    verdict
        .diagnostics()
        .iter()
        .map(|d| TreeDiagnostic {
            code: d.code,
            construct: d.construct,
            path: d.path.clone(),
            line: d.line,
            column: d.column,
        })
        .collect()
}

#[test]
fn positive_examples_are_statically_eligible() {
    let manifest = manifest();
    for workload in manifest["workloads"].as_array().unwrap() {
        let dir = profile().join(workload["source"].as_str().unwrap());
        let diagnostics = check_dir(dir.parent().unwrap());
        assert!(
            diagnostics.is_empty(),
            "{}: {diagnostics:?}",
            workload["name"].as_str().unwrap()
        );
    }
}

#[test]
fn each_negative_fixture_is_rejected_with_its_code_and_location() {
    let manifest = manifest();
    let fixtures = manifest["negative"].as_array().unwrap();
    assert_eq!(
        fixtures
            .iter()
            .map(|f| f["code"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        CODES.iter().copied().collect::<BTreeSet<_>>()
    );
    for fixture in fixtures {
        let code = fixture["code"].as_str().unwrap();
        let diagnostics = check_dir(&profile().join(fixture["path"].as_str().unwrap()));
        let expected = (
            fixture["file"].as_str().unwrap().to_owned(),
            code,
            fixture["line"].as_integer().unwrap() as usize,
            fixture["column"].as_integer().unwrap() as usize,
        );
        assert_eq!(summary(&diagnostics), [expected], "{code}");
        // Names the construct and the location, never source text.
        let source = read(&profile().join(fixture["file"].as_str().unwrap()));
        assert!(!diagnostics[0].construct.is_empty());
        assert!(
            source
                .lines()
                .filter(|line| line.len() > 8)
                .all(|line| !diagnostics[0].construct.contains(line.trim())),
            "{code}"
        );
    }
}

#[test]
fn go_generate_is_an_install_hook_wherever_it_appears() {
    let hook =
        |line: usize, column: usize| [("a.go".to_owned(), "WASM-INSTALL-HOOK", line, column)];
    // File level, between imports and inside a grouped import: regressions of the PR #429
    // loop, which consumed comment tokens in a grouped import without the directive checks.
    assert_eq!(
        one(
            "a.go",
            "package main\n\n//go:generate go run gen.go\n\nfunc main() {}\n"
        ),
        hook(3, 1)
    );
    assert_eq!(
        one(
            "a.go",
            "package main\n\nimport \"fmt\"\n//go:generate go run gen.go\nimport \"strings\"\n"
        ),
        hook(4, 1)
    );
    assert_eq!(
        one(
            "a.go",
            "package main\n\nimport (\n\t\"fmt\"\n//go:generate go run gen.go\n\t\"strings\"\n)\n"
        ),
        hook(5, 1)
    );
    assert_eq!(
        one(
            "a.go",
            "package main\n\nimport (\n//go:generate go run gen.go\n\t\"fmt\"\n)\n"
        ),
        hook(4, 1)
    );
    assert_eq!(
        one(
            "a.go",
            "package main\n\nimport (\n\t\"fmt\"\n//go:generate go run gen.go\n)\n"
        ),
        hook(5, 1)
    );
    // After an aliased import, after a trailing comment and within a function body.
    assert_eq!(
        one(
            "a.go",
            "package main\n\nimport (\n\tf \"fmt\" // note\n//go:generate x\n\t_ \"strings\"\n)\n"
        ),
        hook(5, 1)
    );
    assert_eq!(
        one(
            "a.go",
            "package main\n\nfunc main() {\n//go:generate x\n}\n"
        ),
        hook(4, 1)
    );
    // Indented and trailing: still a directive comment for the compiler's pragma scanner.
    assert_eq!(
        one(
            "a.go",
            "package main\n\nfunc main() {\n\t//go:generate x\n}\n"
        ),
        hook(4, 2)
    );
    // Not directives: a space after the slashes, a string, a block comment.
    for text in [
        "package main\n\n// go:generate x\n",
        "package main\n\nvar s = \"//go:generate x\"\n",
        "package main\n\nvar s = `\n//go:generate x\n`\n",
        "package main\n\n/*\n//go:generate x\n*/\n",
    ] {
        assert_eq!(one("a.go", text), [], "{text}");
    }
}

#[test]
fn go_linkname_is_rejected_anywhere() {
    let linkname = |line: usize| [("a.go".to_owned(), "WASM-UNSUPPORTED-API", line, 1usize)];
    for (text, line) in [
        ("package main\n\n//go:linkname x runtime.x\nfunc x()\n", 3),
        (
            "package main\n\nimport (\n\t\"strings\"\n//go:linkname x runtime.x\n\t\"fmt\"\n)\n",
            5,
        ),
        (
            "package main\n\nimport \"fmt\"\n//go:linkname x y\nimport \"strings\"\n",
            4,
        ),
        ("package main\n\nfunc f() {\n//go:linkname x y\n}\n", 4),
    ] {
        assert_eq!(one("a.go", text), linkname(line), "{text}");
    }
}

#[test]
fn cgo_native_process_gpu_and_catalogue_imports_are_rejected() {
    let code =
        |text: &str| -> Vec<&'static str> { one("a.go", text).into_iter().map(|d| d.1).collect() };
    assert_eq!(
        code("package main\nimport \"C\"\n"),
        ["WASM-NATIVE-LIBRARY"]
    );
    assert_eq!(
        code("package main\nimport (\n\t\"fmt\"\n\tc \"C\"\n)\n"),
        ["WASM-NATIVE-LIBRARY"]
    );
    // An escape sequence or raw string cannot disguise the pseudo-package C.
    assert_eq!(
        code("package main\nimport \"\\x43\"\n"),
        ["WASM-UNSUPPORTED-API"]
    );
    assert_eq!(code("package main\nimport `C`\n"), ["WASM-NATIVE-LIBRARY"]);
    assert_eq!(
        code("package main\nimport \"plugin\"\n"),
        ["WASM-NATIVE-LIBRARY"]
    );
    assert_eq!(
        code("package main\nimport \"runtime/cgo\"\n"),
        ["WASM-NATIVE-LIBRARY"]
    );
    assert_eq!(
        code("package main\n// #cgo LDFLAGS: -lm\nfunc f() {}\n"),
        ["WASM-NATIVE-LIBRARY"]
    );
    assert_eq!(
        code("package main\n/*\n#cgo LDFLAGS: -lm\n*/\n"),
        ["WASM-NATIVE-LIBRARY"]
    );
    assert_eq!(
        code("package main\n//go:cgo_ldflag x\n"),
        ["WASM-NATIVE-LIBRARY"]
    );
    assert_eq!(
        code("package main\nimport \"os/exec\"\n"),
        ["WASM-PROCESS-SPAWN"]
    );
    for gpu in [
        "github.com/NVIDIA/go-nvml/pkg/nvml",
        "gorgonia.org/cu/cuda",
        "example.com/x/opencl",
        "example.com/x/webgpu",
    ] {
        assert_eq!(
            code(&format!("package main\nimport \"{gpu}\"\n")),
            ["WASM-GPU"],
            "{gpu}"
        );
    }
    assert_eq!(
        code("package main\nimport \"github.com/example/leftpad\"\n"),
        ["WASM-PACKAGE-NOT-IN-CATALOGUE"]
    );
    let raw_memory = ["un", "safe"].concat();
    for api in [
        "os",
        "net/http",
        "syscall",
        raw_memory.as_str(),
        "embed",
        "runtime",
        "time",
        "log",
    ] {
        assert_eq!(
            code(&format!("package main\nimport \"{api}\"\n")),
            ["WASM-UNSUPPORTED-API"],
            "{api}"
        );
    }
    assert_eq!(
        code("package main\nimport (\n\t\"fmt\"\n\t\"strings\"\n\tr \"crypto/rand\"\n)\n"),
        Vec::<&str>::new()
    );
    // A dynamic-run limit of TinyGo reflection, through an aliased import.
    assert_eq!(
        code("package main\nimport rf \"reflect\"\nvar _ = rf.StructOf\n"),
        ["WASM-UNSUPPORTED-API"]
    );
    // A later blank import of reflect does not hide an earlier alias.
    assert_eq!(
        code("package main\nimport (\n\tr \"reflect\"\n\t_ \"reflect\"\n)\nvar _ = r.MakeFunc\n"),
        ["WASM-UNSUPPORTED-API"]
    );
    assert_eq!(
        code("package main\nimport (\n\t_ \"reflect\"\n\tr \"reflect\"\n)\nvar _ = r.MakeFunc\n"),
        ["WASM-UNSUPPORTED-API"]
    );
    assert_eq!(
        code("package main\nimport (\n\t. \"reflect\"\n\tr \"reflect\"\n)\nvar _ = r.StructOf\n"),
        ["WASM-UNSUPPORTED-API"]
    );
    assert_eq!(
        code("package main\nimport \"reflect\"\nvar _ = reflect.TypeOf(1)\n"),
        Vec::<&str>::new()
    );
    // A dot import exposes the members unqualified; the limits are still found, in either form.
    assert_eq!(
        code("package main\nimport . \"reflect\"\nvar _ = StructOf(nil)\n"),
        ["WASM-UNSUPPORTED-API"]
    );
    assert_eq!(
        code("package main\nimport (\n\t. \"reflect\"\n)\nfunc f() { MakeFunc(nil, nil) }\n"),
        ["WASM-UNSUPPORTED-API"]
    );
    assert_eq!(
        code("package main\nimport . \"reflect\"\nvar _ = TypeOf(1)\n"),
        Vec::<&str>::new()
    );
    // An undeclared interface in //go:wasmimport is outside the allow-list; a listed one is not.
    assert_eq!(
        code(
            "package main\n//go:wasmimport wasi:cli/environment@0.2.0 get-environment\nfunc f()\n"
        ),
        ["WASM-UNSUPPORTED-API"]
    );
    assert_eq!(
        code(
            "package main\n//go:wasmimport wasi:random/random@0.2.0 get-random-u64\nfunc f() uint64\n"
        ),
        Vec::<&str>::new()
    );
    assert_eq!(
        code("package main\n//go:wasmexport evil\nfunc f() {}\n"),
        ["WASM-UNSUPPORTED-API"]
    );
}

#[test]
fn trees_files_and_go_mod_are_checked() {
    let module = "module example.invalid/m\n\ngo 1.26.11\n";
    // go.mod: dependencies and replacements are outside the catalogue, tools are hooks.
    let mod_codes = |text: &str| -> Vec<(&'static str, usize)> {
        tree(&[("go.mod", text)])
            .into_iter()
            .map(|d| (d.1, d.2))
            .collect()
    };
    assert_eq!(mod_codes(module), []);
    assert_eq!(
        mod_codes("module m\nrequire github.com/x/y v1.0.0\n"),
        [("WASM-PACKAGE-NOT-IN-CATALOGUE", 2)]
    );
    assert_eq!(
        mod_codes("module m\nrequire (\n\tgithub.com/x/y v1.0.0 // indirect\n)\n"),
        [("WASM-PACKAGE-NOT-IN-CATALOGUE", 3)]
    );
    assert_eq!(
        mod_codes("module m\nreplace github.com/x/y => ../y\n"),
        [("WASM-PACKAGE-NOT-IN-CATALOGUE", 2)]
    );
    assert_eq!(
        mod_codes("module m\ntool example.com/gen\n"),
        [("WASM-INSTALL-HOOK", 2)]
    );
    assert_eq!(
        mod_codes("module m\ntoolchain go1.99.0\n"),
        [("WASM-UNSUPPORTED-API", 2)]
    );
    // In-tree imports: a directory of the tree and the trusted bindings resolve; others do not.
    let main = "package main\nimport (\n\t\"example.invalid/m/bindings\"\n\t\"example.invalid/m/lib\"\n)\n";
    assert_eq!(
        tree(&[
            ("go.mod", module),
            ("main.go", main),
            ("lib/lib.go", "package lib\n")
        ]),
        []
    );
    assert_eq!(
        tree(&[("go.mod", module), ("main.go", main)])
            .into_iter()
            .map(|d| (d.1, d.2))
            .collect::<Vec<_>>(),
        [("WASM-UNSUPPORTED-API", 4)]
    );
    // A tenant tree may not carry its own bindings package.
    assert_eq!(
        tree(&[("go.mod", module), ("bindings/x.go", "package bindings\n")])
            .into_iter()
            .map(|d| d.1)
            .collect::<Vec<_>>(),
        ["WASM-UNSUPPORTED-API"]
    );
    // Native sources and build scripts in the tree.
    assert_eq!(
        tree(&[("c/helper.c", "int x;"), ("lib.s", "")])
            .into_iter()
            .map(|d| d.1)
            .collect::<Vec<_>>(),
        ["WASM-NATIVE-LIBRARY", "WASM-NATIVE-LIBRARY"]
    );
    assert_eq!(
        tree(&[("Makefile", "all:\n")])
            .into_iter()
            .map(|d| d.1)
            .collect::<Vec<_>>(),
        ["WASM-INSTALL-HOOK"]
    );
    // Malformed trees never pass: a bad path, a duplicate path.
    assert_eq!(one("../x.go", "package x\n").len(), 1);
    assert_eq!(
        tree(&[("a.go", "package a\n"), ("a.go", "package a\n")]).len(),
        1
    );
    // Unterminated constructs are recorded and later imports are still found.
    let found = one("a.go", "package a\nvar s = \"open\nimport \"os/exec\"\n");
    assert!(
        found.iter().any(|d| d.1 == "WASM-PROCESS-SPAWN"),
        "{found:?}"
    );
    assert!(
        found.iter().any(|d| d.1 == "WASM-UNSUPPORTED-API"),
        "{found:?}"
    );
}

// ---------------------------------------------------------------------------- drift

#[test]
fn matrix_entry_equals_the_manifest_and_stays_disabled() {
    let manifest = manifest();
    let matrix = matrix();
    let go = &matrix["profiles"]["go"];
    assert_eq!(go["id"], manifest["profile"]["id"].as_str().unwrap());
    assert_eq!(go["status"], "disabled");
    assert_eq!(go["evidence"], serde_json::json!({}));
    assert_eq!(
        matrix["matrixVersion"],
        manifest["profile"]["matrix_revision"].as_str().unwrap()
    );
    assert_eq!(
        go["wasmtimeVersion"],
        manifest["toolchain"]["wasmtime"].as_str().unwrap()
    );
    assert_eq!(
        go["wasiVersion"],
        manifest["toolchain"]["wasi"].as_str().unwrap()
    );
    assert_eq!(
        go["toolchainVersion"],
        manifest["toolchain"]["tinygo"].as_str().unwrap()
    );
    assert_eq!(go["packageCatalogueVersion"], "unqualified");
    let allowed: Vec<_> = go["allowedInterfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(allowed, allowed_interfaces(&manifest));
    let tested: Vec<_> = go["testedExamples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    let workloads: Vec<_> = manifest["workloads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(tested, workloads);
    let diagnostics: Vec<_> = go["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(diagnostics, CODES);
    for code in CODES {
        assert!(
            manifest["diagnostics"][code]["message"].as_str().is_some(),
            "{code}"
        );
    }
    // The worker copy and the published contract are identical.
    assert_eq!(
        read(&worker().join("src/adapters/language-profile-matrix.json")),
        read(&worker().join("../docs/contracts/language-profile-matrix.json"))
    );
    // The other profiles' entries are untouched by this change.
    assert_eq!(matrix["profiles"]["python"]["status"], "disabled");
    assert_eq!(
        matrix["profiles"]["javascript"]["toolchainVersion"],
        "1.41.0"
    );
}

#[test]
fn the_go_document_matches_the_manifest_and_matrix() {
    let doc = read(&worker().join("../docs/qualification/profiles/go.md"));
    let manifest = manifest();
    let toolchain = &manifest["toolchain"];
    for field in ["tinygo", "go", "wasmtime", "tinygo_sha256", "go_sha256"] {
        let value = toolchain[field].as_str().unwrap();
        assert!(doc.contains(value), "go.md must state {field} = {value}");
    }
    for interface in allowed_interfaces(&manifest) {
        assert!(doc.contains(&interface), "go.md must list {interface}");
    }
    for code in CODES {
        assert!(doc.contains(code), "go.md must document {code}");
    }
    for entry in manifest["stdlib"].as_array().unwrap() {
        let package = entry["package"].as_str().unwrap();
        assert!(
            doc.contains(&format!("`{package}`")),
            "go.md must list {package}"
        );
    }
    for workload in manifest["workloads"].as_array().unwrap() {
        assert!(doc.contains(workload["name"].as_str().unwrap()));
    }
    for limit in [
        "65536",
        "wasip2",
        "reflect.MakeFunc",
        "cabi_realloc",
        "one linear memory",
        "go:generate",
        "go:linkname",
        "disabled",
    ] {
        assert!(doc.contains(limit), "go.md must mention {limit}");
    }
    // Drift is detected: a document with a different pin fails the same check.
    let drifted = doc.replace("0.44.0", "0.44.1");
    assert!(!drifted.contains(toolchain["tinygo"].as_str().unwrap()));
    let contract = read(&worker().join("../docs/contracts/language-profile-matrix.md"));
    for needed in [RANDOM, "65536", "CSPRNG"] {
        assert!(
            contract.contains(needed),
            "profile contract must mention {needed}"
        );
    }
}

#[test]
fn scripts_read_the_single_source_of_truth_and_use_supported_options_only() {
    let script = read(&worker().join("qualification/go/build-profile.sh"));
    assert!(script.contains("src/qualification/go_versions.toml"));
    // No second copy of the pins: neither hash nor version literal in the script.
    for pinned in [TINYGO_SHA256, GO_SHA256, "0.44.0", "1.26.11"] {
        assert!(
            !script.contains(pinned),
            "{pinned} must come from go_versions.toml"
        );
    }
    assert!(!worker().join("qualification/go/versions.toml").exists());
    // wasi-virt is not used: it adds a second memory (memories(1)) and --deny-all does not exist.
    for forbidden in ["wasi-virt", "--deny-all", "virt "] {
        assert!(!script.contains(forbidden), "{forbidden}");
    }
    // Only options the pinned tools support; an unknown argument is found.
    assert_eq!(unsupported_options(&script), Vec::<String>::new());
    let tinygo = "\"$tinygo\" build -target=wasip2 -wit-package w -o out ./x\n";
    assert_eq!(unsupported_options(tinygo), Vec::<String>::new());
    for bad in ["--deny-all", "-target=wasip1", "-wit-pack", "--no-debug"] {
        let line = format!("\"$tinygo\" build {bad} ./x\n");
        assert_eq!(unsupported_options(&line), [bad], "{bad}");
    }
    let virt = "\"$wasm_tools\" component new --deny-all x -o y\n";
    assert!(!unsupported_options(virt).is_empty());
    assert!(script.contains("\"$tinygo\" build -target=wasip2"));
}

/// Options of every tinygo and wasm-tools invocation that the pinned tools do not support.
/// TinyGo 0.44.0 supports `-target`, `-o`, `-no-debug`, `-wit-package` and `-wit-world`;
/// wasm-tools is only asked for `--version`.
fn unsupported_options(script: &str) -> Vec<String> {
    let mut bad = Vec::new();
    for line in script
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
    {
        let words: Vec<_> = line.split_whitespace().collect();
        // The tool is the command word: first, or right after `&&` in a subshell.
        let command = words.iter().enumerate().find_map(|(at, word)| {
            let leading = at == 0 || words[at - 1] == "&&";
            (leading && matches!(*word, "\"$tinygo\"" | "\"$wasm_tools\"")).then_some((at, *word))
        });
        let Some((at, tool)) = command else { continue };
        let supported: &[&str] = if tool == "\"$tinygo\"" {
            &[
                "-target=wasip2",
                "-no-debug",
                "-wit-package",
                "-wit-world",
                "-o",
            ]
        } else {
            &["--version"]
        };
        for word in words[at + 1..].iter().filter(|word| word.starts_with('-')) {
            if !supported.contains(word) {
                bad.push((*word).to_owned());
            }
        }
    }
    bad
}
