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
}

#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    at: usize,
    kind: Kind,
}

/// Escapes, templates and regex literals are unresolved rather than interpreted heuristically.
fn lex(text: &str) -> Result<Vec<Token<'_>>, usize> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut out = Vec::new();
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
                    return Err(at);
                };
                at += end + 4;
                continue;
            }
            b'\'' | b'"' => {
                let quote = bytes[at];
                at += 1;
                while at < bytes.len() && bytes[at] != quote {
                    if matches!(bytes[at], b'\\' | b'\n' | b'\r') {
                        return Err(start);
                    }
                    at += 1;
                }
                if at == bytes.len() {
                    return Err(start);
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
                if !serde_json::from_str::<Value>(token).is_ok_and(|value| value.is_number()) {
                    return Err(start);
                }
                out.push(Token {
                    text: token,
                    at: start,
                    kind: Kind::Number,
                });
            }
            b'(' | b')' | b'{' | b'}' | b'[' | b']' | b'.' | b',' | b':' | b';' | b'-' => {
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
            _ => return Err(at),
        }
        if out.len() > 100_000 {
            return Err(at);
        }
    }
    Ok(out)
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

/// The seeded catalogue is unqualified: no third-party package or version is eligible.
/// `manifest` is package.json, not a lockfile or an instruction to execute package scripts.
pub fn check(sources: &[Source<'_>], manifest: &str) -> Verdict {
    let mut diagnostics = Vec::new();
    if sources.len() != 1 || sources[0].text.len() > 1_048_576 || manifest.len() > 1_048_576 {
        return Verdict(vec![diagnostic("", 0, 0, UNSUPPORTED, "source bundle")]);
    }
    match serde_json::from_str::<Value>(manifest)
        .ok()
        .and_then(|value| value.as_object().cloned())
    {
        None => diagnostics.push(diagnostic(
            manifest,
            sources.len(),
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
                            sources.len(),
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
                            sources.len(),
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
                        sources.len(),
                        at,
                        UNSUPPORTED,
                        "package manifest",
                    )),
                }
            }
        }
    }
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
        for (index, token) in tokens.iter().enumerate() {
            if diagnostics.len() >= 256 {
                break;
            }
            let next = |offset: usize| tokens.get(index + offset).map(|token| token.text);
            let feature = match (token.kind, token.text) {
                (Kind::String, "child_process" | "node:child_process") => {
                    Some(("WASM-PROCESS-SPAWN", "child_process"))
                }
                (Kind::String, "worker_threads" | "node:worker_threads") => {
                    Some(("WASM-PROCESS-SPAWN", "worker_threads"))
                }
                (Kind::String, "cluster" | "node:cluster") => {
                    Some(("WASM-PROCESS-SPAWN", "cluster"))
                }
                (Kind::String, module) if module.ends_with(".node") => {
                    Some(("WASM-NATIVE-LIBRARY", ".node"))
                }
                (Kind::String, "node:crypto" | "crypto") => Some((UNSUPPORTED, "node:crypto API")),
                (Kind::String, module) if module.starts_with("node:") => {
                    Some((UNSUPPORTED, "node module"))
                }
                (Kind::Word, "process") if next(1) == Some(".") && next(2) == Some("dlopen") => {
                    Some(("WASM-NATIVE-LIBRARY", "process.dlopen"))
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
                    if argument.is_none_or(|argument| argument.kind != Kind::String) {
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
                (Kind::String, _)
                    if (index > 0 && matches!(tokens[index - 1].text, "from" | "import"))
                        || (index > 1
                            && tokens[index - 1].text == "("
                            && matches!(tokens[index - 2].text, "require" | "import")) =>
                {
                    // No dependency catalogue is qualified, including local modules.
                    Some(("WASM-PACKAGE-NOT-IN-CATALOGUE", "package import"))
                }
                _ => None,
            };
            if let Some((code, construct)) = feature {
                diagnostics.push(diagnostic(input.text, source, token.at, code, construct));
            }
        }
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
