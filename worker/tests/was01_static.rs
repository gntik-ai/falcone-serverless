//! WAS-01 qualification inputs; validates source only, never executes a guest.
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use toml::Value;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

fn document(path: &str) -> Value {
    toml::from_str(&fs::read_to_string(root().join(path)).unwrap()).unwrap()
}

fn inventory_valid(root: &Path, path: &str, digest: &str) -> bool {
    let path = root.join(path);
    path.is_file()
        && fs::read(path).is_ok_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == digest)
}

#[test]
fn pinned_versions_and_inventory() {
    let versions = document("worker/qualification/was-01/versions.toml");
    let workspace = document("Cargo.toml");
    let dependency = &workspace["workspace"]["dependencies"]["wasmtime"];
    let requirement = dependency["version"].as_str().unwrap();
    assert!(requirement.starts_with('='));
    assert_eq!(dependency["default-features"].as_bool(), Some(false));
    assert_eq!(
        dependency["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["cranelift", "runtime", "std", "wat", "component-model"]
    );
    assert_eq!(
        document("worker/Cargo.toml")["dependencies"]["wasmtime"]["workspace"].as_bool(),
        Some(true)
    );
    let lock = document("Cargo.lock");
    let packages: Vec<_> = lock["package"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"].as_str() == Some("wasmtime"))
        .collect();
    assert_eq!(packages.len(), 1);
    assert_eq!(
        packages[0]["version"].as_str(),
        requirement.strip_prefix('=')
    );
    assert_eq!(
        versions["wasmtime_version"].as_str(),
        requirement.strip_prefix('=')
    );
    // The qualification inventory records the exact engine build, so a feature change is visible there.
    assert_eq!(versions["wasmtime_default_features"].as_bool(), Some(false));
    assert_eq!(versions["wasmtime_features"], dependency["features"]);
    for (key, expected) in [
        ("compilation_backend", "cranelift"),
        ("wasi_interface", "none"),
        ("qualification_status", "unqualified"),
        ("target_arch", std::env::consts::ARCH),
        ("target_os", std::env::consts::OS),
        ("kubernetes_version", "unverified"),
        ("openshift_version", "unverified"),
        ("namespaced_permissions", "unverified"),
    ] {
        assert_eq!(versions[key].as_str(), Some(expected), "{key}");
    }
    assert_eq!(
        versions["rust_toolchain"],
        document("rust-toolchain.toml")["toolchain"]["channel"]
    );
    let path = versions["contract_inventory_path"].as_str().unwrap();
    let digest = versions["contract_inventory_sha256"].as_str().unwrap();
    assert!(inventory_valid(root(), path, digest));
    // The v1 inventory digest is retired: the pin follows the v1.3 inventory (RUN-01.t1).
    let v1_digest = "c9c4921595cfe5e0662e082d1af860453e4c40e49e473f4baa213fa11f871973";
    assert_ne!(digest, v1_digest);
    assert!(!inventory_valid(root(), path, v1_digest));
    let v1_2_digest = "764c449c4e326345d03e4d3bac258c1d40d7613b59b2fd433ed4276cbed1ed6b";
    assert_ne!(digest, v1_2_digest);
    assert!(!inventory_valid(root(), path, v1_2_digest));
    assert!(!inventory_valid(root(), path, &"0".repeat(64)));
    assert!(!inventory_valid(root(), "missing-was01-inventory", digest));
    assert!(!inventory_valid(root(), "docs/contracts", digest));
    assert!(!inventory_valid(root(), path, &digest.to_uppercase()));
}

fn fixture_names_valid(names: &[String]) -> bool {
    let mut names = names.to_vec();
    names.sort();
    names
        == [
            "capability-checkpoint.wat",
            "capability-context.wat",
            "capability-events.wat",
            "capability-handles.wat",
            "capability-http.wat",
            "capability-import.wat",
            "capability-logging.wat",
            "capability-storage.wat",
            "capability-table.wat",
            "capability-workflows.wat",
            "infinite_loop.wat",
            "valid.wat",
        ]
}

fn fixture_valid(name: &str, source: &str) -> bool {
    // Module::new uses the locked wat parser and wasmparser validator. No instance
    // is created and no guest executes. Exact source tokens also freeze the bodies.
    let engine = wasmtime::Engine::default();
    let Ok(module) = wasmtime::Module::new(&engine, source) else {
        return false;
    };
    let expected = match name {
        "valid.wat" => "(module (func (export \"run\") (result i32) i32.const 42))",
        "infinite_loop.wat" => "(module (func (export \"run\") (loop br 0)))",
        _ => return false,
    };
    let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    module.imports().len() == 0 && compact(source) == compact(expected)
}

#[test]
fn embedded_fixtures_are_exact_and_import_free() {
    let dir = root().join("worker/src/qualification/fixtures");
    let mut names = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assert!(fixture_names_valid(&names));
    for required in names.iter() {
        let missing = names
            .iter()
            .filter(|name| *name != required)
            .cloned()
            .collect::<Vec<_>>();
        assert!(!fixture_names_valid(&missing), "missing {required}");
    }
    names.push("extra.wat".into());
    assert!(!fixture_names_valid(&names));
    for name in ["valid.wat", "infinite_loop.wat"] {
        let source = fs::read_to_string(dir.join(name)).unwrap();
        assert!(fixture_valid(name, &source), "{name}");
        let imported = source.replacen("(module", "(module (import \"host\" \"call\" (func))", 1);
        assert!(!fixture_valid(name, &imported));
        assert!(!fixture_valid(
            name,
            &source.replace("42", "41").replace("br 0", "br 1")
        ));
    }
}

/// The keyword for unchecked code, spelled indirectly so this file stays free of it.
const UNSAFE: &str = concat!("un", "safe");

/// Splits Rust source into tokens: comments and whitespace vanish, so a comment between tokens
/// cannot hide a path, and each string or character literal becomes one `"` token, so its text
/// is never mistaken for code. Identifiers keep their text (a raw `r#name` keeps `name`), `::`
/// is one token and any other character is its own token.
fn tokens(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == '*' {
            let mut depth = 1;
            i += 2;
            while i < chars.len() && depth > 0 {
                if chars[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if c == '"' || c == '\'' && is_char_literal(&chars, i) {
            i = skip_quoted(&chars, i);
            out.push("\"".into());
        } else if c == '\'' {
            i += 1; // a lifetime tick
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while at(i).is_alphanumeric() || at(i) == '_' {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let hashes = (i..chars.len()).take_while(|&k| chars[k] == '#').count();
            let raw_prefix = matches!(word.as_str(), "r" | "br" | "cr");
            let plain_prefix = matches!(word.as_str(), "b" | "c");
            if raw_prefix && at(i + hashes) == '"' {
                i = skip_raw(&chars, i + hashes, hashes);
                out.push("\"".into());
            } else if plain_prefix && at(i) == '"' || word == "b" && at(i) == '\'' {
                i = skip_quoted(&chars, i);
                out.push("\"".into());
            } else if word == "r" && hashes == 1 && (at(i + 1).is_alphabetic() || at(i + 1) == '_')
            {
                i += 1;
                let start = i;
                while at(i).is_alphanumeric() || at(i) == '_' {
                    i += 1;
                }
                out.push(chars[start..i].iter().collect());
            } else {
                out.push(word);
            }
        } else if c == ':' && at(i + 1) == ':' {
            out.push("::".into());
            i += 2;
        } else {
            out.push(c.to_string());
            i += 1;
        }
    }
    out
}

/// A `'` starts a character literal, not a lifetime, when it closes within the next characters.
fn is_char_literal(chars: &[char], i: usize) -> bool {
    let at = |k: usize| chars.get(k).copied().unwrap_or('\0');
    at(i + 1) == '\\' || at(i + 2) == '\''
}

/// Skips a `"` or `'` literal that starts at `i` (escapes honoured); returns the index after it.
fn skip_quoted(chars: &[char], i: usize) -> usize {
    let quote = chars[i];
    let mut k = i + 1;
    while k < chars.len() && chars[k] != quote {
        k += if chars[k] == '\\' { 2 } else { 1 };
    }
    k + 1
}

/// Skips a raw string whose opening quote is at `quote`, closed by `"` and `hashes` hashes.
fn skip_raw(chars: &[char], quote: usize, hashes: usize) -> usize {
    let mut k = quote + 1;
    while k < chars.len() {
        if chars[k] == '"' && (1..=hashes).all(|n| chars.get(k + n) == Some(&'#')) {
            return k + 1 + hashes;
        }
        k += 1;
    }
    k
}

/// Wasmtime artifact loading, judged on tokens so comments and spacing cannot hide it:
/// `Module`/`Component` `::` `deserialize*`, the loaders `deserialize_file`,
/// `deserialize_open_file` and `deserialize_raw`, any mention of the engine crate, or the
/// keyword for unchecked code, which loading needs.
fn loads_precompiled_code(source: &str) -> bool {
    let tokens = tokens(source);
    let loader = tokens.windows(3).any(|w| {
        matches!(w[0].as_str(), "Module" | "Component")
            && w[1] == "::"
            && w[2].starts_with("deserialize")
    });
    loader
        || tokens.iter().any(|token| {
            matches!(
                token.as_str(),
                "deserialize_file"
                    | "deserialize_open_file"
                    | "deserialize_raw"
                    | "wasmtime"
                    | UNSAFE
            )
        })
}

/// The no-deserialize rule: a tenant-supplied precompiled artifact is never deserialized, so
/// no worker source may mention `deserialize` except `cache/load.rs`, the one place that loads
/// trusted compiler output. `api/` only parses JSON requests with serde (`Deserialize`, a serde
/// `fn deserialize`, `deserialize_with`), so it may use that word, but it still fails on every
/// Wasmtime loading API, on any path into the engine crate and on the keyword for unchecked
/// code, which loading needs, judged on tokens. Every other file, `api/` included, keeps the
/// full ban on engine artifact loading.
fn deserialize_valid(path: &Path, source: &str) -> bool {
    if path == Path::new("cache/load.rs") {
        return true;
    }
    if path.starts_with("api") && path.components().count() > 1 {
        return !loads_precompiled_code(source);
    }
    !source.contains("deserialize")
}

fn source_valid(path: &Path, source: &str) -> bool {
    let engine = path == Path::new("qualification/engine.rs");
    let bin = path == Path::new("bin/qualification_runner.rs");
    let outcome = path == Path::new("qualification/outcome.rs");
    let runner = path == Path::new("qualification/runner.rs");
    let capability = [
        "host/mod.rs",
        "host/net.rs",
        "host/runtime.rs",
        "host/execute.rs",
        "main.rs",
        "qualification/execute_cases.rs",
        "qualification/capability_http.rs",
        "qualification/capability_invocations.rs",
    ]
    .iter()
    .any(|allowed| path == Path::new(allowed));
    deserialize_valid(path, source)
        && !source.contains("tests/")
        && (!source.contains("wasmtime::") || engine || bin || capability)
        && (!source.contains("Status::Passed") || outcome)
        && (!(runner || bin) || (!source.contains("env::args") && !source.contains("Engine::new")))
}

fn scan_sources(dir: &Path, prefix: &Path) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan_sources(&path, prefix);
        } else {
            let bytes = fs::read(&path).unwrap();
            assert!(
                source_valid(
                    path.strip_prefix(prefix).unwrap(),
                    &String::from_utf8_lossy(&bytes)
                ),
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn qualification_sources_obey_boundaries() {
    let src = root().join("worker/src");
    scan_sources(&src, &src);
    let engine = fs::read_to_string(src.join("qualification/engine.rs")).unwrap();
    assert!(engine.contains(".increment_epoch()"));
    assert!(engine.contains("Module::new("));
    assert!(engine.contains("include_str!(\"fixtures/valid.wat\")"));
    assert!(engine.contains("include_str!(\"fixtures/infinite_loop.wat\")"));
    let bin = fs::read_to_string(src.join("bin/qualification_runner.rs")).unwrap();
    assert!(bin.contains("run_checks("));
    for (path, source) in [
        ("qualification/engine.rs", "deserialize"),
        ("config.rs", "wasmtime::Engine"),
        ("host/other.rs", "wasmtime::Engine"),
        ("host/execute.rs.bak", "wasmtime::Engine"),
        ("qualification/execute_other.rs", "wasmtime::Engine"),
        ("host/nested/runtime.rs", "wasmtime::Engine"),
        ("qualification/capability_other.rs", "wasmtime::Engine"),
        (
            "qualification/capability_invocations.rs.bak",
            "wasmtime::Engine",
        ),
        ("host/runtime.rs", "deserialize"),
        ("host/runtime.rs", "Status::Passed"),
        ("qualification/runner.rs", "Status::Passed"),
        ("qualification/runner.rs", "env::args()"),
        ("bin/qualification_runner.rs", "Engine::new()"),
        (
            "qualification/compile_cache_runner.rs",
            "include_bytes!(\"../../tests/fixtures/was-02/native.elf\")",
        ),
    ] {
        assert!(!source_valid(Path::new(path), source), "{path}: {source}");
    }
}

#[test]
fn native_load_exception_is_exactly_one_source_path() {
    assert!(source_valid(Path::new("cache/load.rs"), "deserialize"));
    for sibling in [
        "cache/other.rs",
        "cache/nested/load.rs",
        "artifact/load.rs",
        "qualification/load.rs",
        "cache/load.rs.bak",
    ] {
        assert!(
            !source_valid(Path::new(sibling), "deserialize"),
            "{sibling}"
        );
    }
}

#[test]
fn api_may_use_serde_but_never_wasmtime_artifact_loading() {
    // worker/src/api parses JSON requests only, so serde Deserialize, a serde
    // `fn deserialize` and `deserialize_with` are allowed there (RUN-01.t1).
    let serde_code = "use serde::{Deserialize, Deserializer};\n\
        fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {\n\
            T::deserialize(d).map(Some)\n\
        }\n\
        struct S { #[serde(default, deserialize_with = \"present\")] x: Option<u8> }\n\
        impl<'de> Deserialize<'de> for T { fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> { todo!() } }";
    assert!(source_valid(Path::new("api/x.rs"), serde_code));
    assert!(source_valid(Path::new("api/nested/x.rs"), serde_code));
    // The same serde code anywhere else still fails, so does a lookalike directory.
    for path in [
        "host/x.rs",
        "config.rs",
        "api.rs",
        "apix/x.rs",
        "cache/other.rs",
        "xapi/x.rs",
    ] {
        assert!(!source_valid(Path::new(path), serde_code), "{path}");
    }
    // Wasmtime precompiled loading fails in api/ in every spelling the guard knows.
    for loader in [
        "Module::deserialize(&engine, bytes);",
        "Module :: deserialize ( &engine , bytes );",
        "wasmtime::Module::deserialize(&engine, bytes);",
        "Component::deserialize(&engine, bytes);",
        "Component::deserialize_file(&engine, path);",
        "Module::deserialize_file(&engine, path);",
        "Module::deserialize_open_file(&engine, file);",
        "Component::deserialize_raw(&engine, ptr);",
        "Module::deserialize_raw(&engine, ptr);",
        "use wasmtime::Module as M; fn f() { M::deserialize(); }",
        "use ::wasmtime as w;",
        "extern crate wasmtime as w;",
        "fn deserialize_raw() {}",
        "Module /* hidden */ :: deserialize(&engine, bytes);",
        "Module::/* hidden */deserialize(&engine, bytes);",
        "Component /* a /* nested */ b */ :: deserialize_file(&engine, path);",
        "Module // line comment\n :: deserialize(&engine, bytes);",
        "use wasmtime/* hidden */::Module;",
        "use /**/ ::wasmtime::Module as M;",
        "extern crate /* hidden */ wasmtime;",
        "fn f() { /* hidden */ Module::deserialize_open_file(&engine, file) }",
        "fn f() { r#deserialize_raw(); }",
        "// Module::deserialize in a comment\nfn f() { Module::deserialize(&e, b); }",
        "fn f() { Module::deserialize(&e, b); } /* trailing */",
    ] {
        let source = format!("{serde_code}\n{loader}\n");
        assert!(!source_valid(Path::new("api/x.rs"), &source), "{loader}");
        assert!(!source_valid(Path::new("host/x.rs"), &source), "{loader}");
    }
    // Text in comments and literals is not code: a doc comment or a message may name the
    // loaders. (`source_valid` has further plain-text rules, so this judges the deserialize rule.)
    for harmless in [
        "// Module::deserialize is forbidden here\nfn f() {}",
        "/* Component::deserialize_file, wasmtime::Engine and the loaders */ fn f() {}",
        "/// Never call `Module::deserialize`.\nfn f() {}",
        "fn f() -> &'static str { \"Module::deserialize wasmtime::Engine\" }",
        "fn f() -> &'static str { r#\"Module::deserialize \"wasmtime\" \"# }",
        "fn f<'a>(x: &'a str, c: char) -> bool { c == '\"' && x.is_empty() }",
        "struct Identity { wasmtime_version: String, module: u8 }",
    ] {
        assert!(
            deserialize_valid(Path::new("api/x.rs"), harmless),
            "{harmless}"
        );
    }
    // The keyword for unchecked code fails in api/ too: engine artifact loading needs it.
    let keyword = format!("fn f() {{ {UNSAFE} {{}} }}");
    assert!(!source_valid(Path::new("api/x.rs"), &keyword));
    // Identifiers that merely contain the engine name stay legal (`wasmtime_version`).
    assert!(source_valid(
        Path::new("api/x.rs"),
        "struct Identity { wasmtime_version: String }\n"
    ));
}

#[test]
fn api_exception_does_not_widen_the_native_load_exception() {
    // cache/load.rs keeps its exception unchanged, and nothing else gains it.
    assert!(source_valid(
        Path::new("cache/load.rs"),
        "Module::deserialize(&engine, bytes);"
    ));
    assert!(!source_valid(
        Path::new("api/load.rs"),
        "Module::deserialize(&engine, bytes);"
    ));
    assert!(!source_valid(
        Path::new("cache/api/load.rs"),
        "fn deserialize() {}"
    ));
}
