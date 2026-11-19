//! Fail-closed, deliberately small JavaScript grammar. This is not a JavaScript engine.
//! Only a parsed `main(params)` and explicitly recognized operations can be supported.
//! Unsupported syntax (including escaped identifiers, templates and computed access) is
//! blocked rather than guessed at. No third-party catalogue is qualified yet.
use serde_json::Value;
use std::collections::BTreeSet;

const UNSUPPORTED: &str = "WASM-UNSUPPORTED-API";

/// Diagnostics contain rule labels and fixed source filenames, never source excerpts,
/// package names, parameter values or caller-supplied paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub construct: &'static str,
    pub file: &'static str,
    pub line: usize,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {} ({})",
            self.file, self.line, self.code, self.construct
        )
    }
}

/// Cannot be constructed from a caller's boolean or an untrusted serialized verdict.
#[derive(Debug)]
pub struct Verdict {
    diagnostics: Vec<Diagnostic>,
}

impl Verdict {
    pub fn is_supported(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

fn diagnostic(construct: &'static str, line: usize) -> Diagnostic {
    Diagnostic {
        code: UNSUPPORTED,
        construct,
        file: "function.js",
        line,
    }
}

#[derive(PartialEq, Eq)]
enum Kind {
    Word,
    String,
    Number,
    Symbol,
}

struct Token {
    text: String,
    kind: Kind,
    line: usize,
}

fn lex(source: &str) -> Result<Vec<Token>, Diagnostic> {
    if source.len() > 1_048_576 {
        return Err(diagnostic("source size limit", 1));
    }
    let bytes = source.as_bytes();
    let (mut at, mut line) = (0, 1);
    let mut out = Vec::new();
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
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
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
                continue;
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at + 1 < bytes.len() && &bytes[at..at + 2] != b"*/" {
                    if bytes[at] == b'\n' {
                        line += 1;
                    }
                    at += 1;
                }
                if at + 1 >= bytes.len() {
                    return Err(diagnostic("unterminated comment", line));
                }
                at += 2;
                continue;
            }
            _ => {}
        }
        let token_line = line;
        let (kind, text) = match bytes[at] {
            quote @ (b'\'' | b'"') => {
                at += 1;
                let content = at;
                while at < bytes.len() && bytes[at] != quote {
                    // Escape forms are intentionally unqualified, not silently interpreted.
                    if bytes[at] == b'\\' || bytes[at] < b' ' {
                        return Err(diagnostic("string syntax", line));
                    }
                    at += 1;
                }
                if at == bytes.len() {
                    return Err(diagnostic("unterminated string", line));
                }
                let text = source[content..at].to_owned();
                at += 1;
                (Kind::String, text)
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => {
                at += 1;
                while bytes
                    .get(at)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
                {
                    at += 1;
                }
                (Kind::Word, source[start..at].to_owned())
            }
            b'0'..=b'9' => {
                at += 1;
                while bytes
                    .get(at)
                    .is_some_and(|b| b.is_ascii_digit() || *b == b'.')
                {
                    at += 1;
                }
                let text = &source[start..at];
                if serde_json::from_str::<serde_json::Number>(text).is_err() {
                    return Err(diagnostic("number syntax", line));
                }
                (Kind::Number, text.to_owned())
            }
            b'{' | b'}' | b'(' | b')' | b'[' | b']' | b'.' | b',' | b':' | b';' | b'=' | b'+'
            | b'-' | b'*' | b'/' => {
                at += 1;
                (Kind::Symbol, source[start..at].to_owned())
            }
            _ => return Err(diagnostic("unknown syntax", line)),
        };
        out.push(Token {
            text,
            kind,
            line: token_line,
        });
        if out.len() > 65_536 {
            return Err(diagnostic("token limit", line));
        }
    }
    Ok(out)
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
    depth: usize,
    locals: BTreeSet<String>,
    imports: BTreeSet<String>,
    asynchronous: bool,
    crypto_imported: bool,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self {
            tokens,
            at: 0,
            depth: 0,
            locals: BTreeSet::from(["params".into()]),
            imports: BTreeSet::new(),
            asynchronous: false,
            crypto_imported: false,
        }
    }
    fn is(&self, text: &str) -> bool {
        self.tokens
            .get(self.at)
            .is_some_and(|t| t.text == text && t.kind != Kind::String)
    }
    fn take(&mut self, text: &str) -> bool {
        let found = self.is(text);
        self.at += usize::from(found);
        found
    }
    fn error(&self) -> Diagnostic {
        diagnostic(
            "unqualified syntax or operation",
            self.tokens
                .get(self.at)
                .or_else(|| self.tokens.last())
                .map_or(1, |t| t.line),
        )
    }
    fn expect(&mut self, text: &str) -> Result<(), Diagnostic> {
        if self.take(text) {
            Ok(())
        } else {
            Err(self.error())
        }
    }
    fn program(&mut self) -> Result<(), Diagnostic> {
        while self.take("import") {
            let mut names = Vec::new();
            let crypto = self.take("crypto");
            if crypto && self.crypto_imported {
                return Err(self.error());
            }
            self.crypto_imported |= crypto;
            if !crypto {
                self.expect("{")?;
                loop {
                    let name = self.tokens.get(self.at).ok_or_else(|| self.error())?;
                    if name.kind != Kind::Word
                        || reserved(&name.text)
                        || self.locals.contains(&name.text)
                        || self.imports.contains(&name.text)
                        || names.contains(&name.text)
                    {
                        return Err(self.error());
                    }
                    names.push(name.text.clone());
                    self.at += 1;
                    if self.take("}") {
                        break;
                    }
                    self.expect(",")?;
                }
            }
            self.expect("from")?;
            let module = self.tokens.get(self.at).ok_or_else(|| self.error())?;
            if module.kind != Kind::String
                || (crypto && module.text != "node:crypto")
                || (!crypto && !names.iter().all(|name| host_function(&module.text, name)))
            {
                return Err(self.error());
            }
            self.imports.extend(names);
            self.at += 1;
            self.expect(";")?;
        }
        self.take("export");
        self.asynchronous = self.take("async");
        for token in ["function", "main", "(", "params", ")", "{"] {
            self.expect(token)?;
        }
        while !self.take("}") {
            if self.take("return") {
                self.expression()?;
            } else if self.take("throw") {
                if self
                    .tokens
                    .get(self.at)
                    .is_some_and(|t| t.line != self.tokens[self.at - 1].line)
                {
                    return Err(self.error());
                }
                self.expression()?;
            } else if self.take("const") || self.take("let") {
                let name = self.tokens.get(self.at).ok_or_else(|| self.error())?;
                if name.kind != Kind::Word
                    || self.locals.contains(&name.text)
                    || self.imports.contains(&name.text)
                    || reserved(&name.text)
                {
                    return Err(self.error());
                }
                let name = name.text.clone();
                self.at += 1;
                self.expect("=")?;
                self.expression()?;
                self.locals.insert(name);
            } else {
                self.expression()?;
            }
            if !self.take(";") && !self.is("}") {
                return Err(self.error());
            }
        }
        self.take(";");
        if self.at == self.tokens.len() {
            Ok(())
        } else {
            Err(self.error())
        }
    }
    fn expression(&mut self) -> Result<(), Diagnostic> {
        if self.depth >= 64 {
            return Err(self.error());
        }
        self.depth += 1;
        let result = self.value();
        self.depth -= 1;
        result
    }
    fn value(&mut self) -> Result<(), Diagnostic> {
        if self.take("await") {
            if !self.asynchronous {
                return Err(self.error());
            }
            return self.expression();
        }
        if self
            .tokens
            .get(self.at)
            .is_some_and(|token| token.kind == Kind::Word && self.imports.contains(&token.text))
        {
            self.at += 1;
            self.arguments()?;
        } else if self.take("console") {
            self.expect(".")?;
            self.expect("log")?;
            self.arguments()?;
        } else if self.take("crypto") {
            self.expect(".")?;
            if self.take("subtle") {
                self.expect(".")?;
                self.expect("digest")?;
            } else {
                self.expect("getRandomValues")?;
            }
            self.arguments()?;
        } else if self.take("new") {
            if !self.take("Uint8Array") && !self.take("Error") {
                return Err(self.error());
            }
            self.arguments()?;
        } else if self.take("{") {
            if self.take("}") {
                return Ok(());
            }
            loop {
                let key = self.tokens.get(self.at).ok_or_else(|| self.error())?;
                if !matches!(key.kind, Kind::Word | Kind::String | Kind::Number)
                    || key.text == "__proto__"
                {
                    return Err(self.error());
                }
                self.at += 1;
                self.expect(":")?;
                self.expression()?;
                if self.take("}") {
                    break;
                }
                self.expect(",")?;
            }
        } else if self.take("[") {
            if self.take("]") {
                return Ok(());
            }
            loop {
                self.expression()?;
                if self.take("]") {
                    break;
                }
                self.expect(",")?;
            }
        } else if self.take("(") {
            self.expression()?;
            self.expect(")")?;
        } else {
            let token = self.tokens.get(self.at).ok_or_else(|| self.error())?;
            if matches!(token.kind, Kind::String | Kind::Number)
                || (token.kind == Kind::Word
                    && (self.locals.contains(&token.text)
                        || matches!(token.text.as_str(), "true" | "false" | "null")))
            {
                self.at += 1;
            } else {
                return Err(self.error());
            }
        }
        while self.take(".") {
            let property = self.tokens.get(self.at).ok_or_else(|| self.error())?;
            if property.kind != Kind::Word
                || matches!(
                    property.text.as_str(),
                    "constructor" | "prototype" | "__proto__"
                )
            {
                return Err(self.error());
            }
            self.at += 1;
        }
        Ok(())
    }
    fn arguments(&mut self) -> Result<(), Diagnostic> {
        self.expect("(")?;
        if self.take(")") {
            return Ok(());
        }
        loop {
            self.expression()?;
            if self.take(")") {
                return Ok(());
            }
            self.expect(",")?;
        }
    }
}

