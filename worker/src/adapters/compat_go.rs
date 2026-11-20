//! Static compatibility check for the Go (TinyGo wasip2) profile. It reads source text only:
//! no Go toolchain runs and no guest is built or executed. Every diagnostic names the
//! construct and its location (path, line, byte column), never source text. Supported means
//! statically eligible for qualification, never enabled to run.
//!
//! Directives are checked on every comment token of a file, separately from import
//! traversal, so a `//go:generate` or `//go:linkname` cannot hide between or inside imports.

use super::compat::{TreeDiagnostic, TreeFile};
use std::collections::BTreeMap;

/// The pins and the standard-library allow-list; the one source of truth lives under
/// worker/src so the image build context contains it.
pub const PINS: &str = include_str!("../qualification/go_versions.toml");

const NATIVE: &str = "WASM-NATIVE-LIBRARY";
const GPU: &str = "WASM-GPU";
const SPAWN: &str = "WASM-PROCESS-SPAWN";
const HOOK: &str = "WASM-INSTALL-HOOK";
const CATALOGUE: &str = "WASM-PACKAGE-NOT-IN-CATALOGUE";
const UNSUPPORTED: &str = "WASM-UNSUPPORTED-API";

const MAX_FILES: usize = 256;
const MAX_FILE_BYTES: usize = 1_048_576;
/// In-tree import path (relative to the module) of the trusted platform bindings. The trusted
/// image supplies that directory; a tenant tree may not contain files under it.
pub const BINDINGS_DIR: &str = "bindings";

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

/// Standard-library packages a tenant source may import, bound to the pinned Go release.
pub fn stdlib_allow_list() -> Vec<String> {
    let pins: toml::Value = toml::from_str(PINS).expect("embedded pins parse");
    pins["stdlib"]["packages"]
        .as_array()
        .expect("stdlib packages")
        .iter()
        .map(|package| package.as_str().expect("package name").to_owned())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Text,
    Punctuation,
}

struct Token<'a> {
    kind: Kind,
    /// Raw source text, including quotes for string and rune literals.
    text: &'a str,
    at: usize,
}

struct Comment<'a> {
    block: bool,
    /// The comment body, without its delimiters.
    body: &'a str,
    at: usize,
}

#[derive(Default)]
struct Lexed<'a> {
    tokens: Vec<Token<'a>>,
    comments: Vec<Comment<'a>>,
    /// Constructs that cannot be lexed: offset and construct. Lexing resumes after each.
    errors: Vec<(usize, &'static str)>,
}

fn lex(text: &str) -> Lexed<'_> {
    let bytes = text.as_bytes();
    let mut out = Lexed::default();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            b' ' | b'\t' | b'\r' | b'\n' => at += 1,
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                let end = bytes[at..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| at + offset);
                out.comments.push(Comment {
                    block: false,
                    body: &text[at + 2..end],
                    at,
                });
                at = end;
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => match text[at + 2..].find("*/") {
                Some(offset) => {
                    out.comments.push(Comment {
                        block: true,
                        body: &text[at + 2..at + 2 + offset],
                        at,
                    });
                    at += offset + 4;
                }
                None => {
                    out.errors.push((at, "unterminated comment"));
                    at = bytes.len();
                }
            },
            quote @ (b'"' | b'\'') => {
                let mut end = at + 1;
                loop {
                    match bytes.get(end) {
                        None | Some(b'\n') => {
                            out.errors.push((start, "unterminated literal"));
                            at = end;
                            break;
                        }
                        Some(b'\\') => end += 2,
                        Some(byte) if *byte == quote => {
                            out.tokens.push(Token {
                                kind: Kind::Text,
                                text: &text[start..=end],
                                at: start,
                            });
                            at = end + 1;
                            break;
                        }
                        Some(_) => end += 1,
                    }
                }
            }
            b'`' => match text[at + 1..].find('`') {
                Some(offset) => {
                    out.tokens.push(Token {
                        kind: Kind::Text,
                        text: &text[at..at + offset + 2],
                        at,
                    });
                    at += offset + 2;
                }
                None => {
                    out.errors.push((at, "unterminated literal"));
                    at = bytes.len();
                }
            },
            byte if byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80 => {
                while at < bytes.len()
                    && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || bytes[at] >= 0x80)
                {
                    at += 1;
                }
                out.tokens.push(Token {
                    kind: Kind::Word,
                    text: &text[start..at],
                    at: start,
                });
            }
            _ => {
                at += 1;
                out.tokens.push(Token {
                    kind: Kind::Punctuation,
                    text: &text[start..at],
                    at: start,
                });
            }
        }
    }
    out
}

