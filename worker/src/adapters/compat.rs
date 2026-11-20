//! A deliberately small, closed JavaScript grammar, not a blacklist-based JS sandbox.
//! Unknown syntax, ambient names, property calls, computed access and dependencies are
//! refused. Supported means statically eligible for qualification, never enabled to run.

use serde_json::Value;

pub struct Source<'a> {
    pub text: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub construct: &'static str,
    /// Zero-based source index; sources.len() identifies package.json. No input filename.
    pub source: usize,
    pub line: usize,
    pub column: usize,
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

const UNSUPPORTED: &str = "WASM-UNSUPPORTED-API";

// Node's bare builtin specifiers (including subpaths such as fs/promises) are APIs,
// not third-party catalogue packages. Prefix-only builtins are caught by node: below.
fn bare_node_builtin(module: &str) -> bool {
    matches!(
        module.split('/').next().unwrap_or_default(),
        "assert"
            | "async_hooks"
            | "buffer"
            | "child_process"
            | "cluster"
            | "console"
            | "constants"
            | "crypto"
            | "dgram"
            | "diagnostics_channel"
            | "dns"
            | "domain"
            | "events"
            | "fs"
            | "http"
            | "http2"
            | "https"
            | "inspector"
            | "module"
            | "net"
            | "os"
            | "path"
            | "perf_hooks"
            | "process"
            | "punycode"
            | "querystring"
            | "readline"
            | "repl"
            | "stream"
            | "string_decoder"
            | "sys"
            | "timers"
            | "tls"
            | "trace_events"
            | "tty"
            | "url"
            | "util"
            | "v8"
            | "vm"
            | "wasi"
            | "worker_threads"
            | "zlib"
    )
}

fn diagnostic(
    text: &str,
    source: usize,
    at: usize,
    code: &'static str,
    construct: &'static str,
) -> Diagnostic {
    let (mut line, mut column, mut after_cr) = (1, 1, false);
    for character in text[..at].chars() {
        if character == '\n' && after_cr {
            after_cr = false;
            continue;
        }
        if line_separator(character) {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
        after_cr = character == '\r';
    }
    Diagnostic {
        code,
        construct,
        source,
        line,
        column,
    }
}

fn line_separator(character: char) -> bool {
    matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    String,
    Number,
    Punctuation,
    Opaque,
}

#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    at: usize,
    kind: Kind,
}

/// Escapes, templates and regex literals are unresolved rather than interpreted heuristically.
fn lex(text: &str) -> Result<Vec<Token<'_>>, usize> {
    match lex_with(text, false) {
        (tokens, None) => Ok(tokens),
        (_, Some(at)) => Err(at),
    }
}

/// Index of the quote closing the string opened at `open`: backslash escapes and line
/// continuations are skipped, and an unescaped line break means the string never closes.
fn string_end(bytes: &[u8], open: usize) -> Option<usize> {
    let quote = bytes[open];
    let mut at = open + 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => {
                at += if bytes[at + 1..].starts_with(b"\r\n") {
                    3
                } else {
                    2
                };
            }
            b'\n' | b'\r' => return None,
            byte if byte == quote => return Some(at),
            _ => at += 1,
        }
    }
    None
}

/// Index of the first line break at or after `from`, or the end of the text.
fn line_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|byte| matches!(byte, b'\n' | b'\r'))
        .map_or(bytes.len(), |offset| from + offset)
}