fn host_function(module: &str, name: &str) -> bool {
    match module {
        "falcone:host/invocation-context@0.2.0" => {
            matches!(name, "invocationKey" | "attemptId" | "idempotencyKey")
        }
        "falcone:host/events@0.2.0" => name == "publish",
        "falcone:host/storage@0.2.0" => matches!(
            name,
            "openObject"
                | "readObject"
                | "writeObject"
                | "openCheckpoint"
                | "readCheckpoint"
                | "writeCheckpoint"
        ),
        _ => false,
    }
}

fn reserved(name: &str) -> bool {
    matches!(
        name,
        "crypto"
            | "console"
            | "Uint8Array"
            | "Error"
            | "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "instanceof"
            | "interface"
            | "let"
            | "new"
            | "null"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
    )
}

fn feature_diagnostics(tokens: &[Token]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let text = token.text.as_str();
        let next =
            |offset: usize, text: &str| tokens.get(at + offset).is_some_and(|t| t.text == text);
        let feature = if token.kind == Kind::String && text.ends_with(".node") {
            Some(("WASM-NATIVE-LIBRARY", "native addon"))
        } else if text == "process" && next(1, ".") && next(2, "dlopen") {
            Some(("WASM-NATIVE-LIBRARY", "process.dlopen"))
        } else if matches!(
            text.strip_prefix("node:").unwrap_or(text),
            "child_process" | "worker_threads" | "cluster"
        ) {
            Some(("WASM-PROCESS-SPAWN", "process or thread module"))
        } else if matches!(
            text,
            "gpu" | "GPU" | "webgpu" | "WebGLRenderingContext" | "WebGL2RenderingContext"
        ) {
            Some(("WASM-GPU", "GPU access"))
        } else if token.kind == Kind::String && text.starts_with("node:") && text != "node:crypto" {
            Some((UNSUPPORTED, "unsupported node module"))
        } else if text == "crypto"
            && next(1, ".")
            && !(next(2, "getRandomValues")
                || (next(2, "subtle") && next(3, ".") && next(4, "digest")))
        {
            Some((UNSUPPORTED, "crypto API"))
        } else if token.kind == Kind::Word && text == "eval" {
            Some((UNSUPPORTED, "eval"))
        } else if text == "new" && next(1, "Function") {
            Some((UNSUPPORTED, "new Function"))
        } else if matches!(text, "require" | "import")
            && next(1, "(")
            && tokens.get(at + 2).is_none_or(|t| t.kind != Kind::String)
        {
            Some((
                UNSUPPORTED,
                if text == "require" {
                    "dynamic require"
                } else {
                    "dynamic import"
                },
            ))
        } else if token.kind == Kind::String
            && is_module_specifier(tokens, at)
            && text != "node:crypto"
            && !text.starts_with("falcone:host/")
        {
            Some(("WASM-PACKAGE-NOT-IN-CATALOGUE", "third-party module"))
        } else {
            None
        };
        if let Some((code, construct)) = feature {
            out.push(Diagnostic {
                code,
                ..diagnostic(construct, token.line)
            });
        }
    }
    out
}

