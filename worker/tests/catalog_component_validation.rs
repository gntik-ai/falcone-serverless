#[allow(dead_code)]
#[path = "../src/artifact/mod.rs"]
mod artifact;
#[path = "../src/build/mod.rs"]
mod build;

#[test]
fn accepts_valid_component() {
    let bytes = include_bytes!("fixtures/catalog/valid_no_imports.wasm");
    let result = build::validate_catalog_component(bytes, bytes.len(), |_| {});
    let build::Verdict::Accepted(accepted) = result else {
        panic!("valid component rejected: {result:?}");
    };
    assert_eq!(
        accepted.digest,
        "sha256:5389acc3864c7951ea6d7d9b976086092bb3d23b639e6392bc297051d8a7e9c9"
    );
    assert_eq!(accepted.imports, Vec::<String>::new());
}

#[test]
fn rejects_forbidden_imports() {
    for (bytes, import) in [
        (
            include_bytes!("fixtures/catalog/import_wasi_filesystem.wasm").as_slice(),
            "wasi:filesystem/types@0.2.0",
        ),
        (
            include_bytes!("fixtures/catalog/import_wasi_sockets.wasm").as_slice(),
            "wasi:sockets/tcp@0.2.0",
        ),
        (
            include_bytes!("fixtures/catalog/import_unknown.wasm").as_slice(),
            "example:unknown/iface@1.0.0",
        ),
    ] {
        let rejected = rejection(bytes, bytes.len(), "forbidden_import");
        assert!(rejected.detail.contains(import), "{rejected:?}");
    }
}

fn rejection(bytes: &[u8], limit: usize, reason: &str) -> build::Rejected {
    let result = build::validate_catalog_component(bytes, limit, |_| {
        panic!("rejected input reached handoff");
    });
    let build::Verdict::Rejected(rejected) = result else {
        panic!("invalid input accepted: {result:?}");
    };
    assert_eq!(rejected.reason, reason);
    rejected
}

#[test]
fn rejects_core_module() {
    let bytes = include_bytes!("fixtures/catalog/core_module.wasm");
    rejection(bytes, bytes.len(), "not_a_component");
    // An invalid core body still has the core preamble, per the catalog mapping.
    let mut invalid_core = bytes.to_vec();
    invalid_core.push(0xff);
    rejection(&invalid_core, invalid_core.len(), "not_a_component");
    // Size errors take the fallback mapping even with a core preamble.
    rejection(bytes, bytes.len() - 1, "malformed");
}

fn native_binary() -> [u8; 64] {
    let mut bytes = [0; 64];
    bytes[..7].copy_from_slice(&[0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01, 0x01]);
    bytes
}

#[test]
fn rejects_native_binary() {
    let bytes = native_binary();
    rejection(&bytes, bytes.len(), "native_binary");
    // ELF classification precedes even the validator's size check.
    rejection(&bytes, 0, "native_binary");
    rejection(&bytes[..4], 4, "native_binary");
}

fn malformed_inputs() -> Vec<(Vec<u8>, usize)> {
    let valid = include_bytes!("fixtures/catalog/valid_no_imports.wasm");
    let mut bad_magic = valid.to_vec();
    bad_magic[..4].fill(0);
    let invalid_body = include_bytes!("fixtures/catalog/invalid_body.wasm");
    vec![
        (vec![], 0),
        (valid[..4].to_vec(), valid.len()),
        (valid[..valid.len() - 1].to_vec(), valid.len()),
        (bad_magic, valid.len()),
        (invalid_body.to_vec(), invalid_body.len()),
        (valid.to_vec(), valid.len() - 1),
        (b"other precompiled input".to_vec(), 1024),
    ]
}

#[test]
fn rejects_malformed_inputs() {
    assert!(include_bytes!("fixtures/catalog/valid_no_imports.wasm").len() > 8);
    for (bytes, limit) in malformed_inputs() {
        rejection(&bytes, limit, "malformed");
    }
}

#[test]
fn no_handoff_for_rejected_input() {
    let mut inputs = malformed_inputs();
    for bytes in [
        include_bytes!("fixtures/catalog/import_wasi_filesystem.wasm").as_slice(),
        include_bytes!("fixtures/catalog/import_wasi_sockets.wasm").as_slice(),
        include_bytes!("fixtures/catalog/import_unknown.wasm").as_slice(),
        include_bytes!("fixtures/catalog/core_module.wasm").as_slice(),
        native_binary().as_slice(),
    ] {
        inputs.push((bytes.to_vec(), bytes.len()));
    }
    let mut calls = Vec::new();
    for (bytes, limit) in inputs {
        let result = build::validate_catalog_component(&bytes, limit, |input| {
            calls.push(input.to_vec());
        });
        assert!(matches!(result, build::Verdict::Rejected(_)), "{result:?}");
        assert!(calls.is_empty());
    }
    let valid = include_bytes!("fixtures/catalog/valid_no_imports.wasm");
    let result = build::validate_catalog_component(valid, valid.len(), |input| {
        calls.push(input.to_vec());
    });
    assert!(matches!(result, build::Verdict::Accepted(_)), "{result:?}");
    assert_eq!(calls, vec![valid.to_vec()]);
}

#[test]
fn no_deserialize_in_build_module() {
    let mut directories = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/build")];
    let mut checked = 0;
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                directories.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                for forbidden in [
                    "Module::deserialize",
                    "Component::deserialize",
                    "deserialize_file",
                    "deserialize_raw",
                    "deserialize_open_file",
                    "precompile_module",
                    "precompile_component",
                ] {
                    assert!(
                        !source.contains(forbidden),
                        "{}: {forbidden}",
                        path.display()
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
}

#[test]
fn fixtures_match_wat_sources() {
    for (source, bytes) in [
        (
            include_str!("fixtures/catalog/valid_no_imports.wat"),
            include_bytes!("fixtures/catalog/valid_no_imports.wasm").as_slice(),
        ),
        (
            include_str!("fixtures/catalog/import_wasi_filesystem.wat"),
            include_bytes!("fixtures/catalog/import_wasi_filesystem.wasm").as_slice(),
        ),
        (
            include_str!("fixtures/catalog/import_wasi_sockets.wat"),
            include_bytes!("fixtures/catalog/import_wasi_sockets.wasm").as_slice(),
        ),
        (
            include_str!("fixtures/catalog/import_unknown.wat"),
            include_bytes!("fixtures/catalog/import_unknown.wasm").as_slice(),
        ),
        (
            include_str!("fixtures/catalog/core_module.wat"),
            include_bytes!("fixtures/catalog/core_module.wasm").as_slice(),
        ),
        (
            include_str!("fixtures/catalog/invalid_body.wat"),
            include_bytes!("fixtures/catalog/invalid_body.wasm").as_slice(),
        ),
    ] {
        assert_eq!(bytes, wat::parse_str(source).unwrap(), "{source}");
    }
    assert!(
        wasmparser::Validator::new()
            .validate_all(include_bytes!("fixtures/catalog/core_module.wasm"))
            .is_ok()
    );
    assert!(
        wasmparser::Validator::new()
            .validate_all(include_bytes!("fixtures/catalog/invalid_body.wasm"))
            .is_err()
    );
}
