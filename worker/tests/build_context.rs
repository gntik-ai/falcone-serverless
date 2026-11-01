//! Check the actual image COPY context without resolving any third-party crates.

use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const CONTAINERFILE: &str = include_str!("../Containerfile");

struct Context(PathBuf);

impl Context {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce =
            RandomState::new().hash_one((std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let path = std::env::temp_dir().join(format!("fs-test-context-{nonce:016x}"));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn copy(source: &Path, destination: &Path) {
    if source.is_dir() {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            copy(&entry.path(), &destination.join(entry.file_name()));
        }
    } else {
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(source, destination).unwrap();
    }
}

fn check_context(containerfile: &str) -> Result<(), String> {
    let context = Context::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut stages = 0;
    for line in containerfile.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        match words.first().copied() {
            Some("FROM") => stages += 1,
            Some("COPY") if stages == 1 => {
                let (destination, sources) = words[1..].split_last().unwrap();
                assert!(!sources.iter().any(|source| source.starts_with("--")));
                for source in sources {
                    let destination = if destination.ends_with('/') {
                        context.0.join(destination).join(source)
                    } else {
                        context.0.join(destination)
                    };
                    copy(&root.join(source), &destination);
                }
            }
            _ => {}
        }
    }
    // An empty registry proves this checks workspace completeness, not dependency caches
    // (including crates for platforms other than the Linux image build).
    let cargo_home = context.0.join("empty-cargo-home");
    fs::create_dir(&cargo_home).unwrap();
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .current_dir(&context.0)
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap();
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(())
}

#[test]
fn copied_context_loads_with_locked_cargo_metadata() {
    check_context(CONTAINERFILE).unwrap();
}

#[test]
fn omitted_compiler_copy_names_the_missing_member() {
    let fixture = CONTAINERFILE
        .lines()
        .filter(|line| !line.starts_with("COPY compiler/"))
        .collect::<Vec<_>>()
        .join("\n");
    let error = check_context(&fixture).unwrap_err();
    assert!(error.contains("compiler/Cargo.toml"), "{error}");
}
