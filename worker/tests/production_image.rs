//! The production worker image must carry everything `falcone-serverless-worker` spawns. The
//! worker resolves the trusted compiler `artifact-compile-child` beside its own executable
//! (`worker/src/server/mod.rs`); an image without it answers every first execution of a valid
//! component with 500 `compile_failed`. Derived from the Containerfile, so it cannot drift.

const CONTAINERFILE: &str = include_str!("../Containerfile");

fn final_stage(containerfile: &str) -> Vec<&str> {
    let mut stage = Vec::new();
    for line in containerfile.lines() {
        if line.trim_start().to_ascii_uppercase().starts_with("FROM ") {
            stage.clear();
        }
        stage.push(line.trim());
    }
    stage
}

/// Directory the final stage copies a built binary to.
fn installed_in<'a>(stage: &[&'a str], binary: &str) -> Option<&'a str> {
    stage.iter().find_map(|line| {
        let words: Vec<_> = line.split_whitespace().collect();
        if words.first()? != &"COPY" {
            return None;
        }
        let source = words.iter().find(|word| word.starts_with("/build/"))?;
        if !source.ends_with(&format!("/release/{binary}")) {
            return None;
        }
        words.last()?.rsplit_once('/').map(|(dir, _)| dir)
    })
}

#[test]
fn the_final_image_ships_the_compiler_beside_the_worker() {
    let stage = final_stage(CONTAINERFILE);
    let worker = installed_in(&stage, "falcone-serverless-worker").expect("worker is copied");
    let compiler = installed_in(&stage, "artifact-compile-child").expect("compiler is copied");
    assert_eq!(worker, compiler);
}

#[test]
fn the_build_stage_builds_the_compiler_the_final_image_copies() {
    let build = CONTAINERFILE
        .lines()
        .find(|line| line.trim_start().starts_with("RUN cargo build"))
        .expect("a cargo build line");
    assert!(build.contains("--locked"));
    assert!(build.contains("artifact-compile-child"));
    assert!(build.contains("falcone-serverless-worker"));
}

#[test]
fn the_entrypoint_is_still_the_worker() {
    assert!(
        final_stage(CONTAINERFILE)
            .iter()
            .any(|line| line.starts_with("ENTRYPOINT")
                && line.contains("/usr/local/bin/falcone-serverless-worker"))
    );
}