fn is_module_specifier(tokens: &[Token], at: usize) -> bool {
    (at > 0 && tokens[at - 1].text == "from")
        || (at > 0 && tokens[at - 1].text == "import")
        || (at >= 2
            && tokens[at - 1].text == "("
            && matches!(tokens[at - 2].text.as_str(), "require" | "import"))
}

fn manifest_diagnostics(bytes: &[u8]) -> Vec<Diagnostic> {
    let invalid = || {
        vec![Diagnostic {
            file: "package.json",
            ..diagnostic("manifest syntax", 1)
        }]
    };
    if bytes.len() > 1_048_576 {
        return invalid();
    }
    let Ok(Value::Object(manifest)) = serde_json::from_slice::<Value>(bytes) else {
        return invalid();
    };
    let source = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    for key in [
        "scripts",
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
        "bundledDependencies",
        "bundleDependencies",
        "gypfile",
        "binary",
    ] {
        let Some(value) = manifest.get(key) else {
            continue;
        };
        if value.as_object().is_some_and(|map| map.is_empty())
            || (matches!(key, "bundledDependencies" | "bundleDependencies")
                && value.as_array().is_some_and(|list| list.is_empty()))
        {
            continue;
        }
        let (code, construct) = match key {
            "scripts" => ("WASM-INSTALL-HOOK", "lifecycle script"),
            "gypfile" | "binary" => ("WASM-NATIVE-LIBRARY", "native addon manifest"),
            _ => ("WASM-PACKAGE-NOT-IN-CATALOGUE", "third-party dependency"),
        };
        let line = manifest_key_line(&source, key);
        out.push(Diagnostic {
            code,
            construct,
            file: "package.json",
            line,
        });
    }
    out
}

