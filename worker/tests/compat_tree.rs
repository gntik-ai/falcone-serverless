//! Tree-aware static check: relative imports are followed inside the supplied tree and
//! `process.env` is a stable unsupported API. Static only; nothing is built or executed.
use falcone_serverless_worker::adapters::compat::{TreeFile, check_tree};

const UNSUPPORTED: &str = "WASM-UNSUPPORTED-API";
const NOT_IN_CATALOGUE: &str = "WASM-PACKAGE-NOT-IN-CATALOGUE";

fn found(files: &[(&str, &str)], entry: &str) -> Vec<(String, &'static str, usize, usize)> {
    let files: Vec<_> = files
        .iter()
        .map(|(path, text)| TreeFile { path, text })
        .collect();
    check_tree(&files, entry, None)
        .diagnostics()
        .iter()
        .map(|d| (d.path.clone(), d.code, d.line, d.column))
        .collect()
}

#[test]
fn relative_imports_are_followed_and_never_reported_as_catalogue_packages() {
    let diagnostics = found(
        &[
            (
                "actions/main.mjs",
                "import { a } from '../src/a.mjs';\nimport { b } from './b.mjs';\n",
            ),
            ("src/a.mjs", "import fs from 'node:fs';\n"),
            ("actions/b.mjs", "export const b = 1;\n"),
        ],
        "actions/main.mjs",
    );
    assert!(diagnostics.iter().all(|d| d.1 != NOT_IN_CATALOGUE));
    // The followed file is checked: its node:fs import is attributed to it, with its line.
    assert!(diagnostics.contains(&("src/a.mjs".into(), UNSUPPORTED, 1, 16)));
}

#[test]
fn bare_packages_are_not_in_the_catalogue_with_file_and_line() {
    let diagnostics = found(
        &[(
            "main.mjs",
            "import pg from 'pg';\nimport { K } from 'kafkajs';\n",
        )],
        "main.mjs",
    );
    assert!(diagnostics.contains(&("main.mjs".into(), NOT_IN_CATALOGUE, 1, 16)));
    assert!(diagnostics.contains(&("main.mjs".into(), NOT_IN_CATALOGUE, 2, 19)));
}

#[test]
fn specifiers_that_escape_or_do_not_resolve_get_a_diagnostic() {
    for (specifier, line) in [
        ("../outside.mjs", 1),
        ("./missing.mjs", 1),
        ("../../x.mjs", 1),
    ] {
        let text = format!("import x from '{specifier}';\n");
        let diagnostics = found(&[("main.mjs", &text)], "main.mjs");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.0 == "main.mjs" && d.1 == UNSUPPORTED && d.2 == line),
            "{specifier}: {diagnostics:?}"
        );
        assert!(diagnostics.iter().all(|d| d.1 != NOT_IN_CATALOGUE));
    }
    // A nested path that climbs out of the root after a descent also escapes.
    let diagnostics = found(&[("a/b.mjs", "import x from '../../c.mjs';\n")], "a/b.mjs");
    assert!(diagnostics.iter().any(|d| d.1 == UNSUPPORTED && d.2 == 1));
    // Resolving back into the tree through a harmless dot segment is not an escape.
    let diagnostics = found(
        &[
            ("a/b.mjs", "import x from '../a/./c.mjs';\n"),
            ("a/c.mjs", "import fs from 'node:fs';\n"),
        ],
        "a/b.mjs",
    );
    assert!(diagnostics.contains(&("a/c.mjs".into(), UNSUPPORTED, 1, 16)));
    assert!(diagnostics.iter().all(|d| d.1 != NOT_IN_CATALOGUE));
}

#[test]
fn every_process_env_read_is_reported_at_its_line() {
    let text = "const a = process.env.A;\nconst b = process.env[name];\nconst c = obj.process.env.C;\nconst d = process.env.D || process.env.E;\n";
    let diagnostics = found(&[("config.mjs", text)], "config.mjs");
    let env: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.1 == UNSUPPORTED && d.2 != 0)
        .map(|d| (d.2, d.3))
        .collect();
    assert!(env.contains(&(1, 11)));
    assert!(env.contains(&(2, 11)));
    assert!(env.contains(&(4, 11)));
    assert!(env.contains(&(4, 28)));
    // A member named `process` on another object is not the ambient process.
    assert_eq!(env.iter().filter(|d| d.0 == 3).count(), 0);
}