/// With `recover`, a construct that cannot be lexed is recorded (first one only) and lexing
/// resumes after it, so later imports and feature uses are still found. Recovered streams are
/// only used to report diagnostics; they never make a source eligible.
fn lex_with(text: &str, recover: bool) -> (Vec<Token<'_>>, Option<usize>) {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut out = Vec::new();
    let mut failed: Option<usize> = None;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            b' ' | b'\r' | b'\n' | b'\t' => {
                at += 1;
                continue;
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while let Some(character) = text[at..].chars().next() {
                    if line_separator(character) {
                        break;
                    }
                    at += character.len_utf8();
                }
                continue;
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                let Some(end) = text[at + 2..].find("*/") else {
                    if recover {
                        failed.get_or_insert(at);
                        at += 2;
                        continue;
                    }
                    return (out, Some(at));
                };
                at += end + 4;
                continue;
            }
            b'\'' | b'"' => {
                let quote = bytes[at];
                at += 1;
                while at < bytes.len() && bytes[at] != quote {
                    if matches!(bytes[at], b'\\' | b'\n' | b'\r') {
                        break;
                    }
                    at += 1;
                }
                if at == bytes.len() || bytes[at] != quote {
                    if recover {
                        // Resume at the string's real end (an escape-aware scan on the same
                        // line), or at the end of the line when it never closes, so a quote
                        // inside the string cannot open a phantom string that swallows a
                        // later import on the same line.
                        failed.get_or_insert(start);
                        at = string_end(bytes, start)
                            .map_or_else(|| line_end(bytes, start), |closing| closing + 1);
                        out.push(Token {
                            text: &text[start..at],
                            at: start,
                            kind: Kind::Opaque,
                        });
                        continue;
                    }
                    return (out, Some(start));
                }
                out.push(Token {
                    text: &text[start + 1..at],
                    at: start,
                    kind: Kind::String,
                });
                at += 1;
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => {
                at += 1;
                while at < bytes.len()
                    && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'$'))
                {
                    at += 1;
                }
                out.push(Token {
                    text: &text[start..at],
                    at: start,
                    kind: Kind::Word,
                });
            }
            b'0'..=b'9' => {
                at += 1;
                while at < bytes.len()
                    && (bytes[at].is_ascii_digit()
                        || matches!(bytes[at], b'.' | b'e' | b'E' | b'+' | b'-'))
                {
                    at += 1;
                }
                let token = &text[start..at];
                out.push(Token {
                    text: token,
                    at: start,
                    kind: if serde_json::from_str::<Value>(token)
                        .is_ok_and(|value| value.is_number())
                    {
                        Kind::Number
                    } else {
                        Kind::Opaque
                    },
                });
            }
            // Tokenize bindings so unsupported imports still receive their specific
            // diagnostic. Assignment remains outside the eligible main grammar.
            b'(' | b')' | b'{' | b'}' | b'[' | b']' | b'.' | b',' | b':' | b';' | b'-' | b'=' => {
                at += 1;
                out.push(Token {
                    text: &text[start..at],
                    at: start,
                    kind: Kind::Punctuation,
                });
            }
            _ if text[at..].starts_with(['\u{2028}', '\u{2029}']) => {
                at += 3;
                continue;
            }
            // Preserve feature diagnostics across operators and other unknown characters.
            // Opaque tokens remain ineligible in the closed grammar below. Advance by a
            // whole character so subsequent tokens retain valid UTF-8 source locations.
            _ => {
                at += text[at..].chars().next().unwrap().len_utf8();
                out.push(Token {
                    text: &text[start..at],
                    at: start,
                    kind: Kind::Opaque,
                });
            }
        }
        if out.len() > 100_000 {
            return (out, Some(failed.unwrap_or(at)));
        }
    }
    (out, failed)
}