fn position(text: &str, at: usize) -> (usize, usize) {
    let before = &text.as_bytes()[..at.min(text.len())];
    let line = before.iter().filter(|byte| **byte == b'\n').count() + 1;
    let column = before
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(at, |newline| at - newline - 1)
        + 1;
    (line, column)
}

struct Report<'a> {
    path: &'a str,
    text: &'a str,
    out: Vec<TreeDiagnostic>,
}

impl Report<'_> {
    fn at(&mut self, at: usize, code: &'static str, construct: &'static str) {
        let (line, column) = position(self.text, at);
        self.out.push(TreeDiagnostic {
            code,
            construct,
            path: self.path.to_owned(),
            line,
            column,
        });
    }
}

struct Import<'a> {
    /// Unquoted import path, or `None` when it uses an escape sequence.
    path: Option<&'a str>,
    alias: Option<&'a str>,
    at: usize,
}

fn unquote(literal: &str) -> Option<&str> {
    let inner = literal.get(1..literal.len().checked_sub(1)?)?;
    match literal.as_bytes()[0] {
        // Escapes could spell `"C"` or any other path; they are refused, never interpreted.
        b'"' if inner.contains('\\') => None,
        b'"' | b'`' => Some(inner),
        _ => None,
    }
}

/// Every import spec of a file, in a grouped or a single `import` declaration. Comments are
/// lexed apart from tokens, so they never interrupt a group.
fn imports<'a>(tokens: &[Token<'a>]) -> Vec<Import<'a>> {
    let mut out = Vec::new();
    let mut i = 0;
    let spec = |i: &mut usize, out: &mut Vec<Import<'a>>| {
        let mut alias = None;
        if let Some(token) = tokens.get(*i) {
            let next_is_text = tokens
                .get(*i + 1)
                .is_some_and(|next| next.kind == Kind::Text);
            if next_is_text && (token.kind == Kind::Word || token.text == ".") {
                alias = Some(token.text);
                *i += 1;
            }
        }
        match tokens.get(*i) {
            Some(token) if token.kind == Kind::Text => {
                out.push(Import {
                    path: unquote(token.text),
                    alias,
                    at: token.at,
                });
            }
            _ => {}
        }
        *i += 1;
    };
    while i < tokens.len() {
        let token = &tokens[i];
        let selector = i > 0 && tokens[i - 1].text == ".";
        i += 1;
        if token.kind != Kind::Word || token.text != "import" || selector {
            continue;
        }
        if tokens.get(i).is_some_and(|next| next.text == "(") {
            i += 1;
            while i < tokens.len() && tokens[i].text != ")" {
                if tokens[i].text == ";" {
                    i += 1;
                } else {
                    spec(&mut i, &mut out);
                }
            }
            i += 1;
        } else {
            spec(&mut i, &mut out);
        }
    }
    out
}

/// A directive in one comment. Position independent: `//go:name` is recognised wherever the
/// comment starts, and a `#cgo` line is recognised in a line or a block comment.
fn directives(comment: &Comment<'_>, report: &mut Report<'_>, interfaces: &[&str]) {
    if !comment.block {
        let body = comment.body;
        if body.starts_with("go:generate") {
            report.at(comment.at, HOOK, "go:generate directive");
        } else if body.starts_with("go:linkname") {
            report.at(comment.at, UNSUPPORTED, "go:linkname directive");
        } else if body.starts_with("go:cgo_") || body.trim_start().starts_with("#cgo") {
            report.at(comment.at, NATIVE, "cgo directive");
        } else if body.starts_with("go:wasmexport") {
            report.at(comment.at, UNSUPPORTED, "go:wasmexport directive");
        } else if let Some(rest) = body.strip_prefix("go:wasmimport") {
            // Only an interface the profile allows may be imported.
            let module = rest.split_whitespace().next().unwrap_or_default();
            if !interfaces.contains(&module) {
                report.at(
                    comment.at,
                    UNSUPPORTED,
                    "go:wasmimport of an interface outside the allow-list",
                );
            }
        }
    } else if comment.body.lines().any(|line| {
        line.trim_start()
            .trim_start_matches('*')
            .trim_start()
            .starts_with("#cgo")
    }) {
        report.at(comment.at, NATIVE, "cgo directive");
    }
}

