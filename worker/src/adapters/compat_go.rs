//! Static Go profile eligibility. This does not execute sources or enable a profile.
//! Comment checks are a separate pass over every token, independent of import traversal.
use super::compat::{TreeDiagnostic, TreeFile};

const UNSUPPORTED: &str = "WASM-UNSUPPORTED-API";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Comment,
    String,
    Word,
    Symbol,
}
struct Token<'a> {
    kind: Kind,
    text: &'a str,
    at: usize,
}

fn lex(source: &str) -> Result<Vec<Token<'_>>, ()> {
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut out = Vec::new();
    while at < bytes.len() {
        let start = at;
        let c = source[at..].chars().next().ok_or(())?;
        if c.is_whitespace() {
            at += c.len_utf8();
            continue;
        }
        let kind = if source[at..].starts_with("//") {
            at = source[at..].find('\n').map_or(bytes.len(), |n| at + n);
            Kind::Comment
        } else if source[at..].starts_with("/*") {
            at += 2;
            at += source[at..].find("*/").ok_or(())? + 2;
            Kind::Comment
        } else if matches!(c, '"' | '\'' | '`') {
            at += 1;
            loop {
                let b = *bytes.get(at).ok_or(())?;
                at += 1;
                if b == c as u8 {
                    break;
                }
                if c != '`' && b == b'\n' {
                    return Err(());
                }
                if c != '`' && b == b'\\' {
                    at += 1;
                }
            }
            if c == '\'' {
                Kind::Symbol
            } else {
                Kind::String
            }
        } else if c.is_alphabetic() || c == '_' {
            at += c.len_utf8();
            while let Some(c) = source[at..].chars().next() {
                if !c.is_alphanumeric() && c != '_' {
                    break;
                }
                at += c.len_utf8();
            }
            Kind::Word
        } else {
            at += c.len_utf8();
            Kind::Symbol
        };
        out.push(Token {
            kind,
            text: &source[start..at],
            at: start,
        });
    }
    Ok(out)
}

fn diagnostic(
    file: &TreeFile<'_>,
    at: usize,
    code: &'static str,
    construct: &'static str,
) -> TreeDiagnostic {
    let prefix = &file.text[..at];
    TreeDiagnostic {
        code,
        construct,
        path: file.path.to_owned(),
        line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        column: prefix
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count()
            + 1,
    }
}

pub fn check_tree(files: &[TreeFile<'_>], manifest: &str) -> Vec<TreeDiagnostic> {
    let mut out = Vec::new();
    let Some(profile) = profile(manifest) else {
        return vec![TreeDiagnostic {
            code: "WASM-PACKAGE-NOT-IN-CATALOGUE",
            construct: "profile manifest",
            path: "manifest.toml".into(),
            line: 1,
            column: 1,
        }];
    };
    if files.len() > 256 || files.iter().map(|f| f.text.len()).sum::<usize>() > 4 * 1024 * 1024 {
        return vec![TreeDiagnostic {
            code: UNSUPPORTED,
            construct: "source tree limit",
            path: "<tree>".into(),
            line: 1,
            column: 1,
        }];
    }
    for file in files {
        if !file.path.ends_with(".go") {
            let native = [".c", ".h", ".s", ".so", ".a"]
                .iter()
                .any(|ext| file.path.ends_with(ext));
            out.push(diagnostic(
                file,
                0,
                if native {
                    "WASM-NATIVE-LIBRARY"
                } else {
                    UNSUPPORTED
                },
                "non-Go source",
            ));
            continue;
        }
        let Ok(tokens) = lex(file.text) else {
            out.push(diagnostic(file, 0, UNSUPPORTED, "invalid Go source"));
            continue;
        };
        for token in tokens.iter().filter(|t| t.kind == Kind::Comment) {
            let text = token.text.trim();
            if text.starts_with("//go:generate") {
                out.push(diagnostic(
                    file,
                    token.at,
                    "WASM-INSTALL-HOOK",
                    "//go:generate",
                ));
            }
            if text.starts_with("//go:linkname") {
                out.push(diagnostic(file, token.at, UNSUPPORTED, "//go:linkname"));
            }
            if let Some(import) = text.strip_prefix("//go:wasmimport") {
                let mut fields = import.split_whitespace();
                let interface = fields.next().unwrap_or_default();
                let function = fields.next().unwrap_or_default();
                let allowed = profile["wit"]["interfaces"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(interface)));
                if !allowed
                    || (interface == "wasi:random/random@0.2.0"
                        && !matches!(function, "get-random-u64" | "get-random-bytes"))
                {
                    out.push(diagnostic(
                        file,
                        token.at,
                        UNSUPPORTED,
                        "undeclared host import",
                    ));
                }
            }
            if text.starts_with("//go:embed") {
                out.push(diagnostic(file, token.at, UNSUPPORTED, "//go:embed"));
            }
        }
        // Traversal never owns comments; directives above see every comment token.
        let tokens: Vec<_> = tokens.iter().filter(|t| t.kind != Kind::Comment).collect();
        let mut index = 0;
        while index < tokens.len() {
            let token = tokens[index];
            index += 1;
            if token.kind == Kind::Word && token.text == "go" {
                out.push(diagnostic(
                    file,
                    token.at,
                    "WASM-PROCESS-SPAWN",
                    "goroutine",
                ));
            }
            if token.kind != Kind::Word || token.text != "import" {
                continue;
            }
            let grouped = tokens.get(index).is_some_and(|t| t.text == "(");
            if grouped {
                index += 1;
            }
            while let Some(token) = tokens.get(index) {
                index += 1;
                if token.text == ")" {
                    break;
                }
                if token.kind == Kind::String {
                    match go_string(token.text) {
                        Some(import) => {
                            if let Some((code, construct)) = import_diagnostic(&import, &profile) {
                                out.push(diagnostic(file, token.at, code, construct));
                            }
                        }
                        None => out.push(diagnostic(
                            file,
                            token.at,
                            UNSUPPORTED,
                            "invalid import string",
                        )),
                    }
                    if !grouped {
                        break;
                    }
                } else if token.text != "." && token.text != ";" && token.kind != Kind::Word {
                    out.push(diagnostic(
                        file,
                        token.at,
                        UNSUPPORTED,
                        "invalid import declaration",
                    ));
                    break;
                }
            }
        }
    }
    out
}