struct Grammar<'a> {
    source: &'a str,
    tokens: &'a [Token<'a>],
    at: usize,
    asynchronous: bool,
    context: bool,
}
impl Grammar<'_> {
    fn take(&mut self, text: &str) -> bool {
        if self
            .tokens
            .get(self.at)
            .is_some_and(|token| token.text == text && token.kind != Kind::String)
        {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, text: &str) -> Result<(), ()> {
        self.take(text).then_some(()).ok_or(())
    }
    fn same_line(&self) -> Result<(), ()> {
        let previous = self.tokens.get(self.at - 1).ok_or(())?;
        let next = self.tokens.get(self.at).ok_or(())?;
        (!self.source[previous.at..next.at]
            .chars()
            .any(line_separator))
        .then_some(())
        .ok_or(())
    }
    fn program(&mut self) -> Result<(), ()> {
        self.take("export");
        self.asynchronous = self.take("async");
        if self.asynchronous {
            self.same_line()?;
        }
        self.expect("function")?;
        self.expect("main")?;
        self.expect("(")?;
        self.expect("params")?;
        if self.take(",") {
            self.expect("context")?;
            self.context = true;
        }
        self.expect(")")?;
        self.expect("{")?;
        loop {
            if self.take("return") {
                self.same_line()?;
                self.expression(0)?;
                self.take(";");
                break;
            }
            if self.take("throw") {
                self.same_line()?;
                self.expect("new")?;
                self.expect("Error")?;
                self.arguments(0)?;
                self.take(";");
                break;
            }
            self.expect("console")?;
            self.expect(".")?;
            self.expect("log")?;
            // Object inspection, dynamic values and printf formatting have no qualified
            // legacy oracle. Only a single literal message is currently resolvable.
            self.expect("(")?;
            let message = self.tokens.get(self.at).ok_or(())?;
            if message.kind != Kind::String || message.text.contains('%') {
                return Err(());
            }
            self.at += 1;
            self.expect(")")?;
            self.expect(";")?;
        }
        self.expect("}")?;
        self.take(";");
        (self.at == self.tokens.len()).then_some(()).ok_or(())
    }
    fn expression(&mut self, depth: usize) -> Result<(), ()> {
        if depth > 64 {
            return Err(());
        }
        let token = *self.tokens.get(self.at).ok_or(())?;
        if matches!(token.kind, Kind::String | Kind::Number)
            || matches!(token.text, "true" | "false" | "null")
        {
            self.at += 1;
            return Ok(());
        }
        if self.take("-") {
            let number = self.tokens.get(self.at).ok_or(())?;
            if number.kind != Kind::Number {
                return Err(());
            }
            self.at += 1;
            return Ok(());
        }
        if self.take("params") {
            while self.take(".") {
                let key = self.tokens.get(self.at).ok_or(())?;
                if key.kind != Kind::Word
                    || matches!(key.text, "constructor" | "prototype" | "__proto__")
                {
                    return Err(());
                }
                self.at += 1;
            }
            return Ok(());
        }
        if self.context && self.take("context") {
            self.expect(".")?;
            let key = self.tokens.get(self.at).ok_or(())?;
            if key.kind != Kind::Word
                || !matches!(
                    key.text,
                    "tenant" | "workspace" | "invocation_key" | "attempt_id" | "idempotency_key"
                )
            {
                return Err(());
            }
            self.at += 1;
            return Ok(());
        }
        if self.asynchronous && self.take("await") {
            return self.expression(depth + 1);
        }
        if self.take("new") {
            self.expect("Uint8Array")?;
            return self.arguments(depth + 1);
        }
        if self.take("Promise") {
            self.expect(".")?;
            self.expect("reject")?;
            return self.arguments(depth + 1);
        }
        if self.take("crypto") {
            self.expect(".")?;
            if !self.take("getRandomValues") {
                self.expect("subtle")?;
                self.expect(".")?;
                self.expect("digest")?;
            }
            return self.arguments(depth + 1);
        }
        if self.take("storage") {
            self.expect(".")?;
            if !self.take("openObject") {
                self.expect("readObject")?;
            }
            return self.arguments(depth + 1);
        }
        let (close, object) = if self.take("{") {
            ("}", true)
        } else if self.take("[") {
            ("]", false)
        } else {
            return Err(());
        };
        if self.take(close) {
            return Ok(());
        }
        loop {
            if object {
                let key = self.tokens.get(self.at).ok_or(())?;
                if !matches!(key.kind, Kind::Word | Kind::String)
                    || matches!(key.text, "constructor" | "prototype" | "__proto__")
                {
                    return Err(());
                }
                self.at += 1;
                self.expect(":")?;
            }
            self.expression(depth + 1)?;
            if self.take(close) {
                return Ok(());
            }
            self.expect(",")?;
        }
    }
    fn arguments(&mut self, depth: usize) -> Result<(), ()> {
        if depth > 64 {
            return Err(());
        }
        self.expect("(")?;
        if self.take(")") {
            return Ok(());
        }
        loop {
            self.expression(depth + 1)?;
            if self.take(")") {
                return Ok(());
            }
            self.expect(",")?;
        }
    }
}

