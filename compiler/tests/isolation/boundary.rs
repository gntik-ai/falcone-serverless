use std::path::Path;

fn audited(path: &Path) -> bool {
    path == Path::new("compiler/src/isolate/fd_hygiene.rs")
        || path == Path::new("worker/src/cache/load.rs")
}

#[test]
fn audited_exceptions_are_exact_paths() {
    assert!(audited(Path::new("compiler/src/isolate/fd_hygiene.rs")));
    assert!(audited(Path::new("worker/src/cache/load.rs")));
    for path in [
        "worker/src/cache/sibling.rs",
        "worker/src/cache/nested/load.rs",
        "worker/src/artifact/load.rs",
        "compiler/src/isolate/load.rs",
    ] {
        assert!(!audited(Path::new(path)), "{path}");
    }
}

#[test]
fn only_the_two_audited_boundaries_are_exceptions() {
    fn check(root: &Path, repository: &Path) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                let name = path.file_name().unwrap().to_string_lossy();
                if !name.starts_with('.') && name != "target" && name != "node_modules" {
                    check(&path, repository);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && !audited(path.strip_prefix(repository).unwrap())
            {
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(
                    !text.contains(&["un", "safe"].concat()),
                    "unreviewed boundary: {}",
                    path.display()
                );
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    check(root, root);
}
