//! Offline proof that `worker/Containerfile` copies everything the workspace build needs.
//!
//! No cluster, registry or container engine is involved. The COPY set is read from the real
//! file and checked against the root `Cargo.toml` members and the `include_str!` /
//! `include_bytes!` inputs of the worker sources. An opt-in `podman build` of the real file
//! runs only with `FS_TEST_IMAGE_BUILD=1` (it is skipped by default and never a CI gate).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap()
}

/// Sources of every `COPY` in the build stage, relative to the build context root.
fn copy_sources(containerfile: &str) -> BTreeSet<String> {
    let mut sources = BTreeSet::new();
    for line in containerfile.lines() {
        let mut words = line.split_whitespace();
        if !words.next().is_some_and(|w| w.eq_ignore_ascii_case("COPY")) {
            continue;
        }
        let words: Vec<_> = words.filter(|w| !w.starts_with("--")).collect();
        // The last word is the destination; a --from copy never reads the build context.
        if line.contains("--from") || words.len() < 2 {
            continue;
        }
        for source in &words[..words.len() - 1] {
            sources.insert(source.trim_end_matches('/').to_string());
        }
    }
    sources
}

fn workspace_members(manifest: &str) -> Vec<String> {
    let value: toml::Table = manifest.parse().unwrap();
    value["workspace"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member.as_str().unwrap().to_string())
        .collect()
}

/// A path is covered when it is a copied source or lies below one.
fn covered(copied: &BTreeSet<String>, path: &str) -> bool {
    copied
        .iter()
        .any(|source| path == source || path.starts_with(&format!("{source}/")))
}

/// Everything missing from the COPY set for the workspace to load and compile.
fn missing(containerfile: &str, manifest: &str, includes: &[String]) -> Vec<String> {
    let copied = copy_sources(containerfile);
    let mut needed: Vec<String> = vec!["Cargo.toml".into(), "Cargo.lock".into()];
    for member in workspace_members(manifest) {
        needed.push(format!("{member}/Cargo.toml"));
        needed.push(format!("{member}/src"));
    }
    needed.extend(includes.iter().cloned());
    needed
        .into_iter()
        .filter(|path| !covered(&copied, path))
        .collect()
}

/// Context-relative target of an include literal written in `source`, or why it escapes.
fn resolve_include(source: &Path, literal: &str) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    let joined = source.parent().unwrap().join(literal);
    for component in joined.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(format!(
                        "{}: {literal:?} climbs above the build context",
                        source.display()
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "{}: {literal:?} is an absolute path",
                    source.display()
                ));
            }
        }
    }
    Ok(parts.join("/"))
}

/// Every `include_str!`/`include_bytes!` in `text`, resolved against `source` (context-relative).
fn scan_source(source: &Path, text: &str) -> Vec<Result<String, String>> {
    let mut results = Vec::new();
    for macro_name in ["include_str!", "include_bytes!"] {
        for part in text.split(macro_name).skip(1) {
            let literal = part
                .trim_start()
                .trim_start_matches('(')
                .trim_start()
                .strip_prefix('"')
                .and_then(|rest| rest.split('"').next());
            results.push(match literal {
                Some(literal) => resolve_include(source, literal),
                None => Err(format!("{}: non-literal include path", source.display())),
            });
        }
    }
    results
}

/// Context-relative target of every `include_str!`/`include_bytes!` below `dir`.
fn include_targets(
    dir: &Path,
    context: &Path,
    found: &mut BTreeSet<String>,
    escapes: &mut Vec<String>,
) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            include_targets(&path, context, found, escapes);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = fs::read_to_string(&path).unwrap();
            let relative = path.strip_prefix(context).unwrap();
            for result in scan_source(relative, &text) {
                match result {
                    Ok(target) => {
                        found.insert(target);
                    }
                    Err(reason) => escapes.push(reason),
                }
            }
        }
    }
}

fn worker_includes() -> Vec<String> {
    let context = root().canonicalize().unwrap();
    let mut found = BTreeSet::new();
    let mut escapes = Vec::new();
    include_targets(
        &context.join("worker/src"),
        &context,
        &mut found,
        &mut escapes,
    );
    include_targets(
        &context.join("compiler/src"),
        &context,
        &mut found,
        &mut escapes,
    );
    assert_eq!(escapes, Vec::<String>::new());
    assert!(
        found.contains("docs/contracts/worker-runtime-config.md"),
        "the scan must see the docs/contracts inputs: {found:?}"
    );
    found.into_iter().collect()
}

