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

// ---------------------------------------------------------------------------------------------
// The worker image builds `cargo build --release --locked -p falcone-serverless-worker` from the
// context above and nothing else. Embedding a file outside worker/src broke that build once
// (PR #432: worker/qualification/go/versions.toml), so the build is run for real, and a mutation
// shows the same build fails when an embedded file is outside the copied context.

fn shared_target() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target"))
        .join("image-context")
}

fn release_build(context: &Context) -> std::process::Output {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["build", "--release", "--locked", "--offline"])
        .args(["-p", "falcone-serverless-worker"])
        .current_dir(&context.0)
        .env("CARGO_TARGET_DIR", shared_target())
        .output()
        .unwrap()
}

#[test]
fn the_worker_builds_from_only_the_containerfile_inputs() {
    let context = build_context(CONTAINERFILE);
    // Exactly what the image has: workspace manifests, the compiler crate and worker/src.
    assert!(context.0.join("worker/src").is_dir());
    assert!(!context.0.join("worker/qualification").exists());
    assert!(!context.0.join("docs").exists());
    let output = release_build(&context);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        shared_target()
            .join("release/falcone-serverless-worker")
            .is_file()
    );
}

#[test]
fn a_file_embedded_from_outside_worker_src_fails_the_image_context_build() {
    let context = build_context(CONTAINERFILE);
    // The shape of the PR #432 defect: non-test code embeds a file that exists in the repository
    // but is not copied into the image build context.
    let source = context.0.join("worker/src/adapters/compat_go.rs");
    let text = fs::read_to_string(&source).unwrap();
    let embedded = "include_str!(\"../qualification/go_versions.toml\")";
    assert!(
        text.contains(embedded),
        "the pins must be embedded from worker/src"
    );
    fs::write(
        &source,
        text.replace(
            embedded,
            "include_str!(\"../../qualification/go/build-profile.sh\")",
        ),
    )
    .unwrap();
    assert!(
        repo_root()
            .join("worker/qualification/go/build-profile.sh")
            .is_file()
    );
    let output = release_build(&context);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("couldn't read"), "{stderr}");
}

/// Path arguments of `include_str!`, `include_bytes!` and `include!` in non-test code that
/// resolve outside `src`. Everything from the first `#[cfg(test)]` line on is test code, which
/// the image never builds.
fn escaping_includes(file: &Path, text: &str, src: &Path) -> Vec<String> {
    let non_test = text.split("#[cfg(test)]").next().unwrap_or_default();
    let mut found = Vec::new();
    for marker in ["include_str!(", "include_bytes!(", "include!("] {
        for (at, _) in non_test.match_indices(marker) {
            let rest = &non_test[at + marker.len()..];
            let Some(open) = rest.find('"') else { continue };
            let Some(length) = rest[open + 1..].find('"') else {
                continue;
            };
            let target = &rest[open + 1..open + 1 + length];
            let mut resolved = file.parent().unwrap().to_path_buf();
            for part in target.split('/') {
                match part {
                    ".." => {
                        resolved.pop();
                    }
                    "." | "" => {}
                    other => resolved.push(other),
                }
            }
            if !resolved.starts_with(src) {
                found.push(format!("{}: {target}", file.display()));
            }
        }
    }
    found
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}

/// Files that a `#[cfg(test)] mod name;` declaration compiles into test builds only.
fn test_only_modules(file: &Path, text: &str) -> Vec<PathBuf> {
    let dir = file.parent().unwrap();
    let mut out = Vec::new();
    let mut lines = text.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if line != "#[cfg(test)]" {
            continue;
        }
        let declaration = lines.next().unwrap_or_default();
        let declaration = declaration.strip_prefix("pub ").unwrap_or(declaration);
        if let Some(name) = declaration
            .strip_prefix("mod ")
            .and_then(|rest| rest.strip_suffix(';'))
        {
            out.push(dir.join(format!("{name}.rs")));
            out.push(dir.join(name));
        }
    }
    out
}

#[test]
fn non_test_worker_code_embeds_only_files_under_worker_src() {
    let src = repo_root().join("worker/src");
    let files = rust_files(&src);
    let test_only: Vec<PathBuf> = files
        .iter()
        .flat_map(|file| test_only_modules(file, &fs::read_to_string(file).unwrap()))
        .collect();
    assert!(
        test_only
            .iter()
            .any(|path| path.ends_with("runtime_metrics.rs")),
        "the test-only module declarations must be found"
    );
    let mut escaping = Vec::new();
    for file in files {
        if test_only.iter().any(|only| file.starts_with(only)) {
            continue;
        }
        escaping.extend(escaping_includes(
            &file,
            &fs::read_to_string(&file).unwrap(),
            &src,
        ));
    }
    assert_eq!(escaping, Vec::<String>::new());
    // The scan itself bites: an outside path in non-test code is found, a test-only one is not.
    let probe = src.join("adapters/probe.rs");
    let outside = "const X: &str = include_str!(\"../../qualification/go/versions.toml\");";
    assert_eq!(escaping_includes(&probe, outside, &src).len(), 1);
    let inside = "const X: &str = include_str!(\"../qualification/go_versions.toml\");";
    assert!(escaping_includes(&probe, inside, &src).is_empty());
    let test_only = format!("#[cfg(test)]\nmod tests {{ {outside} }}");
    assert!(escaping_includes(&probe, &test_only, &src).is_empty());
}