/// Locate root JSON keys only after serde has validated the entire manifest. Unlike the JS
/// subset lexer, JSON key decoding must accept escapes. Duplicate keys use the last span,
/// matching serde's object parsing. Script bodies are skipped and never retained here.
fn manifest_locations(text: &str) -> std::collections::BTreeMap<String, usize> {
    let bytes = text.as_bytes();
    let (mut at, mut depth) = (0, 0);
    let mut locations = std::collections::BTreeMap::new();
    while at < bytes.len() {
        match bytes[at] {
            b'{' | b'[' => {
                depth += 1;
                at += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                at += 1;
            }
            b'"' => {
                let start = at;
                at += 1;
                while bytes[at] != b'"' {
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
                at += 1;
                let mut next = at;
                while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                    next += 1;
                }
                if depth == 1
                    && bytes.get(next) == Some(&b':')
                    && let Ok(key) = serde_json::from_str::<String>(&text[start..at])
                {
                    locations.insert(key, start);
                }
            }
            _ => at += 1,
        }
    }
    locations
}

/// Package manifest checks shared by `check` and `check_tree`. `source` is the diagnostic index.
fn check_manifest(manifest: &str, source: usize, diagnostics: &mut Vec<Diagnostic>) {
    match serde_json::from_str::<Value>(manifest)
        .ok()
        .and_then(|value| value.as_object().cloned())
    {
        None => diagnostics.push(diagnostic(
            manifest,
            source,
            0,
            UNSUPPORTED,
            "package manifest",
        )),
        Some(fields) => {
            let locations = manifest_locations(manifest);
            for (key, value) in fields {
                if diagnostics.len() >= 256 {
                    break;
                }
                let at = locations.get(&key).copied().unwrap_or(0);
                match key.as_str() {
                    "scripts" if value.as_object().is_some_and(|scripts| !scripts.is_empty()) => {
                        diagnostics.push(diagnostic(
                            manifest,
                            source,
                            at,
                            "WASM-INSTALL-HOOK",
                            "lifecycle script",
                        ));
                    }
                    "dependencies"
                    | "devDependencies"
                    | "peerDependencies"
                    | "optionalDependencies"
                    | "bundledDependencies"
                    | "bundleDependencies"
                        if value
                            .as_object()
                            .is_none_or(|packages| !packages.is_empty()) =>
                    {
                        diagnostics.push(diagnostic(
                            manifest,
                            source,
                            at,
                            "WASM-PACKAGE-NOT-IN-CATALOGUE",
                            "package dependency",
                        ));
                    }
                    "name" | "version" | "description" | "license" | "type"
                        if value.is_string() => {}
                    "dependencies"
                    | "devDependencies"
                    | "peerDependencies"
                    | "optionalDependencies"
                    | "scripts"
                        if value.as_object().is_some_and(|fields| fields.is_empty()) => {}
                    _ => diagnostics.push(diagnostic(
                        manifest,
                        source,
                        at,
                        UNSUPPORTED,
                        "package manifest",
                    )),
                }
            }
        }
    }
}

/// A specifier that names a file by path rather than by package or builtin name.
fn tree_specifier(module: &str) -> bool {
    module.starts_with("./")
        || module.starts_with("../")
        || module.starts_with('/')
        || matches!(module, "." | "..")
}

