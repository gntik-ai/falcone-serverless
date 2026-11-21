//! Reproduce the worker/Containerfile source boundary without a container engine.
use std::{fs, path::Path, process::Command};
fn copy(from: &Path, to: &Path) {
    if from.is_dir() {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            copy(&path, &to.join(path.file_name().unwrap()));
        }
    } else {
        fs::copy(from, to).unwrap();
    }
}
#[test]
#[ignore = "operator-run release build of the Containerfile source context"]
fn release_worker_uses_only_image_context() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let scratch = std::env::temp_dir().join(format!("go-image-context-{}", std::process::id()));
    fs::create_dir(&scratch).unwrap();
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "compiler/Cargo.toml",
        "compiler/src",
        "worker/Cargo.toml",
        "worker/src",
    ] {
        let target = scratch.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        copy(&root.join(path), &target);
    }
    let result = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--locked",
            "--offline",
            "-p",
            "falcone-serverless-worker",
        ])
        .current_dir(&scratch)
        .status();
    fs::remove_dir_all(scratch).unwrap();
    assert!(result.unwrap().success());
}
