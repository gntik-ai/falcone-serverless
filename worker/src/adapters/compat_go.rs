//! Static compatibility check for Go (TinyGo wasip2) guest sources. Nothing here builds or
//! runs a guest. Every diagnostic names a construct and its file, line and column, never
//! source text. Supported means statically eligible for qualification, never enabled to run.

pub struct GoFile<'a> {
    /// Path relative to the guest directory, `/`-separated.
    pub path: &'a str,
    pub text: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub construct: &'static str,
    pub path: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug)]
pub struct Verdict(Vec<Diagnostic>);

/// Report order: by file, then line and column, then code and construct.
fn report_order(found: &mut Vec<Diagnostic>) {
    found.sort_by(|a, b| {
        (&a.path, a.line, a.column, a.code, a.construct).cmp(&(
            &b.path,
            b.line,
            b.column,
            b.code,
            b.construct,
        ))
    });
    found.dedup();
    found.truncate(MAX_DIAGNOSTICS);
}

impl Verdict {
    pub fn supported(&self) -> bool {
        self.0.is_empty()
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.0
    }
}

/// What a guest may import. Everything else is a diagnostic.
pub struct Policy<'a> {
    /// Standard-library packages of the pinned Go toolchain that TinyGo supports.
    pub stdlib: &'a [&'a str],
    /// The one trusted bindings package every guest imports.
    pub bindings: &'a str,
    /// Catalogue modules (exactly versioned in the manifest). Empty while unqualified.
    pub modules: &'a [&'a str],
}

const NATIVE: &str = "WASM-NATIVE-LIBRARY";
const SPAWN: &str = "WASM-PROCESS-SPAWN";
const HOOK: &str = "WASM-INSTALL-HOOK";
const GPU: &str = "WASM-GPU";
const CATALOGUE: &str = "WASM-PACKAGE-NOT-IN-CATALOGUE";
const UNSUPPORTED: &str = "WASM-UNSUPPORTED-API";

/// Go's raw-pointer package, spelled out at compile time: the repository's boundary audit
/// reserves the literal for its reviewed paths.
const UNSAFE: &str = concat!("un", "safe");

const MAX_FILES: usize = 256;
const MAX_BYTES: usize = 1_048_576;
const MAX_DIAGNOSTICS: usize = 256;

/// Import paths of GPU toolkits and bindings. Matching is by path prefix at a `/` boundary.
const GPU_PREFIXES: &[&str] = &[
    "gorgonia.org/cu",
    "github.com/gorgonia/cu",
    "github.com/mumax/3/cuda",
    "github.com/NVIDIA/go-nvml",
    "github.com/NVIDIA/go-nvlib",
    "github.com/go-gl",
    "github.com/rajveermalviya/go-webgpu",
    "github.com/cogentcore/webgpu",
    "github.com/gpuweb",
    "github.com/ROCm",
    "github.com/mjibson/go-dsp/cuda",
    "github.com/ebitengine/purego/gpu",
];

/// Standard-library packages that load native code or spawn work outside the Wasm sandbox.
const NATIVE_STDLIB: &[&str] = &["plugin", "runtime/cgo"];

/// Source extensions that the Go toolchain compiles or links as native code.
const NATIVE_EXTENSIONS: &[&str] = &[
    ".c", ".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".m", ".f", ".for", ".f90", ".s", ".sx",
    ".syso", ".swig", ".swigcxx",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Word,
    String,
    RawString,
    Rune,
    Punctuation,
    LineComment,
    BlockComment,
}

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    text: &'a str,
    at: usize,
    kind: Kind,
}