fn gpu(path: &str) -> bool {
    path.to_ascii_lowercase()
        .split(['/', '.', '-', '_'])
        .any(|part| {
            matches!(
                part,
                "gpu" | "cuda" | "opencl" | "vulkan" | "webgpu" | "nvml" | "rocm" | "hip"
            )
        })
}

/// TinyGo cannot construct functions, structs or channels at run time through reflection.
fn reflect_limit(name: &str) -> Option<&'static str> {
    Some(match name {
        "MakeFunc" => "reflect.MakeFunc (TinyGo reflection limit)",
        "FuncOf" => "reflect.FuncOf (TinyGo reflection limit)",
        "StructOf" => "reflect.StructOf (TinyGo reflection limit)",
        "ChanOf" => "reflect.ChanOf (TinyGo reflection limit)",
        "MakeChan" => "reflect.MakeChan (TinyGo reflection limit)",
        "Select" => "reflect.Select (TinyGo reflection limit)",
        _ => return None,
    })
}

struct Module<'a> {
    path: Option<&'a str>,
}

fn check_go_file(
    file: &TreeFile<'_>,
    module: &Module<'_>,
    tenant_dirs: &[&str],
    stdlib: &[String],
    interfaces: &[&str],
    out: &mut Vec<TreeDiagnostic>,
) {
    let lexed = lex(file.text);
    let mut report = Report {
        path: file.path,
        text: file.text,
        out: Vec::new(),
    };
    for (at, construct) in &lexed.errors {
        report.at(*at, UNSUPPORTED, construct);
    }
    for comment in &lexed.comments {
        directives(comment, &mut report, interfaces);
    }
    let mut reflect = None;
    for import in imports(&lexed.tokens) {
        let Some(path) = import.path else {
            report.at(
                import.at,
                UNSUPPORTED,
                "import path with an escape sequence",
            );
            continue;
        };
        let first = path.split('/').next().unwrap_or_default();
        if path == "C" || path == "runtime/cgo" || path == "plugin" {
            report.at(import.at, NATIVE, "native code import");
        } else if path == "os/exec" {
            report.at(import.at, SPAWN, "os/exec process spawning");
        } else if let Some(relative) = module
            .path
            .and_then(|root| path.strip_prefix(root))
            .and_then(|rest| rest.strip_prefix('/'))
        {
            // Only the trusted bindings or a directory of this tree resolves in-tree.
            let resolves = relative == BINDINGS_DIR || tenant_dirs.contains(&relative);
            if !resolves {
                report.at(import.at, UNSUPPORTED, "import outside the source tree");
            }
        } else if first.contains('.') {
            if gpu(path) {
                report.at(import.at, GPU, "GPU package import");
            } else {
                report.at(import.at, CATALOGUE, "package outside the pinned catalogue");
            }
        } else if !stdlib.iter().any(|allowed| allowed == path) {
            report.at(
                import.at,
                UNSUPPORTED,
                "standard-library package outside the allow-list",
            );
        } else if path == "reflect" {
            reflect = Some(import.alias.unwrap_or("reflect"));
        }
    }
    if let Some(name) = reflect {
        for (i, token) in lexed.tokens.iter().enumerate() {
            let selector = i > 0 && lexed.tokens[i - 1].text == ".";
            if token.kind != Kind::Word || token.text != name || selector {
                continue;
            }
            if let (Some(dot), Some(member)) = (lexed.tokens.get(i + 1), lexed.tokens.get(i + 2))
                && dot.text == "."
                && let Some(construct) = reflect_limit(member.text)
            {
                report.at(token.at, UNSUPPORTED, construct);
            }
        }
    }
    out.extend(report.out);
}