/// An ambient `process.env` read, including optional chaining, computed access and
/// `globalThis.process`. A member called `process` on another object is not the ambient one.
fn process_env(tokens: &[Token<'_>], index: usize) -> bool {
    let text = |at: usize| tokens.get(at).map(|token| token.text);
    let punctuation = |at: usize, value: &str| {
        tokens
            .get(at)
            .is_some_and(|token| token.kind == Kind::Punctuation && token.text == value)
    };
    if index > 0
        && punctuation(index - 1, ".")
        && !(index > 1 && text(index - 2) == Some("globalThis"))
    {
        return false;
    }
    let dot = if text(index + 1) == Some("?") {
        index + 2
    } else {
        index + 1
    };
    (punctuation(dot, ".") && text(dot + 1) == Some("env") && tokens[dot + 1].kind == Kind::Word)
        || (punctuation(index + 1, "[")
            && tokens
                .get(index + 2)
                .is_some_and(|key| key.kind == Kind::String && key.text == "env"))
}

/// Feature and import diagnostics for one token stream. In tree mode (`relatives` is `Some`)
/// relative specifiers are collected for resolution instead of being reported as packages, and
/// `process.env` reads are diagnosed. Single-source `check` passes `None` and is unchanged.
fn scan_tokens(
    text: &str,
    source: usize,
    tokens: &[Token<'_>],
    diagnostics: &mut Vec<Diagnostic>,
    mut relatives: Option<&mut Vec<(String, usize)>>,
) {
    for (index, token) in tokens.iter().enumerate() {
        if diagnostics.len() >= 256 {
            break;
        }
        let next = |offset: usize| tokens.get(index + offset).map(|token| token.text);
        // Module names in ordinary return values and log messages are data. Only
        // import/require specifiers identify a library or an unsupported Node API.
        let module_specifier = (index > 0
            && tokens[index - 1].kind == Kind::Word
            && matches!(tokens[index - 1].text, "from" | "import"))
            || (index > 1
                && tokens[index - 1].kind == Kind::Punctuation
                && tokens[index - 1].text == "("
                && tokens[index - 2].kind == Kind::Word
                && matches!(tokens[index - 2].text, "require" | "import")
                && tokens
                    .get(index + 1)
                    .is_some_and(|close| close.kind == Kind::Punctuation && close.text == ")"));
        let feature = match (token.kind, token.text) {
            (Kind::String, "child_process" | "node:child_process") if module_specifier => {
                Some(("WASM-PROCESS-SPAWN", "child_process"))
            }
            (Kind::String, "worker_threads" | "node:worker_threads") if module_specifier => {
                Some(("WASM-PROCESS-SPAWN", "worker_threads"))
            }
            (Kind::String, "cluster" | "node:cluster") if module_specifier => {
                Some(("WASM-PROCESS-SPAWN", "cluster"))
            }
            (Kind::String, module) if module_specifier && module.ends_with(".node") => {
                Some(("WASM-NATIVE-LIBRARY", ".node"))
            }
            // Tree mode follows relative specifiers inside the supplied tree; they are never
            // third-party catalogue packages. The tree resolver diagnoses unresolved ones.
            (Kind::String, module)
                if module_specifier && relatives.is_some() && tree_specifier(module) =>
            {
                if let Some(found) = relatives.as_deref_mut() {
                    found.push((module.to_owned(), token.at));
                }
                None
            }
            (Kind::String, "node:crypto" | "crypto") if module_specifier => {
                Some((UNSUPPORTED, "node:crypto API"))
            }
            (Kind::String, module) if module_specifier && module.starts_with("node:") => {
                Some((UNSUPPORTED, "node module"))
            }
            (Kind::Word, "process") if next(1) == Some(".") && next(2) == Some("dlopen") => {
                Some(("WASM-NATIVE-LIBRARY", "process.dlopen"))
            }
            (Kind::Word, "process") if relatives.is_some() && process_env(tokens, index) => {
                Some((UNSUPPORTED, "process.env"))
            }
            (Kind::Word, "navigator") if next(1) == Some(".") && next(2) == Some("gpu") => {
                Some(("WASM-GPU", "navigator.gpu"))
            }
            (Kind::Word, "GPU" | "WebGPU") => Some(("WASM-GPU", "GPU")),
            (Kind::Word, "eval") => Some((UNSUPPORTED, "eval")),
            (Kind::Word, "Function") => Some((UNSUPPORTED, "Function")),
            (Kind::Word, "new") if next(1) == Some("Function") => {
                Some((UNSUPPORTED, "new Function"))
            }
            (Kind::Word, "require" | "import") => {
                let argument = tokens.get(index + if next(1) == Some("(") { 2 } else { 1 });
                // A leading string in an expression or argument list is not a
                // statically resolved specifier. Diagnose the whole dynamic call.
                let literal = argument.is_some_and(|argument| argument.kind == Kind::String)
                    && (next(1) != Some("(")
                        || tokens.get(index + 3).is_some_and(|close| {
                            close.kind == Kind::Punctuation && close.text == ")"
                        }));
                if !literal {
                    // Static import declarations are diagnosed at the specifier below.
                    if token.text == "import" && next(1) != Some("(") {
                        None
                    } else {
                        Some((
                            UNSUPPORTED,
                            if token.text == "require" {
                                "dynamic require"
                            } else {
                                "dynamic import"
                            },
                        ))
                    }
                } else {
                    None
                }
            }
            (Kind::String, module) if module_specifier => {
                if bare_node_builtin(module) {
                    Some((UNSUPPORTED, "node module"))
                } else {
                    // No dependency catalogue is qualified, including local modules.
                    Some(("WASM-PACKAGE-NOT-IN-CATALOGUE", "package import"))
                }
            }
            _ => None,
        };
        if let Some((code, construct)) = feature {
            diagnostics.push(diagnostic(text, source, token.at, code, construct));
        }
    }
}

/// The seeded catalogue is unqualified: no third-party package or version is eligible.
/// `manifest` is package.json, not a lockfile or an instruction to execute package scripts.
pub fn check(sources: &[Source<'_>], manifest: &str) -> Verdict {
    let mut diagnostics = Vec::new();
    if sources.len() != 1 || sources[0].text.len() > 1_048_576 || manifest.len() > 1_048_576 {
        return Verdict(vec![diagnostic("", 0, 0, UNSUPPORTED, "source bundle")]);
    }
    check_manifest(manifest, sources.len(), &mut diagnostics);
    for (source, input) in sources.iter().enumerate() {
        let tokens = match lex(input.text) {
            Ok(tokens) => tokens,
            Err(at) => {
                diagnostics.push(diagnostic(
                    input.text,
                    source,
                    at,
                    UNSUPPORTED,
                    "unresolved syntax",
                ));
                continue;
            }
        };
        scan_tokens(input.text, source, &tokens, &mut diagnostics, None);
        if diagnostics.is_empty() {
            let mut grammar = Grammar {
                source: input.text,
                tokens: &tokens,
                at: 0,
                asynchronous: false,
                context: false,
            };
            if grammar.program().is_err() {
                let at = tokens
                    .get(grammar.at)
                    .map_or(input.text.len(), |token| token.at);
                diagnostics.push(diagnostic(
                    input.text,
                    source,
                    at,
                    UNSUPPORTED,
                    "unresolved syntax",
                ));
            }
        }
    }
    Verdict(diagnostics)
}

/// One file of a multi-file source tree. `path` is relative to the tree root, `/`-separated.
pub struct TreeFile<'a> {
    pub path: &'a str,
    pub text: &'a str,
}

/// A tree diagnostic names the construct and its location (file, line, column), never source text.
#[derive(Debug, PartialEq, Eq)]
pub struct TreeDiagnostic {
    pub code: &'static str,
    pub construct: &'static str,
    pub path: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug)]
pub struct TreeVerdict(Vec<TreeDiagnostic>);

impl TreeVerdict {
    pub(super) fn from_diagnostics(diagnostics: Vec<TreeDiagnostic>) -> Self {
        Self(diagnostics)
    }

    pub fn supported(&self) -> bool {
        self.0.is_empty()
    }
    pub fn diagnostics(&self) -> &[TreeDiagnostic] {
        &self.0
    }
}

const MAX_TREE_FILES: usize = 256;

fn tree_path(path: &str) -> Option<String> {
    (!path.is_empty()
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".."))
    .then(|| path.to_owned())
}