#[test]
fn a_lex_failure_does_not_suppress_diagnostics_in_that_file_or_others() {
    // The escaped quote makes the lexer give up on the first string in `bad.mjs`.
    let bad = "import { createHash } from 'node:crypto';\nconst s = \"a\\\"b\";\nimport pg from 'pg';\nconst v = process.env.X;\n";
    let diagnostics = found(
        &[
            (
                "main.mjs",
                "import './bad.mjs';\nimport k from 'kafkajs';\n",
            ),
            ("bad.mjs", bad),
        ],
        "main.mjs",
    );
    assert!(diagnostics.contains(&("bad.mjs".into(), UNSUPPORTED, 1, 28)));
    assert!(diagnostics.contains(&("bad.mjs".into(), NOT_IN_CATALOGUE, 3, 16)));
    assert!(diagnostics.contains(&("bad.mjs".into(), UNSUPPORTED, 4, 11)));
    // The lex failure itself is still reported, at the construct that could not be lexed.
    assert!(diagnostics.contains(&("bad.mjs".into(), UNSUPPORTED, 2, 11)));
    assert!(diagnostics.contains(&("main.mjs".into(), NOT_IN_CATALOGUE, 2, 15)));
}

#[test]
fn an_escaped_backslash_string_does_not_absorb_a_same_line_import() {
    // The string `"a\\"` ends at its second quote. Resuming at a wrong boundary would make that
    // quote open a new string that swallows `import pg from 'pg'` up to the next double quote.
    let bad = "const s = \"a\\\\\"; import pg from 'pg'; const t = \"x\";\n";
    let diagnostics = found(&[("main.mjs", bad)], "main.mjs");
    assert!(
        diagnostics.contains(&("main.mjs".into(), NOT_IN_CATALOGUE, 1, 33)),
        "{diagnostics:?}"
    );
    // The string that could not be interpreted is still reported where it starts.
    assert!(diagnostics.contains(&("main.mjs".into(), UNSUPPORTED, 1, 11)));
}

#[test]
fn a_missing_entry_and_an_invalid_tree_path_are_diagnosed() {
    let diagnostics = found(&[("main.mjs", "export const a = 1;\n")], "absent.mjs");
    assert!(diagnostics.iter().any(|d| d.1 == UNSUPPORTED));
    let diagnostics = found(&[("../main.mjs", "export const a = 1;\n")], "../main.mjs");
    assert!(diagnostics.iter().any(|d| d.1 == UNSUPPORTED));
}

#[test]
fn the_manifest_is_checked_with_the_same_stable_codes() {
    let files = [TreeFile {
        path: "main.mjs",
        text: "export const a = 1;\n",
    }];
    let verdict = check_tree(
        &files,
        "main.mjs",
        Some("{\n  \"scripts\": {\"postinstall\": \"node x\"}\n}"),
    );
    let found: Vec<_> = verdict
        .diagnostics()
        .iter()
        .map(|d| (d.path.as_str(), d.code, d.line))
        .collect();
    assert!(found.contains(&("package.json", "WASM-INSTALL-HOOK", 2)));
}

#[test]
fn diagnostics_are_deterministic_and_never_carry_source_text() {
    let files = [TreeFile {
        path: "main.mjs",
        text: "import pg from 'pg';\nconst secret = process.env.SECRET_TOKEN_VALUE;\n",
    }];
    let first = check_tree(&files, "main.mjs", None);
    let second = check_tree(&files, "main.mjs", None);
    assert_eq!(first.diagnostics(), second.diagnostics());
    let rendered = format!("{:?}", first.diagnostics());
    assert!(!rendered.contains("SECRET_TOKEN_VALUE"));
    assert!(!first.supported());
}
