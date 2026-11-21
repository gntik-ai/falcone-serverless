//! Static qualification diagnostics, never an execution or enablement gate.
//! Comments are checked independently of import traversal. Strings and rune
//! literals are opaque, so tenant text is never included in diagnostics.
use std::collections::BTreeSet;

pub const VERSIONS: &str = include_str!("../qualification/go_versions.toml");
const UNSUPPORTED: &str = "WASM-UNSUPPORTED-API";

pub struct TreeFile<'a> {
    pub path: &'a str,
    pub text: &'a str,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: String,
    pub line: usize,
    pub code: &'static str,
    pub construct: &'static str,
}
#[derive(Debug)]
pub struct Verdict(Vec<Diagnostic>);
impl Verdict {
    pub fn supported(&self) -> bool {
        self.0.is_empty()
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
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
    line: usize,
    kind: Kind,
}

// Go import paths with escapes are conservatively refused instead of decoded.
fn tokens(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let (mut at, mut line) = (0, 1);
    let mut out = Vec::new();
    while at < bytes.len() {
        let start = at;
        let origin = line;
        let kind = match bytes[at] {
            b'\n' => {
                line += 1;
                at += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                at += 1;
                continue;
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at += 2;
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
                Kind::Comment
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at < bytes.len() && !bytes[at..].starts_with(b"*/") {
                    if bytes[at] == b'\n' {
                        line += 1;
                    }
                    at += 1;
                }
                at = (at + 2).min(bytes.len());
                Kind::Comment
            }
            quote @ (b'"' | b'\'' | b'`') => {
                at += 1;
                while at < bytes.len() {
                    if bytes[at] == b'\n' {
                        line += 1;
                    }
                    if bytes[at] == quote {
                        at += 1;
                        break;
                    }
                    if bytes[at] == b'\\' && quote != b'`' {
                        at += 1;
                    }
                    at = (at + 1).min(bytes.len());
                }
                Kind::String
            }
            _ if text[at..]
                .chars()
                .next()
                .is_some_and(|c| c == '_' || c.is_alphabetic()) =>
            {
                // Go identifiers may contain Unicode letters and digits. Keep the
                // whole name intact for both import aliases and their uses.
                while let Some(c) = text[at..].chars().next() {
                    if c != '_' && !c.is_alphanumeric() {
                        break;
                    }
                    at += c.len_utf8();
                }
                Kind::Word
            }
            _ => {
                at += text[at..].chars().next().unwrap().len_utf8();
                Kind::Punctuation
            }
        };
        out.push(Token {
            text: &text[start..at],
            line: origin,
            kind,
        });
    }
    out
}
fn emit(
    out: &mut Vec<Diagnostic>,
    path: &str,
    line: usize,
    code: &'static str,
    construct: &'static str,
) {
    if out.len() < 256
        && !out
            .iter()
            .any(|d| d.path == path && d.line == line && d.code == code)
    {
        out.push(Diagnostic {
            path: path.into(),
            line,
            code,
            construct,
        });
    }
}
fn package(path: &str) -> Option<(&'static str, &'static str)> {
    match path {
        "C" => Some(("WASM-NATIVE-LIBRARY", "cgo import")),
        "os/exec" | "syscall" => Some(("WASM-PROCESS-SPAWN", "process API")),
        "encoding/json"
        | "encoding/hex"
        | "crypto/sha256"
        | "regexp"
        | "reflect"
        | "falcone.local/go-profile/bindings" => None,
        _ if ["cuda", "opencl", "vulkan", "gpu"]
            .iter()
            .any(|name| path.to_ascii_lowercase().contains(name)) =>
        {
            Some(("WASM-GPU", "GPU package"))
        }
        _ if path
            .split('/')
            .next()
            .is_some_and(|name| name.contains('.')) =>
        {
            Some(("WASM-PACKAGE-NOT-IN-CATALOGUE", "uncatalogued module"))
        }
        _ => Some((UNSUPPORTED, "package outside profile")),
    }
}

/// Check all supplied files, including unreachable files. This never executes a guest,
/// generator, compiler, package installer or lifecycle hook. The module catalogue is empty.
pub fn check_tree(files: &[TreeFile<'_>], go_mod: Option<&str>) -> Verdict {
    let mut out = Vec::new();
    if let Some(module) = go_mod {
        for (line, text) in module.lines().enumerate() {
            if matches!(
                text.split_whitespace().next(),
                Some("require" | "replace" | "exclude" | "tool" | "toolchain")
            ) {
                emit(
                    &mut out,
                    "go.mod",
                    line + 1,
                    "WASM-PACKAGE-NOT-IN-CATALOGUE",
                    "module outside pinned catalogue",
                );
            }
        }
    }
    for file in files {
        let all = tokens(file.text);
        // Never let grouped imports consume a directive without checking it.
        for token in &all {
            if token.kind != Kind::Comment {
                continue;
            }
            if token.text.starts_with("//go:generate") {
                emit(
                    &mut out,
                    file.path,
                    token.line,
                    "WASM-INSTALL-HOOK",
                    "go:generate",
                );
            }
            if token.text.starts_with("//go:linkname") {
                emit(&mut out, file.path, token.line, UNSUPPORTED, "go:linkname");
            }
            for (offset, line) in token.text.lines().enumerate() {
                if line.trim_start().starts_with("#cgo") {
                    emit(
                        &mut out,
                        file.path,
                        token.line + offset,
                        "WASM-NATIVE-LIBRARY",
                        "cgo directive",
                    );
                }
            }
        }
        let tokens: Vec<_> = all.iter().filter(|t| t.kind != Kind::Comment).collect();
        let mut aliases = BTreeSet::new();
        let mut dot_reflect = false;
        let mut at = 0;
        while at < tokens.len() {
            if tokens[at].kind != Kind::Word || tokens[at].text != "import" {
                at += 1;
                continue;
            }
            at += 1;
            let grouped = tokens.get(at).is_some_and(|t| t.text == "(");
            if grouped {
                at += 1;
            }
            loop {
                while tokens.get(at).is_some_and(|t| t.text == ";") {
                    at += 1;
                }
                if tokens.get(at).is_none_or(|t| t.text == ")") {
                    at += 1;
                    break;
                }
                let mut alias = None;
                if tokens[at].kind != Kind::String {
                    alias = Some(tokens[at].text);
                    at += 1;
                }
                let Some(token) = tokens.get(at).filter(|t| t.kind == Kind::String) else {
                    emit(
                        &mut out,
                        file.path,
                        tokens.get(at.saturating_sub(1)).map_or(1, |t| t.line),
                        UNSUPPORTED,
                        "unresolved import",
                    );
                    break;
                };
                let path = token
                    .text
                    .strip_prefix('"')
                    .and_then(|p| p.strip_suffix('"'));
                match path {
                    Some(path) if !path.contains('\\') => {
                        if let Some((code, construct)) = package(path) {
                            emit(&mut out, file.path, token.line, code, construct);
                        }
                        if path == "reflect" {
                            match alias.unwrap_or("reflect") {
                                "." => dot_reflect = true,
                                "_" => {}
                                name => {
                                    aliases.insert(name);
                                }
                            }
                        }
                    }
                    _ => emit(
                        &mut out,
                        file.path,
                        token.line,
                        UNSUPPORTED,
                        "unresolved import",
                    ),
                }
                at += 1;
                if !grouped {
                    break;
                }
            }
        }
        for (index, token) in tokens.iter().enumerate() {
            if token.kind != Kind::Word {
                continue;
            }
            if aliases.contains(token.text) && tokens.get(index + 1).is_some_and(|t| t.text == ".")
            {
                if let Some(api) = tokens.get(index + 2)
                    && !matches!(api.text, "TypeOf" | "ValueOf" | "DeepEqual")
                {
                    emit(
                        &mut out,
                        file.path,
                        token.line,
                        UNSUPPORTED,
                        "unsupported reflection API",
                    );
                }
            } else if dot_reflect
                && matches!(
                    token.text,
                    "MakeFunc" | "StructOf" | "FuncOf" | "NewAt" | "SliceAt"
                )
            {
                emit(
                    &mut out,
                    file.path,
                    token.line,
                    UNSUPPORTED,
                    "unsupported reflection API",
                );
            }
        }
    }
    out.sort_by(|a, b| (&a.path, a.line, a.code).cmp(&(&b.path, b.line, b.code)));
    Verdict(out)
}