/// Locate decoded top-level JSON keys, not strings in descriptions or nested objects.
/// The manifest has already passed JSON parsing; the last duplicate key is authoritative.
fn manifest_key_line(source: &str, key: &str) -> usize {
    let bytes = source.as_bytes();
    let (mut at, mut depth, mut line, mut found) = (0, 0usize, 1, 1);
    while at < bytes.len() {
        match bytes[at] {
            b'"' => {
                let start = at;
                at += 1;
                while at < bytes.len() && bytes[at] != b'"' {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
                at += 1;
                if depth == 1
                    && bytes[at..].iter().find(|b| !b.is_ascii_whitespace()) == Some(&b':')
                    && serde_json::from_str::<String>(&source[start..at])
                        .is_ok_and(|name| name == key)
                {
                    found = line;
                }
            }
            b'{' | b'[' => {
                depth += 1;
                at += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                at += 1;
            }
            b'\n' => {
                line += 1;
                at += 1;
            }
            _ => at += 1,
        }
    }
    found
}

/// Classifies one source unit and its package manifest. `supported` means only that the
/// recognized subset has no static blocker; registry admission is a separate mandatory gate.
pub fn check(source: &str, manifest: &[u8]) -> Verdict {
    let mut diagnostics = manifest_diagnostics(manifest);
    match lex(source) {
        Ok(tokens) => {
            diagnostics.extend(feature_diagnostics(&tokens));
            if let Err(error) = Parser::new(&tokens).program() {
                diagnostics.push(error);
            }
        }
        Err(error) => diagnostics.push(error),
    }
    Verdict { diagnostics }
}