/// go.mod: the module path, plus every directive that would reach outside the tree.
fn check_go_mod<'a>(file: &TreeFile<'a>, out: &mut Vec<TreeDiagnostic>) -> Module<'a> {
    let mut report = Report {
        path: file.path,
        text: file.text,
        out: Vec::new(),
    };
    let mut module = Module { path: None };
    let mut block: Option<&str> = None;
    let mut offset = 0;
    for line in file.text.split_inclusive('\n') {
        let at = offset;
        offset += line.len();
        let code = line.split("//").next().unwrap_or_default().trim();
        if code.is_empty() {
            continue;
        }
        if block.is_some() {
            if code == ")" {
                block = None;
            } else if block == Some("require") || block == Some("replace") {
                report.at(at, CATALOGUE, "module outside the pinned catalogue");
            } else if block == Some("tool") {
                report.at(at, HOOK, "go.mod tool directive");
            }
            continue;
        }
        let mut words = code.split_whitespace();
        let (keyword, rest) = (words.next().unwrap_or_default(), words.next());
        match (keyword, rest) {
            ("module", Some(path)) => module.path = Some(path.trim_matches('"')),
            ("require" | "replace" | "tool", Some("(")) => {
                block = Some(match keyword {
                    "require" => "require",
                    "replace" => "replace",
                    _ => "tool",
                });
            }
            ("require" | "replace", _) => {
                report.at(at, CATALOGUE, "module outside the pinned catalogue");
            }
            ("tool", _) => report.at(at, HOOK, "go.mod tool directive"),
            ("toolchain", _) => report.at(at, UNSUPPORTED, "go.mod toolchain directive"),
            _ => {}
        }
    }
    out.extend(report.out);
    module
}

fn extension(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map_or("", |(_, extension)| extension)
}

/// Check one tenant Go source tree. `files` are relative, `/`-separated paths; `.go` files
/// and `go.mod` are read, and a native-code or build-script file is a diagnostic by itself.
/// Diagnostics are sorted by path, line and column.
pub fn check_tree(files: &[TreeFile<'_>]) -> Verdict {
    let at_origin = |path: &str, construct| TreeDiagnostic {
        code: UNSUPPORTED,
        construct,
        path: path.to_owned(),
        line: 1,
        column: 1,
    };
    if files.len() > MAX_FILES || files.iter().any(|file| file.text.len() > MAX_FILE_BYTES) {
        return Verdict(vec![at_origin("", "source bundle")]);
    }
    let mut out = Vec::new();
    let mut seen = BTreeMap::new();
    for file in files {
        let bad = file.path.is_empty()
            || file.path.starts_with('/')
            || file.path.contains('\\')
            || file
                .path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..");
        if bad {
            out.push(at_origin(file.path, "tree path"));
        } else if seen.insert(file.path, ()).is_some() {
            out.push(at_origin(file.path, "duplicate tree path"));
        } else if file.path == BINDINGS_DIR || file.path.starts_with("bindings/") {
            out.push(at_origin(file.path, "reserved platform bindings path"));
        }
    }
    let stdlib = stdlib_allow_list();
    let interfaces = crate::artifact::validate::ALLOWED_IMPORTS;
    let module = match files.iter().find(|file| file.path == "go.mod") {
        Some(file) => check_go_mod(file, &mut out),
        None => Module { path: None },
    };
    let dirs: Vec<&str> = files
        .iter()
        .filter(|file| extension(file.path) == "go")
        .filter_map(|file| file.path.rsplit_once('/').map(|(dir, _)| dir))
        .collect();
    for file in files {
        let name = file.path.rsplit('/').next().unwrap_or_default();
        match extension(file.path) {
            "go" => check_go_file(file, &module, &dirs, &stdlib, &interfaces, &mut out),
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "m" | "mm" | "s" | "S" | "syso" | "f"
            | "F" | "so" | "a" | "dll" | "dylib" | "o" => {
                out.push(TreeDiagnostic {
                    code: NATIVE,
                    construct: "native source or binary file",
                    path: file.path.to_owned(),
                    line: 1,
                    column: 1,
                });
            }
            _ if matches!(
                name,
                "Makefile" | "makefile" | "Dockerfile" | "Containerfile"
            ) =>
            {
                out.push(TreeDiagnostic {
                    code: HOOK,
                    construct: "build script file",
                    path: file.path.to_owned(),
                    line: 1,
                    column: 1,
                });
            }
            _ => {}
        }
    }
    out.sort_by(|a, b| {
        (&a.path, a.line, a.column, a.code).cmp(&(&b.path, b.line, b.column, b.code))
    });
    out.dedup();
    Verdict(out)
}