fn exact(version: &str) -> bool {
    let mut parts = version.strip_prefix('v').unwrap_or(version).split('.');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    }) && parts.next().is_none()
}

fn profile(text: &str) -> Option<toml::Value> {
    let value: toml::Value = toml::from_str(text).ok()?;
    let get = |table: &str, key: &str| value.get(table)?.get(key)?.as_str();
    if get("runtime", "target")? != "wasip2"
        || get("runtime", "wasi")? != "0.2.0"
        || get("runtime", "wasmtime")? != "49.0.5"
        || get("wit", "package")? != "falcone:host@0.2.0"
        || get("wit", "world")? != "go-function"
        || get("profile", "status")? != "disabled"
    {
        return None;
    }
    let pins: toml::Value =
        toml::from_str(include_str!("../../qualification/go/versions.toml")).ok()?;
    for name in ["go", "tinygo"] {
        for field in ["name", "version", "source", "sha256", "archive"] {
            if value.get("toolchains")?.get(name)?.get(field)? != pins.get(name)?.get(field)? {
                return None;
            }
        }
    }
    let modules = value.get("catalogue")?.get("modules")?.as_array()?;
    let mut seen = std::collections::BTreeSet::new();
    for module in modules {
        let path = module.get("path")?.as_str()?;
        if path.is_empty() || !seen.insert(path) || !exact(module.get("version")?.as_str()?) {
            return None;
        }
    }
    let stdlib = value.get("stdlib")?.as_table()?;
    if stdlib.is_empty() || stdlib.values().any(|v| v.as_str() != Some("go@1.26.11")) {
        return None;
    }
    let interfaces = value.get("wit")?.get("interfaces")?.as_array()?;
    for interface in interfaces {
        if !crate::artifact::validate::ALLOWED_IMPORTS.contains(&interface.as_str()?) {
            return None;
        }
    }
    Some(value)
}

// Go import paths may use raw strings and Go's numeric escapes. Decode them before
// classification so an escaped "C" or "os/exec" cannot bypass the catalogue.
fn go_string(text: &str) -> Option<String> {
    if text.starts_with('`') {
        return Some(text[1..text.len() - 1].replace('\r', ""));
    }
    let mut chars = text[1..text.len() - 1].chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let escape = chars.next()?;
        let (radix, digits, first) = match escape {
            'x' => (16, 2, None),
            'u' => (16, 4, None),
            'U' => (16, 8, None),
            '0'..='7' => (8, 2, escape.to_digit(8)),
            '\\' => {
                out.push('\\');
                continue;
            }
            '"' => {
                out.push('"');
                continue;
            }
            _ => return None,
        };
        let mut value = first.unwrap_or(0);
        for _ in 0..digits {
            value = value
                .checked_mul(radix)?
                .checked_add(chars.next()?.to_digit(radix)?)?;
        }
        out.push(char::from_u32(value)?);
    }
    Some(out)
}

fn import_diagnostic(import: &str, profile: &toml::Value) -> Option<(&'static str, &'static str)> {
    if import == "C" {
        return Some(("WASM-NATIVE-LIBRARY", "cgo"));
    }
    if import == "os/exec" {
        return Some(("WASM-PROCESS-SPAWN", "os/exec"));
    }
    if import.split('/').any(|part| {
        let part = part.to_ascii_lowercase();
        part.contains("cuda") || part.contains("opencl") || part.contains("vulkan") || part == "gpu"
    }) {
        return Some(("WASM-GPU", "GPU import"));
    }
    if import == "reflect" {
        return Some((UNSUPPORTED, "reflect"));
    }
    // This path is provided by the platform's reviewed fixture bindings, not a module
    // installed from tenant sources. The build stages that binding directory itself.
    if import == "falcone.profile/bindings" {
        return None;
    }
    if profile["stdlib"].get(import).is_some() {
        return None;
    }
    if profile["catalogue"]["modules"]
        .as_array()
        .is_some_and(|modules| {
            modules.iter().any(|module| {
                module["path"].as_str().is_some_and(|path| {
                    import == path
                        || import
                            .strip_prefix(path)
                            .is_some_and(|tail| tail.starts_with('/'))
                })
            })
        })
    {
        return None;
    }
    if !import.split('/').next().unwrap_or_default().contains('.') {
        Some((UNSUPPORTED, "unlisted standard-library API"))
    } else {
        Some(("WASM-PACKAGE-NOT-IN-CATALOGUE", "module outside catalogue"))
    }
}