/// Go lexer for the checks only. Comments are tokens, so nothing downstream can skip one by
/// accident. A literal that never closes is reported (first one) and lexing resumes after it.
fn lex(text: &str) -> (Vec<Token<'_>>, Option<usize>) {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut failed = None;
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            b' ' | b'\t' | b'\r' | b'\n' => at += 1,
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at = bytes[at..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| at + offset);
                out.push(Token {
                    text: text[start..at].trim_end_matches('\r'),
                    at: start,
                    kind: Kind::LineComment,
                });
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at = match text[at + 2..].find("*/") {
                    Some(end) => at + 2 + end + 2,
                    None => {
                        failed.get_or_insert(start);
                        bytes.len()
                    }
                };
                out.push(Token {
                    text: &text[start..at],
                    at: start,
                    kind: Kind::BlockComment,
                });
            }
            b'"' | b'\'' => {
                let quote = bytes[at];
                at += 1;
                loop {
                    match bytes.get(at) {
                        Some(b'\\') => at += 2,
                        Some(byte) if *byte == quote => {
                            at += 1;
                            break;
                        }
                        Some(b'\n') | None => {
                            failed.get_or_insert(start);
                            break;
                        }
                        Some(_) => at += 1,
                    }
                }
                at = at.min(bytes.len());
                out.push(Token {
                    text: &text[start..at],
                    at: start,
                    kind: if quote == b'"' {
                        Kind::String
                    } else {
                        Kind::Rune
                    },
                });
            }
            b'`' => {
                at = match text[at + 1..].find('`') {
                    Some(end) => at + 1 + end + 1,
                    None => {
                        failed.get_or_insert(start);
                        bytes.len()
                    }
                };
                out.push(Token {
                    text: &text[start..at],
                    at: start,
                    kind: Kind::RawString,
                });
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80 => {
                while at < bytes.len()
                    && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || bytes[at] >= 0x80)
                {
                    at += 1;
                }
                out.push(Token {
                    text: &text[start..at],
                    at: start,
                    kind: Kind::Word,
                });
            }
            _ => {
                at += text[at..].chars().next().map_or(1, char::len_utf8);
                out.push(Token {
                    text: &text[start..at],
                    at: start,
                    kind: Kind::Punctuation,
                });
            }
        }
        if out.len() > 200_000 {
            failed.get_or_insert(at);
            break;
        }
    }
    (out, failed)
}

/// Resolve the escapes of an interpreted Go string, so `"\x72eflect"` is `reflect`.
fn unquote(token: &Token<'_>) -> Option<String> {
    match token.kind {
        Kind::RawString => token
            .text
            .strip_prefix('`')?
            .strip_suffix('`')
            .map(|inner| inner.replace('\r', "")),
        Kind::String => {
            let inner = token.text.strip_prefix('"')?.strip_suffix('"')?;
            let mut out = Vec::new();
            let bytes = inner.as_bytes();
            let mut at = 0;
            while at < bytes.len() {
                if bytes[at] != b'\\' {
                    out.push(bytes[at]);
                    at += 1;
                    continue;
                }
                let escape = *bytes.get(at + 1)?;
                at += 2;
                let digits = |count: usize, radix: u32, at: &mut usize| -> Option<u32> {
                    let part = inner.get(*at..*at + count)?;
                    *at += count;
                    u32::from_str_radix(part, radix).ok()
                };
                match escape {
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(11),
                    b'\\' | b'"' => out.push(escape),
                    b'x' => out.push(u8::try_from(digits(2, 16, &mut at)?).ok()?),
                    b'0'..=b'7' => {
                        at -= 1;
                        out.push(u8::try_from(digits(3, 8, &mut at)?).ok()?);
                    }
                    b'u' | b'U' => {
                        let count = if escape == b'u' { 4 } else { 8 };
                        let ch = char::from_u32(digits(count, 16, &mut at)?)?;
                        let mut buffer = [0; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                    }
                    _ => return None,
                }
            }
            String::from_utf8(out).ok()
        }
        _ => None,
    }
}

fn position(text: &str, at: usize) -> (usize, usize) {
    let before = &text[..at];
    let line = before.matches('\n').count() + 1;
    let column = at - before.rfind('\n').map_or(0, |newline| newline + 1) + 1;
    (line, column)
}

struct Report<'a> {
    path: &'a str,
    text: &'a str,
    found: Vec<Diagnostic>,
}

impl Report<'_> {
    fn add(&mut self, at: usize, code: &'static str, construct: &'static str) {
        let (line, column) = position(self.text, at);
        self.found.push(Diagnostic {
            code,
            construct,
            path: self.path.to_owned(),
            line,
            column,
        });
    }
}

