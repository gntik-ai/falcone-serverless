//! Offline gates for the trusted worker's image inputs and supply-chain records.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).expect(path)
}

fn channel() -> String {
    read("rust-toolchain.toml")
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "channel").then(|| value.trim().trim_matches('"').to_owned())
        })
        .expect("toolchain channel")
}

fn external_images(containerfile: &str) -> Vec<&str> {
    let mut aliases = Vec::new();
    let mut images = Vec::new();
    for line in containerfile.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        if words
            .first()
            .is_some_and(|word| word.eq_ignore_ascii_case("FROM"))
        {
            let image = words[1];
            if !aliases
                .iter()
                .any(|alias: &&str| alias.eq_ignore_ascii_case(image))
            {
                images.push(image);
            }
            if words.len() == 4 && words[2].eq_ignore_ascii_case("AS") {
                aliases.push(words[3]);
            }
        }
    }
    images
}

/// Each entry is one violation with one rule id, independent of the other rules.
fn check(containerfile: &str, channel: &str) -> Vec<&'static str> {
    let mut violations = Vec::new();
    let images = external_images(containerfile);
    if images.len() != 2 {
        violations.push("base-repo");
    }
    for (index, image) in images.iter().enumerate() {
        let (tagged, digest) = image.split_once('@').unwrap_or((image, ""));
        if !digest.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            violations.push("digest");
        }
        let (repo, tag) = tagged.rsplit_once(':').unwrap_or((tagged, ""));
        if index == 0 {
            if repo != "docker.io/library/rust" {
                violations.push("base-repo");
            }
            if tag != format!("{channel}-slim-trixie") {
                violations.push("toolchain-tag");
            }
        } else if tagged != "gcr.io/distroless/cc-debian13:nonroot" {
            violations.push("base-repo");
        }
    }

    let mut aliases: Vec<&str> = Vec::new();
    let mut locked_build = false;
    for line in containerfile.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        let Some(instruction) = words.first() else {
            continue;
        };
        if instruction.eq_ignore_ascii_case("FROM")
            && words.len() == 4
            && words[2].eq_ignore_ascii_case("AS")
        {
            aliases.push(words[3]);
        }
        if instruction.eq_ignore_ascii_case("COPY") {
            for source in words.iter().filter_map(|word| word.strip_prefix("--from=")) {
                if !aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(source))
                {
                    violations.push("copy-from");
                }
            }
        }
        if instruction.eq_ignore_ascii_case("ADD")
            && words
                .iter()
                .any(|word| word.starts_with("https://") || word.starts_with("http://"))
        {
            violations.push("add-remote");
        }
        if instruction.eq_ignore_ascii_case("RUN")
            && words.windows(2).any(|pair| pair == ["cargo", "build"])
            && words.contains(&"--locked")
        {
            locked_build = true;
        }
    }
    if !locked_build {
        violations.push("cargo-locked");
    }
    violations
}

#[test]
fn committed_image_has_pinned_toolchain_and_runtime_inputs() {
    let containerfile = read("worker/Containerfile");
    assert_eq!(check(&containerfile, &channel()), Vec::<&str>::new());
    assert!(containerfile.contains("cargo build --release --locked -p falcone-serverless-worker"));
    assert!(!containerfile.lines().any(|line| line.starts_with("ADD ")));
}

fn fixture_b() -> String {
    let committed = read("worker/Containerfile");
    let images = external_images(&committed);
    assert_eq!(images.len(), 2);
    images
        .iter()
        .enumerate()
        .fold(committed.clone(), |text, (i, image)| {
            let (tag, _) = image.split_once('@').expect("pinned image");
            text.replace(
                image,
                &format!("{tag}@sha256:{}", if i == 0 { "a" } else { "b" }.repeat(64)),
            )
        })
}

#[test]
fn checker_accepts_fixture_b_and_internal_stage_aliases() {
    let fixture = fixture_b();
    assert_eq!(check(&fixture, &channel()), Vec::<&str>::new());
    let internal = format!("{fixture}\nFROM build AS reused\nFROM reused\n");
    assert_eq!(external_images(&internal).len(), 2);
    assert_eq!(check(&internal, &channel()), Vec::<&str>::new());
}

