use std::path::Path;

#[test]
fn descriptor_module_is_the_only_audited_exception() {
    fn check(root: &Path, audited: &Path) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                let name = path.file_name().unwrap().to_string_lossy();
                if !name.starts_with('.') && name != "target" && name != "node_modules" {
                    check(&path, audited);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") && path != audited {
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
    let audited = root.join("compiler/src/isolate/fd_hygiene.rs");
    check(root, &audited);
}