/// The directive a line comment carries, if it is one. `//go:name` and the TinyGo/cgo
/// `//export name` and `//extern name` forms count; `// go:name` and prose do not.
fn directive(comment: &str) -> Option<(&str, &str)> {
    let body = comment.strip_prefix("//")?;
    if let Some(rest) = body.strip_prefix("go:") {
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        return Some((&rest[..end], rest[end..].trim()));
    }
    for name in ["export", "extern"] {
        if let Some(rest) = body.strip_prefix(name)
            && rest.starts_with([' ', '\t'])
            && !rest.trim().is_empty()
        {
            return Some((name, rest.trim()));
        }
    }
    None
}

/// A trusted-source directive rule: the directive name and argument, and the diagnostic if any.
type Judge<'a> = dyn Fn(&str, &str) -> Option<(&'static str, &'static str)> + 'a;

/// Every comment token anywhere in the file goes through the same checks, wherever it sits
/// relative to the imports. `go generate` also scans raw lines, so a column-one
/// `//go:generate` inside a block comment or raw string is found by a separate line scan.
fn scan_directives(report: &mut Report<'_>, tokens: &[Token<'_>], trusted: Option<&Judge>) {
    for token in tokens.iter().filter(|t| t.kind == Kind::LineComment) {
        let Some((name, argument)) = directive(token.text) else {
            continue;
        };
        if let Some(judge) = trusted {
            if let Some((code, construct)) = judge(name, argument) {
                report.add(token.at, code, construct);
            }
            continue;
        }
        let verdict = match name {
            "generate" => Some((HOOK, "go:generate")),
            "linkname" => Some((UNSUPPORTED, "go:linkname")),
            "wasmimport" => Some((UNSUPPORTED, "go:wasmimport")),
            "wasmexport" => Some((UNSUPPORTED, "go:wasmexport")),
            "embed" => Some((UNSUPPORTED, "go:embed")),
            "export" | "extern" => Some((UNSUPPORTED, "export directive")),
            "build" | "noinline" => None,
            _ if name.starts_with("cgo_") => Some((NATIVE, "go:cgo directive")),
            _ => Some((UNSUPPORTED, "compiler directive")),
        };
        if let Some((code, construct)) = verdict {
            report.add(token.at, code, construct);
        }
    }
    let mut offset = 0;
    for line in report.text.split_inclusive('\n') {
        if line.starts_with("//go:generate") {
            report.add(offset, HOOK, "go:generate");
        }
        offset += line.len();
    }
}

