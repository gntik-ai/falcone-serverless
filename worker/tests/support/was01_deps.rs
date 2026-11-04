//! The WAS-01 evidence task adds no dependency: `worker/Cargo.toml` keeps exactly the entries
//! of its dependency tables, and `Cargo.lock` exactly the packages, recorded in
//! `was01_dependency_baseline.txt` when the task started. The baseline is a snapshot, so any
//! later dependency change fails the guard until the baseline is refreshed.

use std::collections::BTreeSet;

pub const BASELINE: &str = include_str!("was01_dependency_baseline.txt");
const TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// How to refresh the baseline; the guard failure says so.
pub const REFRESH: &str = "this guard compares against worker/tests/support/was01_dependency_baseline.txt; \
if a dependency change is intended and reviewed, refresh that file with: \
cargo test -p falcone-serverless-worker --locked --test was01_cluster -- --ignored --nocapture print_dependency_baseline \
and keep only its lines that start with `manifest ` or `package `";

fn document(text: &str) -> Result<toml::Value, String> {
    toml::from_str(text).map_err(|_| "cannot read a manifest or lockfile as TOML".to_owned())
}

/// `manifest <table> <name>` for every entry of every dependency table, including `target.*` tables.
fn manifest_lines(text: &str) -> Result<BTreeSet<String>, String> {
    let manifest = document(text)?;
    let mut lines = BTreeSet::new();
    let mut collect = |scope: &str, table: &toml::Value| {
        for name in TABLES {
            if let Some(entries) = table.get(name).and_then(toml::Value::as_table) {
                for entry in entries.keys() {
                    lines.insert(format!("manifest {scope}{name} {entry}"));
                }
            }
        }
    };
    collect("", &manifest);
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (cfg, table) in targets {
            collect(&format!("target.{cfg}."), table);
        }
    }
    Ok(lines)
}

/// `package <name> <version>` for every `[[package]]`; repeated versions of a name stay distinct.
fn lock_lines(text: &str) -> Result<BTreeSet<String>, String> {
    let lock = document(text)?;
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("the lockfile has no packages")?;
    packages
        .iter()
        .map(|package| {
            let field = |key: &str| package.get(key).and_then(toml::Value::as_str);
            match (field("name"), field("version")) {
                (Some(name), Some(version)) => Ok(format!("package {name} {version}")),
                _ => Err("a lockfile package has no name or version".to_owned()),
            }
        })
        .collect()
}

fn recorded(baseline: &str) -> BTreeSet<String> {
    baseline
        .lines()
        .filter(|line| line.starts_with("manifest ") || line.starts_with("package "))
        .map(str::to_owned)
        .collect()
}

/// The baseline text for the given manifest and lockfile.
pub fn render(manifest: &str, lock: &str) -> Result<String, String> {
    let mut lines = vec![
        "# WAS-01 dependency baseline: entries of worker/Cargo.toml dependency tables and packages of Cargo.lock.".to_owned(),
        "# Refresh only for a reviewed dependency change; see the failure message of the guard.".to_owned(),
    ];
    lines.extend(manifest_lines(manifest)?);
    lines.extend(lock_lines(lock)?);
    Ok(lines.join("\n") + "\n")
}

/// Fails if the manifest's dependency entries or the lockfile's packages differ from `baseline`
/// in either direction.
pub fn check(baseline: &str, manifest: &str, lock: &str) -> Result<(), String> {
    let wanted = recorded(baseline);
    let mut actual = manifest_lines(manifest)?;
    actual.extend(lock_lines(lock)?);
    let added: Vec<_> = actual.difference(&wanted).take(10).cloned().collect();
    let removed: Vec<_> = wanted.difference(&actual).take(10).cloned().collect();
    if added.is_empty() && removed.is_empty() {
        return Ok(());
    }
    Err(format!(
        "dependencies differ from the WAS-01 baseline; added: [{}]; removed: [{}]; {REFRESH}",
        added.join("; "),
        removed.join("; ")
    ))
}