#[test]
fn committed_containerfile_copies_every_workspace_member_and_include_input() {
    let includes = worker_includes();
    assert_eq!(
        missing(
            &read("worker/Containerfile"),
            &read("Cargo.toml"),
            &includes
        ),
        Vec::<String>::new()
    );
}

#[test]
fn every_include_resolves_to_an_existing_file_inside_a_copied_directory() {
    let containerfile = read("worker/Containerfile");
    let copied = copy_sources(&containerfile);
    for include in worker_includes() {
        assert!(root().join(&include).is_file(), "{include} does not exist");
        assert!(covered(&copied, &include), "{include} is outside COPY set");
    }
}

#[test]
fn fixture_containerfiles_missing_inputs_are_rejected() {
    let committed = read("worker/Containerfile");
    let manifest = read("Cargo.toml");
    let includes = worker_includes();
    for (without, expected) in [
        ("COPY compiler/Cargo.toml", "compiler/Cargo.toml"),
        ("COPY compiler/src", "compiler/src"),
        ("COPY docs/contracts", "docs/contracts/"),
        ("COPY worker/Cargo.toml", "worker/Cargo.toml"),
        ("COPY worker/src", "worker/src"),
        ("COPY Cargo.toml Cargo.lock", "Cargo.lock"),
    ] {
        let fixture: String = committed
            .lines()
            .filter(|line| !line.starts_with(without))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_ne!(fixture, committed, "{without} is absent from the file");
        let gaps = missing(&fixture, &manifest, &includes);
        assert!(
            gaps.iter().any(|gap| gap.contains(expected)),
            "{without}: {gaps:?}"
        );
    }
}

#[test]
fn include_literals_that_leave_the_build_context_are_rejected() {
    let source = Path::new("worker/src/config.rs");
    let ok = scan_source(source, r#"include_str!("../../docs/contracts/a.md")"#);
    assert_eq!(ok, [Ok("docs/contracts/a.md".to_string())]);
    for literal in [
        "../../../worker/src/main.rs",
        "/worker/src/main.rs",
        "../../../../etc/passwd",
    ] {
        for macro_name in ["include_str!", "include_bytes!"] {
            let results = scan_source(source, &format!("{macro_name}(\"{literal}\")"));
            assert!(
                matches!(results.as_slice(), [Err(_)]),
                "{macro_name} {literal}: {results:?}"
            );
        }
    }
    let results = scan_source(source, "include_str!(concat!(\"a\", \"b\"))");
    assert!(matches!(results.as_slice(), [Err(_)]));
}

#[test]
fn an_include_outside_the_copied_directories_is_rejected() {
    let mut includes = worker_includes();
    includes.push("docs/adr/0011-namespace-placement-mode.md".into());
    let gaps = missing(
        &read("worker/Containerfile"),
        &read("Cargo.toml"),
        &includes,
    );
    assert_eq!(gaps, ["docs/adr/0011-namespace-placement-mode.md"]);
}

/// The copied context must load with `--locked --offline`: the stage copies the workspace
/// manifests and member sources only, so Cargo.lock must still agree with them.
#[test]
fn copied_context_loads_with_locked_cargo_metadata() {
    let containerfile = read("worker/Containerfile");
    let copied = copy_sources(&containerfile);
    let context = std::env::temp_dir().join(format!("fs-build-context-{}", std::process::id()));
    let _ = fs::remove_dir_all(&context);
    for source in &copied {
        copy_into(&root().join(source), &context.join(source));
    }
    let output = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--locked", "--offline", "--format-version=1"])
        .arg("--manifest-path")
        .arg(context.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", context.join("target"))
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let _ = fs::remove_dir_all(&context);
    assert!(output.status.success(), "{stderr}");
}

fn copy_into(from: &Path, to: &Path) {
    if from.is_dir() {
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            copy_into(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(from, to).unwrap();
    }
}

/// Opt-in: FS_TEST_IMAGE_BUILD=1 builds the real Containerfile with rootless podman.
#[test]
fn opt_in_podman_build_of_the_worker_containerfile() {
    if std::env::var("FS_TEST_IMAGE_BUILD").as_deref() != Ok("1") {
        return;
    }
    let status = std::process::Command::new("podman")
        .args(["build", "--file"])
        .arg(root().join("worker/Containerfile"))
        .arg(root())
        .status()
        .unwrap();
    assert!(status.success());
}