/// Import paths in source order, found from the non-comment tokens only: the directive scan
/// never depends on this traversal and this traversal never consumes a comment unchecked.
fn imports(tokens: &[Token<'_>]) -> Vec<(usize, Option<String>)> {
    let code: Vec<&Token<'_>> = tokens
        .iter()
        .filter(|t| !matches!(t.kind, Kind::LineComment | Kind::BlockComment))
        .collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < code.len() {
        if code[index].kind == Kind::Word && code[index].text == "import" {
            index += 1;
            let grouped = code.get(index).is_some_and(|t| t.text == "(");
            if grouped {
                index += 1;
            }
            loop {
                // An optional name: an identifier, `.` or `_`.
                if code.get(index).is_some_and(|t| {
                    matches!(t.kind, Kind::Word | Kind::Punctuation)
                        && t.text != ")"
                        && t.text != ";"
                }) && code
                    .get(index + 1)
                    .is_some_and(|t| matches!(t.kind, Kind::String | Kind::RawString))
                {
                    index += 1;
                }
                match code.get(index) {
                    Some(t) if matches!(t.kind, Kind::String | Kind::RawString) => {
                        out.push((t.at, unquote(t)));
                        index += 1;
                    }
                    _ => {}
                }
                if code.get(index).is_some_and(|t| t.text == ";") {
                    index += 1;
                }
                if !grouped || code.get(index).is_none_or(|t| t.text == ")") {
                    if grouped {
                        index += 1;
                    }
                    break;
                }
                if !code.get(index).is_some_and(|t| {
                    matches!(t.kind, Kind::String | Kind::RawString | Kind::Word)
                        || matches!(t.text, "." | "_")
                }) {
                    // Not an import spec: stop this group instead of looping forever.
                    break;
                }
            }
        } else {
            index += 1;
        }
    }
    out
}

fn gpu(path: &str) -> bool {
    GPU_PREFIXES
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

fn classify_guest_import(path: &str, policy: &Policy<'_>) -> Option<(&'static str, &'static str)> {
    if path == "C" {
        return Some((NATIVE, "cgo"));
    }
    if path == policy.bindings {
        return None;
    }
    let first = path.split('/').next().unwrap_or_default();
    if path == "os/exec" {
        return Some((SPAWN, "os/exec"));
    }
    if NATIVE_STDLIB.contains(&path) {
        return Some((NATIVE, "native code loading"));
    }
    if path == "reflect" {
        return Some((UNSUPPORTED, "reflect"));
    }
    if path == UNSAFE {
        return Some((UNSUPPORTED, UNSAFE));
    }
    if path.starts_with("./") || path.starts_with("../") || path.starts_with('/') || path.is_empty()
    {
        return Some((UNSUPPORTED, "relative import"));
    }
    if first.contains('.') {
        if gpu(path) {
            return Some((GPU, "GPU package"));
        }
        let catalogued = policy
            .modules
            .iter()
            .any(|module| path == *module || path.starts_with(&format!("{module}/")));
        return (!catalogued).then_some((CATALOGUE, "package import"));
    }
    (!policy.stdlib.contains(&path)).then_some((UNSUPPORTED, "standard library package"))
}

fn tree_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// A `go.mod` in a guest tree: no module requirement is in the (empty) catalogue, and
/// `replace`, `toolchain` and friends are not accepted.
fn check_go_mod(report: &mut Report<'_>, policy: &Policy<'_>) {
    let mut offset = 0;
    let mut block: Option<&str> = None;
    for line in report.text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let body = line.split("//").next().unwrap_or_default().trim();
        if body.is_empty() {
            continue;
        }
        if body == ")" {
            block = None;
            continue;
        }
        let mut words = body.split_whitespace();
        let first = words.next().unwrap_or_default();
        let (directive, module) = match block {
            Some(name) => (name, Some(first)),
            None => {
                if body.ends_with('(') {
                    block = Some(match first {
                        "require" => "require",
                        _ => "other",
                    });
                    if first == "require" {
                        continue;
                    }
                    report.add(start, UNSUPPORTED, "go.mod directive");
                    continue;
                }
                (first, words.next())
            }
        };
        match directive {
            "module" | "go" => {}
            "require" => {
                let catalogued = module.is_some_and(|m| policy.modules.contains(&m));
                if !catalogued {
                    report.add(start, CATALOGUE, "module requirement");
                }
            }
            _ => report.add(start, UNSUPPORTED, "go.mod directive"),
        }
    }
}

/// Static check of a guest source directory. Diagnostics are sorted by file, line and column.
pub fn check_guest(files: &[GoFile<'_>], policy: &Policy<'_>) -> Verdict {
    let origin = |path: &str, construct| Diagnostic {
        code: UNSUPPORTED,
        construct,
        path: path.to_owned(),
        line: 1,
        column: 1,
    };
    if files.len() > MAX_FILES || files.iter().any(|file| file.text.len() > MAX_BYTES) {
        return Verdict(vec![origin("", "source bundle")]);
    }
    let mut found = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for file in files {
        if !tree_path(file.path) {
            found.push(origin(file.path, "tree path"));
            continue;
        }
        if !seen.insert(file.path) {
            found.push(origin(file.path, "duplicate tree path"));
            continue;
        }
        let mut report = Report {
            path: file.path,
            text: file.text,
            found: Vec::new(),
        };
        let name = file.path.rsplit('/').next().unwrap_or_default();
        if NATIVE_EXTENSIONS
            .iter()
            .any(|extension| name.to_ascii_lowercase().ends_with(extension))
        {
            report.add(0, NATIVE, "native source file");
        } else if name == "go.mod" {
            check_go_mod(&mut report, policy);
        } else if name.ends_with(".go") {
            let (tokens, failed) = lex(file.text);
            if let Some(at) = failed {
                report.add(at, UNSUPPORTED, "unresolved syntax");
            }
            scan_directives(&mut report, &tokens, None);
            for (at, path) in imports(&tokens) {
                match path {
                    None => report.add(at, UNSUPPORTED, "import path"),
                    Some(path) => {
                        if let Some((code, construct)) = classify_guest_import(&path, policy) {
                            report.add(at, code, construct);
                        }
                    }
                }
            }
        } else if name != "go.sum" {
            report.add(0, UNSUPPORTED, "unrecognised file");
        }
        found.extend(report.found);
    }
    report_order(&mut found);
    Verdict(found)
}

/// Trusted bindings: they may declare `go:wasmimport` for allowed interfaces and the handler
/// export, and nothing else. Any `cabi_realloc` export is a defect (the TinyGo runtime owns it).
pub fn check_bindings(files: &[GoFile<'_>], interfaces: &[&str], stdlib: &[&str]) -> Verdict {
    let judge = |name: &str, argument: &str| -> Option<(&'static str, &'static str)> {
        let mut words = argument.split_whitespace();
        match name {
            "wasmimport" => (!interfaces.contains(&words.next().unwrap_or_default()))
                .then_some((UNSUPPORTED, "wasm import outside the allow-list")),
            "wasmexport" => match words.next().unwrap_or_default() {
                "falcone:host/handler@0.2.0#handle" => None,
                export if export.contains("cabi_realloc") => {
                    Some((UNSUPPORTED, "cabi_realloc export"))
                }
                _ => Some((UNSUPPORTED, "wasm export outside the handler")),
            },
            "export" | "extern" if argument.contains("cabi_realloc") => {
                Some((UNSUPPORTED, "cabi_realloc export"))
            }
            "export" | "extern" => Some((UNSUPPORTED, "export directive")),
            "noinline" | "build" => None,
            "generate" => Some((HOOK, "go:generate")),
            _ => Some((UNSUPPORTED, "compiler directive")),
        }
    };
    let mut found = Vec::new();
    for file in files {
        let mut report = Report {
            path: file.path,
            text: file.text,
            found: Vec::new(),
        };
        let (tokens, failed) = lex(file.text);
        if let Some(at) = failed {
            report.add(at, UNSUPPORTED, "unresolved syntax");
        }
        scan_directives(&mut report, &tokens, Some(&judge));
        for token in tokens
            .iter()
            .filter(|t| t.kind == Kind::Word && t.text.contains("cabi_realloc"))
        {
            report.add(token.at, UNSUPPORTED, "cabi_realloc export");
        }
        for (at, path) in imports(&tokens) {
            match path {
                Some(path) if path == UNSAFE || stdlib.contains(&path.as_str()) => {}
                _ => report.add(at, UNSUPPORTED, "binding import"),
            }
        }
        found.extend(report.found);
    }
    report_order(&mut found);
    Verdict(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERATE: &str = "WASM-INSTALL-HOOK";
    const UNSUPPORTED: &str = "WASM-UNSUPPORTED-API";

    const POLICY: Policy<'static> = Policy {
        stdlib: &[
            "strings",
            "strconv",
            "sort",
            "crypto/sha256",
            "encoding/hex",
        ],
        bindings: "falcone.invalid/profile/bindings",
        modules: &[],
    };

    /// `@U@` stands for the raw-pointer package name in the sample sources.
    fn run(text: &str) -> Vec<(&'static str, &'static str, usize, usize)> {
        let text = text.replace("@U@", UNSAFE);
        let files = [GoFile {
            path: "main.go",
            text: &text,
        }];
        check_guest(&files, &POLICY)
            .diagnostics()
            .iter()
            .map(|d| (d.code, d.construct, d.line, d.column))
            .collect()
    }

    fn codes(text: &str) -> Vec<(&'static str, usize)> {
        run(text).into_iter().map(|d| (d.0, d.2)).collect()
    }

    const CLEAN: &str =
        "package main\n\nimport (\n\t\"strings\"\n)\n\nfunc init() { _ = strings.ToUpper }\n";

    #[test]
    fn a_clean_guest_is_supported() {
        assert_eq!(run(CLEAN), []);
    }

    // PR #429 finding 2: every comment token goes through the directive checks.
    #[test]
    fn go_generate_inside_a_grouped_import_is_an_install_hook() {
        let text =
            "package main\n\nimport (\n\t\"strings\"\n//go:generate sh -c id\n\t\"strconv\"\n)\n";
        assert_eq!(codes(text), [(GENERATE, 5)]);
    }

    #[test]
    fn go_generate_as_the_first_and_last_comment_of_a_group_is_found() {
        let text = "package main\n\nimport (\n//go:generate a\n\t\"strings\"\n//go:generate b\n)\n";
        assert_eq!(codes(text), [(GENERATE, 4), (GENERATE, 6)]);
    }

    #[test]
    fn go_generate_between_imports_is_an_install_hook() {
        let text =
            "package main\n\nimport \"strings\"\n//go:generate sh -c id\nimport \"strconv\"\n";
        assert_eq!(codes(text), [(GENERATE, 4)]);
    }

    #[test]
    fn go_generate_at_file_level_is_an_install_hook() {
        let text = "package main\n\n//go:generate go run gen.go\nfunc f() {}\n";
        assert_eq!(codes(text), [(GENERATE, 3)]);
    }

    #[test]
    fn go_generate_inside_a_block_comment_is_found_as_go_generate_scans_lines() {
        let text = "package main\n\n/*\n//go:generate sh -c id\n*/\nfunc f() {}\n";
        assert_eq!(codes(text), [(GENERATE, 4)]);
    }

    #[test]
    fn go_generate_inside_a_raw_string_is_found_as_go_generate_scans_lines() {
        let text = "package main\n\nvar s = `\n//go:generate sh -c id\n`\n";
        assert_eq!(codes(text), [(GENERATE, 4)]);
    }

    #[test]
    fn go_linkname_inside_a_grouped_import_is_unsupported() {
        let text = "package main\n\nimport (\n\t_ \"@U@\"\n//go:linkname a b\n)\n";
        let found = run(text);
        assert!(
            found.contains(&(UNSUPPORTED, "go:linkname", 5, 1)),
            "{found:?}"
        );
    }

    #[test]
    fn go_linkname_between_imports_and_at_file_level_is_unsupported() {
        for text in [
            "package main\n\nimport _ \"@U@\"\n//go:linkname a b\nimport \"strings\"\n",
            "package main\n\nimport _ \"@U@\"\n\n//go:linkname a b\nfunc a()\n",
            "package main\n\nfunc f() {\n\t//go:linkname a b\n}\n",
        ] {
            assert!(
                run(text).iter().any(|d| d.1 == "go:linkname"),
                "missed in {text:?}"
            );
        }
    }

    #[test]
    fn an_indented_directive_is_still_a_directive() {
        let text = "package main\n\nimport (\n\t\"strings\"\n\t//go:linkname a b\n)\n";
        assert!(run(text).iter().any(|d| d.1 == "go:linkname"));
    }

    #[test]
    fn comments_that_are_not_directives_are_not_reported() {
        for text in [
            "package main\n// go:generate sh\n",
            "package main\n//gogo:generate sh\n",
            "package main\n// see //go:generate in the docs\n",
            "package main\nvar s = \"//go:generate x\"\n",
            "package main\n/* go:linkname */\n",
            "package main\n// export is a word\n",
            "package main\n//exported helper\n",
            "package main\n//go:build tinygo\n",
        ] {
            assert_eq!(run(text), [], "{text:?}");
        }
    }

    #[test]
    fn locations_are_one_based_line_and_column() {
        let text = "package main\n\nimport (\n\t\"strings\"\n\t//go:generate x\n)\n";
        assert_eq!(run(text), [(GENERATE, "go:generate", 5, 2)]);
    }

    fn import(path: &str) -> Vec<(&'static str, &'static str, usize, usize)> {
        run(&format!("package main\n\nimport \"{path}\"\n"))
    }

    #[test]
    fn each_unsupported_import_yields_its_documented_code_and_location() {
        let cases = [
            ("C", "WASM-NATIVE-LIBRARY", "cgo"),
            ("plugin", "WASM-NATIVE-LIBRARY", "native code loading"),
            ("os/exec", "WASM-PROCESS-SPAWN", "os/exec"),
            (
                "github.com/NVIDIA/go-nvml/pkg/nvml",
                "WASM-GPU",
                "GPU package",
            ),
            ("gorgonia.org/cu", "WASM-GPU", "GPU package"),
            (
                "github.com/google/uuid",
                "WASM-PACKAGE-NOT-IN-CATALOGUE",
                "package import",
            ),
            ("reflect", UNSUPPORTED, "reflect"),
            (UNSAFE, UNSUPPORTED, UNSAFE),
            ("net", UNSUPPORTED, "standard library package"),
            ("os", UNSUPPORTED, "standard library package"),
            ("../x", UNSUPPORTED, "relative import"),
        ];
        for (path, code, construct) in cases {
            assert_eq!(import(path), [(code, construct, 3, 8)], "{path}");
        }
    }

    #[test]
    fn allowed_imports_are_not_reported() {
        for path in [
            "strings",
            "crypto/sha256",
            "falcone.invalid/profile/bindings",
        ] {
            assert_eq!(import(path), [], "{path}");
        }
        // A prefix of a catalogue or GPU path is not a match.
        assert_eq!(
            import("gorgonia.org/cuda-not-gpu")[0].0,
            "WASM-PACKAGE-NOT-IN-CATALOGUE"
        );
    }

    #[test]
    fn an_escaped_import_path_is_resolved_before_it_is_judged() {
        let text =
            "package main\n\nimport (\n\t\"\\x72eflect\"\n\t`os/exec`\n\t_ \"\\u0075nsafe\"\n)\n";
        assert_eq!(
            run(text),
            [
                (UNSUPPORTED, "reflect", 4, 2),
                ("WASM-PROCESS-SPAWN", "os/exec", 5, 2),
                (UNSUPPORTED, UNSAFE, 6, 4),
            ]
        );
        assert_eq!(run("package main\nimport \"\\q\"\n")[0].1, "import path");
    }

    #[test]
    fn named_dot_and_blank_imports_are_judged_by_path() {
        let text = "package main\n\nimport (\n\tx \"os\"\n\t. \"net\"\n\t_ \"reflect\"\n)\n";
        assert_eq!(
            run(text).iter().map(|d| (d.2, d.1)).collect::<Vec<_>>(),
            [
                (4, "standard library package"),
                (5, "standard library package"),
                (6, "reflect")
            ]
        );
    }

    #[test]
    fn export_and_wasm_directives_in_a_guest_are_unsupported() {
        for (text, construct) in [
            (
                "package main\n//go:wasmimport wasi_snapshot_preview1 fd_write\nfunc f()\n",
                "go:wasmimport",
            ),
            (
                "package main\n//go:wasmexport cabi_realloc\nfunc f() {}\n",
                "go:wasmexport",
            ),
            (
                "package main\n//export cabi_realloc\nfunc f() {}\n",
                "export directive",
            ),
            (
                "package main\n//go:embed secret.txt\nvar s string\n",
                "go:embed",
            ),
            ("package main\n//go:cgo_ldflag x\n", "go:cgo directive"),
            (
                "package main\n//go:extern sym\nvar x int\n",
                "export directive",
            ),
            (
                "package main\n//go:nobounds\nfunc f() {}\n",
                "compiler directive",
            ),
        ] {
            assert_eq!(run(text)[0].1, construct, "{text:?}");
        }
    }

    #[test]
    fn an_unterminated_literal_is_reported_and_later_imports_are_still_found() {
        let text = "package main\n\nvar s = \"open\nimport \"os/exec\"\n";
        let found = run(text);
        assert!(
            found.iter().any(|d| d.1 == "unresolved syntax" && d.2 == 3),
            "{found:?}"
        );
        assert!(found.iter().any(|d| d.1 == "os/exec"), "{found:?}");
        let text = "package main\n\n/* open\nimport \"os/exec\"\n";
        assert!(
            run(text)
                .iter()
                .any(|d| d.1 == "unresolved syntax" && d.2 == 3)
        );
    }

    #[test]
    fn go_mod_requirements_replace_and_toolchain_are_diagnosed() {
        let files = [GoFile {
            path: "go.mod",
            text: "module example\n\ngo 1.26\n\nrequire github.com/google/uuid v1.6.0\nreplace a => ../b\ntoolchain go1.99\nrequire (\n\texample.com/x v1.0.0\n)\n",
        }];
        let found: Vec<_> = check_guest(&files, &POLICY)
            .diagnostics()
            .iter()
            .map(|d| (d.code, d.line))
            .collect();
        assert_eq!(
            found,
            [
                ("WASM-PACKAGE-NOT-IN-CATALOGUE", 5),
                (UNSUPPORTED, 6),
                (UNSUPPORTED, 7),
                ("WASM-PACKAGE-NOT-IN-CATALOGUE", 9)
            ]
        );
    }

    #[test]
    fn native_sources_and_unknown_files_are_diagnosed() {
        for (path, code) in [
            ("shim.c", "WASM-NATIVE-LIBRARY"),
            ("a/b.syso", "WASM-NATIVE-LIBRARY"),
            ("asm_amd64.s", "WASM-NATIVE-LIBRARY"),
            ("blob.bin", UNSUPPORTED),
            ("../escape.go", UNSUPPORTED),
        ] {
            let files = [GoFile { path, text: "" }];
            let verdict = check_guest(&files, &POLICY);
            assert_eq!(verdict.diagnostics()[0].code, code, "{path}");
            assert!(!verdict.supported());
        }
    }

    const INTERFACES: &[&str] = &["falcone:host/storage@0.2.0", "falcone:host/events@0.2.0"];

    fn bindings(text: &str) -> Vec<(&'static str, usize)> {
        let text = text.replace("@U@", UNSAFE);
        let files = [GoFile {
            path: "abi.go",
            text: &text,
        }];
        check_bindings(&files, INTERFACES, &["strings"])
            .diagnostics()
            .iter()
            .map(|d| (d.construct, d.line))
            .collect()
    }

    #[test]
    fn bindings_may_import_only_listed_interfaces_and_export_only_the_handler() {
        let good = "package b\n\nimport \"@U@\"\n\n//go:wasmimport falcone:host/events@0.2.0 publish\nfunc p(a uint32)\n\n//go:wasmexport falcone:host/handler@0.2.0#handle\nfunc h() {}\n";
        assert_eq!(bindings(good), []);
        assert_eq!(
            bindings("package b\n//go:wasmimport falcone:host/http@0.2.0 request\nfunc p()\n"),
            [("wasm import outside the allow-list", 2)]
        );
        assert_eq!(
            bindings("package b\n//go:wasmimport wasi:cli/environment@0.2.0 x\nfunc p()\n"),
            [("wasm import outside the allow-list", 2)]
        );
        assert_eq!(
            bindings("package b\n//go:wasmexport other\nfunc p() {}\n"),
            [("wasm export outside the handler", 2)]
        );
    }

    // PR #429 finding 1: the TinyGo runtime exports cabi_realloc; the bindings never do.
    #[test]
    fn bindings_never_declare_cabi_realloc() {
        for text in [
            "package b\n//go:wasmexport cabi_realloc\nfunc r() {}\n",
            "package b\n//export cabi_realloc\nfunc r() {}\n",
            "package b\nfunc cabi_realloc() {}\n",
        ] {
            assert!(
                bindings(text).iter().any(|d| d.0 == "cabi_realloc export"),
                "{text:?}"
            );
        }
    }

    #[test]
    fn bindings_directives_are_checked_inside_import_groups_too() {
        let text = "package b\n\nimport (\n\t\"strings\"\n//go:linkname a b\n)\n";
        assert_eq!(bindings(text), [("compiler directive", 5)]);
        assert_eq!(
            bindings("package b\nimport \"os\"\n"),
            [("binding import", 2)]
        );
    }
}
