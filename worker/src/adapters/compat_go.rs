//! Static Go source eligibility, never a compiler, executor or production enablement gate.
//! The artifact validator remains authoritative for every compiled component.
use super::compat::{TreeDiagnostic, TreeFile};

#[derive(Debug)]
pub struct Verdict(Vec<TreeDiagnostic>);
impl Verdict {
    pub fn supported(&self) -> bool {
        self.0.is_empty()
    }
    pub fn diagnostics(&self) -> &[TreeDiagnostic] {
        &self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    String,
    Comment,
    Punctuation,
}
struct Token<'a> {
    text: &'a str,
    at: usize,
    kind: Kind,
}

// Keep comments as first-class tokens. Strings (including raw strings and runes) are opaque,
// so a directive in a string cannot be confused with a comment.
fn tokens(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let kind = if text[i..].starts_with("//") {
            i = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
            Kind::Comment
        } else if text[i..].starts_with("/*") {
            i = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + n + 4);
            Kind::Comment
        } else if matches!(bytes[i], b'"' | b'\'' | b'`') {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                if quote != b'`' && bytes[i] == b'\\' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += text[i..].chars().next().unwrap().len_utf8();
                }
            }
            Kind::String
        } else if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            Kind::Word
        } else {
            i += text[i..].chars().next().unwrap().len_utf8();
            Kind::Punctuation
        };
        result.push(Token {
            text: &text[start..i],
            at: start,
            kind,
        });
    }
    result
}

fn emit(
    out: &mut Vec<TreeDiagnostic>,
    file: &TreeFile<'_>,
    at: usize,
    code: &'static str,
    construct: &'static str,
) {
    let prefix = &file.text[..at];
    out.push(TreeDiagnostic {
        code,
        construct,
        path: file.path.to_owned(),
        line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        column: prefix.rsplit('\n').next().unwrap().chars().count() + 1,
    });
}

const STDLIB: &[&str] = &["crypto/sha256", "encoding/hex", "strconv", "strings"];
fn import_diagnostic(literal: &str) -> Option<(&'static str, &'static str)> {
    let path = literal
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| literal.strip_prefix('`').and_then(|s| s.strip_suffix('`')))?;
    // Escaped import paths are refused, not interpreted as an approved package.
    if path.contains('\\') {
        return Some(("WASM-UNSUPPORTED-API", "escaped import path"));
    }
    if path == "C" || path == "plugin" {
        return Some(("WASM-NATIVE-LIBRARY", "native import"));
    }
    if path == "os/exec" {
        return Some(("WASM-PROCESS-SPAWN", "os/exec import"));
    }
    let lower = path.to_ascii_lowercase();
    if ["gpu", "cuda", "opencl", "vulkan"]
        .iter()
        .any(|part| lower.contains(part))
    {
        return Some(("WASM-GPU", "GPU import"));
    }
    if STDLIB.contains(&path) || path == "falcone-profile/bindings" {
        return None;
    }
    if path
        .split('/')
        .next()
        .is_some_and(|part| part.contains('.'))
    {
        Some(("WASM-PACKAGE-NOT-IN-CATALOGUE", "module outside catalogue"))
    } else {
        Some((
            "WASM-UNSUPPORTED-API",
            "package outside supported standard library",
        ))
    }
}

// This pass sees every comment, before the import parser can consume any tokens.
fn check_comments(out: &mut Vec<TreeDiagnostic>, file: &TreeFile<'_>, all: &[Token<'_>]) {
    for token in all.iter().filter(|t| t.kind == Kind::Comment) {
        let mut offset = token.at;
        for line in token.text.split_inclusive('\n') {
            if let Some(at) = line.find("go:") {
                let directive = &line[at..];
                let (code, construct) = if directive.starts_with("go:generate") {
                    ("WASM-INSTALL-HOOK", "go:generate directive")
                } else {
                    ("WASM-UNSUPPORTED-API", "Go compiler directive")
                };
                emit(out, file, offset + at, code, construct);
            } else if let Some(at) = line.find("#cgo") {
                emit(
                    out,
                    file,
                    offset + at,
                    "WASM-NATIVE-LIBRARY",
                    "cgo preamble",
                );
            } else if line.starts_with("//export ") || line.starts_with("//line ") {
                emit(
                    out,
                    file,
                    offset,
                    "WASM-UNSUPPORTED-API",
                    "Go export or location directive",
                );
            }
            offset += line.len();
        }
    }
}

/// Every source file is checked, including files with build tags. No guest is built or run.
pub fn check_tree(files: &[TreeFile<'_>]) -> Verdict {
    let mut out = Vec::new();
    for file in files {
        let all = tokens(file.text);
        check_comments(&mut out, file, &all);
        for token in all
            .iter()
            .filter(|t| t.kind == Kind::Word && t.text == "go")
        {
            emit(&mut out, file, token.at, "WASM-PROCESS-SPAWN", "goroutine");
        }
        let significant: Vec<_> = all.iter().filter(|t| t.kind != Kind::Comment).collect();
        let mut i = 0;
        while i < significant.len() {
            if significant[i].text != "import" || significant[i].kind != Kind::Word {
                i += 1;
                continue;
            }
            i += 1;
            let grouped = significant.get(i).is_some_and(|t| t.text == "(");
            if grouped {
                i += 1;
            }
            while let Some(token) = significant.get(i) {
                if grouped && token.text == ")" {
                    break;
                }
                if token.kind == Kind::String {
                    if let Some((code, construct)) = import_diagnostic(token.text) {
                        emit(&mut out, file, token.at, code, construct);
                    }
                    if !grouped {
                        break;
                    }
                }
                i += 1;
            }
            i += 1;
        }
    }
    Verdict(out)
}