#[test]
fn each_single_edit_reports_exactly_its_expected_rule() {
    let fixture = fixture_b();
    let builder_digest = format!("@sha256:{}", "a".repeat(64));
    for (before, after, rule) in [
        (builder_digest.clone(), String::new(), "digest"),
        ("a".repeat(64), "a".repeat(63), "digest"),
        ("a".repeat(64), "A".repeat(64), "digest"),
        (
            format!("{}-slim-trixie", channel()),
            "1.99.0-slim-trixie".into(),
            "toolchain-tag",
        ),
        (
            "gcr.io/distroless/cc-debian13:nonroot".into(),
            "gcr.io/distroless/static-debian13:nonroot".into(),
            "base-repo",
        ),
        (
            "COPY --from=build".into(),
            "COPY --from=docker.io/library/alpine:3".into(),
            "copy-from",
        ),
        ("--locked ".into(), String::new(), "cargo-locked"),
    ] {
        assert!(
            fixture.contains(&before),
            "mutation must change fixture: {rule}"
        );
        let mutated = fixture.replacen(&before, &after, 1);
        assert_eq!(check(&mutated, &channel()), [rule], "{rule}: {mutated}");
    }
    assert_eq!(
        check(
            &format!("{fixture}\nADD https://example.invalid/x /x\n"),
            &channel()
        ),
        ["add-remote"]
    );
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "worker-image-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create temp dir: {error}"),
            }
        }
    }

    fn run(&self, commit: Option<&str>, lockfile: Option<&Path>) -> Output {
        let mut command = Command::new("sh");
        command
            .arg(root().join("worker/scripts/sbom.sh"))
            .arg(self.0.join("out"))
            .current_dir(&self.0)
            .env_remove("SOURCE_COMMIT");
        if let Some(commit) = commit {
            command.env("SOURCE_COMMIT", commit);
        }
        if let Some(lockfile) = lockfile {
            command.arg(lockfile);
        }
        command.output().expect("invoke POSIX sh")
    }

    fn output(&self, name: &str) -> String {
        fs::read_to_string(self.0.join("out").join(name)).expect(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn sbom_and_provenance_are_deterministic_from_any_working_directory() {
    let temp = TempDir::new();
    let result = temp.run(Some(COMMIT), None);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let sbom = temp.output("sbom.spdx");
    for line in [
        "SPDXVersion: SPDX-2.3",
        "PackageName: falcone-serverless-worker",
        "PackageVersion: 0.1.0",
        "Created: 1970-01-01T00:00:00Z",
    ] {
        assert!(sbom.lines().any(|actual| actual == line), "missing {line}");
    }
    assert!(sbom.contains(&format!(
        "DocumentNamespace: https://falcone.invalid/spdx/falcone-serverless-worker-{COMMIT}"
    )));
    let provenance = temp.output("provenance.txt");
    assert!(
        provenance
            .lines()
            .any(|line| line == format!("source_commit={COMMIT}"))
    );
    assert!(
        provenance
            .lines()
            .any(|line| line == format!("rust_toolchain={}", channel()))
    );
    let bases: Vec<_> = provenance
        .lines()
        .filter_map(|line| line.strip_prefix("base_image="))
        .collect();
    assert_eq!(bases, external_images(&read("worker/Containerfile")));
    let result = temp.run(Some(COMMIT), None);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(temp.output("sbom.spdx"), sbom);
    assert_eq!(temp.output("provenance.txt"), provenance);
}

#[test]
fn sbom_inventories_packages_with_and_without_registry_checksums() {
    let temp = TempDir::new();
    let lockfile = temp.0.join("fixture.lock");
    fs::write(&lockfile, "version = 4\n\n[[package]]\nname = \"local-component\"\nversion = \"2.3.4\"\n\n[[package]]\nname = \"registry-component\"\nversion = \"5.6.7\"\nchecksum = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n").unwrap();
    let result = temp.run(Some(COMMIT), Some(&lockfile));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let sbom = temp.output("sbom.spdx");
    let names: Vec<_> = sbom
        .lines()
        .filter_map(|line| line.strip_prefix("PackageName: "))
        .collect();
    assert_eq!(names, ["local-component", "registry-component"]);
    let checksums: Vec<_> = sbom
        .lines()
        .filter(|line| line.starts_with("PackageChecksum:"))
        .collect();
    assert_eq!(
        checksums,
        [
            "PackageChecksum: SHA256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ]
    );
}

#[test]
fn invalid_source_commit_is_rejected_before_creating_any_output() {
    for commit in [
        None,
        Some("abc"),
        Some("0123456789ABCDEF0123456789abcdef01234567"),
    ] {
        let temp = TempDir::new();
        let result = temp.run(commit, None);
        assert!(
            !result.status.success(),
            "accepted invalid SOURCE_COMMIT {commit:?}"
        );
        assert!(!temp.0.join("out/sbom.spdx").exists());
        assert!(
            !temp.0.join("out").exists(),
            "validation must precede all writes"
        );
    }
}

#[test]
fn unreadable_lockfile_publishes_no_sbom_or_provenance() {
    let temp = TempDir::new();
    let result = temp.run(Some(COMMIT), Some(&temp.0.join("missing.lock")));
    assert!(!result.status.success());
    assert!(!temp.0.join("out/sbom.spdx").exists());
    assert!(!temp.0.join("out/provenance.txt").exists());
    assert_eq!(fs::read_dir(temp.0.join("out")).unwrap().count(), 0);
}

#[test]
fn documentation_records_image_inputs_and_supply_chain_limits() {
    let doc = read("docs/supply-chain/trusted-images.md");
    for heading in [
        "## Digest pinning",
        "## SBOM",
        "## Provenance",
        "## Signing",
    ] {
        assert!(doc.lines().any(|line| line == heading), "missing {heading}");
    }
    for token in [
        "worker/Containerfile",
        "worker/scripts/sbom.sh",
        "Cargo.lock",
        "OS packages",
        "1970-01-01T00:00:00Z",
        "neither signs nor verifies",
    ] {
        assert!(doc.contains(token), "missing {token}");
    }
    for image in external_images(&read("worker/Containerfile")) {
        assert!(doc.contains(image), "missing {image}");
    }
}
