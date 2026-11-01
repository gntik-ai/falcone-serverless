//! Offline proof that the Containerfile build stage copies everything cargo needs to load
//! the workspace. The copy set is derived from the Containerfile COPY lines, so the test
//! cannot drift from the file. `--no-deps` keeps the check independent of the network and of
//! crates outside what a Linux build fetches (for example macOS-only dependencies).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const CONTAINERFILE: &str = include_str!("../Containerfile");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .unwrap()
}

struct Context(PathBuf);

impl Drop for Context {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Source and destination pairs of the build stage's COPY lines (destinations are relative
/// to the stage WORKDIR, which is the root of the copied context).
fn copy_set(containerfile: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for line in containerfile.lines().take_while(|line| {
        !line
            .trim_start()
            .to_ascii_uppercase()
            .starts_with("FROM GCR")
    }) {
        let mut words = line.split_whitespace();
        if !words.next().is_some_and(|w| w.eq_ignore_ascii_case("COPY")) {
            continue;
        }
        let args: Vec<_> = words.collect();
        if args.iter().any(|arg| arg.starts_with("--")) {
            continue;
        }
        let (dest, sources) = args.split_last().expect("COPY needs a destination");
        for source in sources {
            let target = if *dest == "./" || dest.ends_with('/') {
                let name = Path::new(source).file_name().unwrap();
                Path::new(dest).join(name)
            } else {
                PathBuf::from(dest)
            };
            let target = target.strip_prefix(".").unwrap_or(&target);
            pairs.push(((*source).into(), target.to_string_lossy().into_owned()));
        }
    }
    pairs
}

fn copy_all(from: &Path, to: &Path) {
    if from.is_dir() {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            copy_all(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(from, to).unwrap();
    }
}

fn build_context(containerfile: &str) -> Context {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "fs-build-context-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let context = Context(dir);
    let root = repo_root();
    let pairs = copy_set(containerfile);
    assert!(!pairs.is_empty(), "no COPY lines in the build stage");
    for (source, target) in pairs {
        copy_all(&root.join(&source), &context.0.join(target));
    }
    context
}

fn metadata_loads(context: &Context) -> bool {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--locked", "--offline", "--no-deps"])
        .args(["--format-version", "1"])
        .current_dir(&context.0)
        .env_remove("CARGO_TARGET_DIR")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn copy_set_is_read_from_the_build_stage_copy_lines() {
    let pairs = copy_set(CONTAINERFILE);
    assert!(pairs.contains(&("Cargo.toml".into(), "Cargo.toml".into())));
    assert!(pairs.contains(&("worker/src".into(), "worker/src".into())));
    assert!(
        !pairs.iter().any(|(source, _)| source.contains("target/")),
        "the runtime stage COPY --from must not be part of the context"
    );
}

#[test]
fn copied_context_loads_with_locked_cargo_metadata() {
    let context = build_context(CONTAINERFILE);
    assert!(metadata_loads(&context));
}

#[test]
fn context_without_a_workspace_member_manifest_fails_to_load() {
    let context = build_context(CONTAINERFILE);
    fs::remove_file(context.0.join("compiler/Cargo.toml")).unwrap();
    assert!(!metadata_loads(&context));
}

#[test]
fn context_missing_a_member_in_the_containerfile_fails_to_load() {
    let without_compiler: String = CONTAINERFILE
        .lines()
        .filter(|line| !line.contains("compiler/"))
        .map(|line| format!("{line}\n"))
        .collect();
    let context = build_context(&without_compiler);
    assert!(!context.0.join("compiler").exists());
    assert!(!metadata_loads(&context));
}