/// Resolve a relative specifier against the importing file. `Err` means it leaves the tree.
fn resolve(from: &str, specifier: &str) -> Result<String, ()> {
    if specifier.starts_with('/') || specifier.contains(['\\', '\0']) {
        return Err(());
    }
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for part in specifier.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or(())?;
            }
            name => parts.push(name),
        }
    }
    if parts.is_empty() {
        Err(())
    } else {
        Ok(parts.join("/"))
    }
}

/// Static check of a multi-file source tree, separate from the single-source `check` so the
/// legacy adapter's build path never newly accepts several files. Starting at `entry`, relative
/// specifiers are resolved inside the tree and the files they reach are checked in turn; a
/// specifier that does not resolve or leaves the tree is a diagnostic, never a package. A lex
/// failure in a file is reported but still lets that file's imports and feature uses surface.
/// A reachable file with no feature hit still goes through the closed grammar, so ineligible
/// helpers are recorded rather than filtered. Diagnostics are sorted by file, line and column.
pub fn check_tree(files: &[TreeFile<'_>], entry: &str, manifest: Option<&str>) -> TreeVerdict {
    let at_origin = |path: &str, construct| TreeDiagnostic {
        code: UNSUPPORTED,
        construct,
        path: path.to_owned(),
        line: 1,
        column: 1,
    };
    if files.len() > MAX_TREE_FILES
        || files.iter().any(|file| file.text.len() > 1_048_576)
        || manifest.is_some_and(|text| text.len() > 1_048_576)
    {
        return TreeVerdict(vec![at_origin("", "source bundle")]);
    }
    let mut out = Vec::new();
    let mut index = std::collections::BTreeMap::new();
    for file in files {
        match tree_path(file.path) {
            Some(path) if index.contains_key(&path) => {
                out.push(at_origin(file.path, "duplicate tree path"));
            }
            Some(path) => {
                index.insert(path, file.text);
            }
            None => out.push(at_origin(file.path, "tree path")),
        }
    }
    if let Some(text) = manifest {
        let mut found = Vec::new();
        check_manifest(text, 0, &mut found);
        out.extend(found.into_iter().map(|d| TreeDiagnostic {
            code: d.code,
            construct: d.construct,
            path: "package.json".to_owned(),
            line: d.line,
            column: d.column,
        }));
    }
    let mut queue = std::collections::VecDeque::new();
    let mut seen = std::collections::BTreeSet::new();
    match tree_path(entry).filter(|path| index.contains_key(path)) {
        Some(path) => {
            seen.insert(path.clone());
            queue.push_back(path);
        }
        None => out.push(at_origin(entry, "entry module")),
    }
    while let Some(path) = queue.pop_front() {
        let text = index[&path];
        let (tokens, failed) = lex_with(text, true);
        let mut found = Vec::new();
        let mut relatives = Vec::new();
        if let Some(at) = failed {
            found.push(diagnostic(text, 0, at, UNSUPPORTED, "unresolved syntax"));
        }
        scan_tokens(text, 0, &tokens, &mut found, Some(&mut relatives));
        for (specifier, at) in relatives {
            match resolve(&path, &specifier) {
                Ok(target) if index.contains_key(&target) => {
                    if seen.insert(target.clone()) {
                        queue.push_back(target);
                    }
                }
                Ok(_) => found.push(diagnostic(
                    text,
                    0,
                    at,
                    UNSUPPORTED,
                    "unresolved relative import",
                )),
                Err(()) => found.push(diagnostic(
                    text,
                    0,
                    at,
                    UNSUPPORTED,
                    "relative import outside tree",
                )),
            }
        }
        if found.is_empty() {
            let mut grammar = Grammar {
                source: text,
                tokens: &tokens,
                at: 0,
                asynchronous: false,
                context: false,
            };
            if grammar.program().is_err() {
                let at = tokens.get(grammar.at).map_or(text.len(), |token| token.at);
                found.push(diagnostic(text, 0, at, UNSUPPORTED, "unresolved syntax"));
            }
        }
        out.extend(found.into_iter().map(|d| TreeDiagnostic {
            code: d.code,
            construct: d.construct,
            path: path.clone(),
            line: d.line,
            column: d.column,
        }));
    }
    out.sort_by(|a, b| {
        (&a.path, a.line, a.column, a.code, a.construct).cmp(&(
            &b.path,
            b.line,
            b.column,
            b.code,
            b.construct,
        ))
    });
    out.truncate(256);
    TreeVerdict(out)
}
