//! Source-only Go eligibility checks for the disabled qualification profile. This is not
//! a Go compiler, a sandbox, or runtime admission. The component import gate remains final.
//! Reports contain bounded relative file locations and fixed construct names, never source text.

pub struct File<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub construct: &'static str,
    pub source: usize,
    pub file: String,
    pub line: usize,
}

#[derive(Debug)]
struct Token {
    text: String,
    string: bool,
    line: usize,
}

// Preserve line numbers while separating comments, quoted/raw strings and identifiers.
// Invalid/unsupported escapes are refused, never guessed or passed through as imports.
fn tokens(text: &str) -> Result<Vec<Token>, usize> {
    let chars: Vec<char> = text.chars().collect();
    let (mut at, mut line) = (0, 1);
    let mut out = Vec::new();
    while at < chars.len() {
        let ch = chars[at];
        if ch.is_whitespace() {
            line += usize::from(ch == '\n');
            at += 1;
            continue;
        }
        if ch == '/' && chars.get(at + 1) == Some(&'/') {
            let begin = at;
            while at < chars.len() && chars[at] != '\n' {
                at += 1;
            }
            let comment: String = chars[begin..at].iter().collect();
            if comment.starts_with("//go:") {
                out.push(Token {
                    text: comment,
                    string: false,
                    line,
                });
            }
            continue;
        }
        if ch == '/' && chars.get(at + 1) == Some(&'*') {
            at += 2;
            while at + 1 < chars.len() && !(chars[at] == '*' && chars[at + 1] == '/') {
                line += usize::from(chars[at] == '\n');
                at += 1;
            }
            if at + 1 >= chars.len() {
                return Err(line);
            }
            at += 2;
            continue;
        }
        let start = line;
        if matches!(ch, '"' | '`' | '\'') {
            let quote = ch;
            at += 1;
            let mut value = String::new();
            while at < chars.len() && chars[at] != quote {
                let mut ch = chars[at];
                if ch == '\\' && quote != '`' {
                    at += 1;
                    ch = *chars.get(at).ok_or(start)?;
                    ch = match ch {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'a' => '\x07',
                        'b' => '\x08',
                        'f' => '\x0c',
                        'v' => '\x0b',
                        '\\' | '"' | '\'' => ch,
                        'x' | 'u' | 'U' | '0'..='7' => {
                            let (count, radix, skip) = match ch {
                                'x' => (2, 16, 1),
                                'u' => (4, 16, 1),
                                'U' => (8, 16, 1),
                                _ => (3, 8, 0),
                            };
                            at += skip;
                            let end = at + count;
                            let digits: String = chars.get(at..end).ok_or(start)?.iter().collect();
                            let code = u32::from_str_radix(&digits, radix).map_err(|_| start)?;
                            if (radix == 8 || count == 2) && code > 255 {
                                return Err(start);
                            }
                            at = end - 1;
                            char::from_u32(code).ok_or(start)?
                        }
                        _ => return Err(start),
                    };
                } else if ch == '\n' {
                    if quote != '`' {
                        return Err(start);
                    }
                    line += 1;
                }
                value.push(ch);
                at += 1;
            }
            if at == chars.len() {
                return Err(start);
            }
            at += 1;
            out.push(Token {
                text: value,
                string: quote != '\'',
                line: start,
            });
        } else if ch.is_alphabetic() || ch == '_' {
            let begin = at;
            at += 1;
            while at < chars.len() && (chars[at].is_alphanumeric() || chars[at] == '_') {
                at += 1;
            }
            out.push(Token {
                text: chars[begin..at].iter().collect(),
                string: false,
                line,
            });
        } else {
            out.push(Token {
                text: ch.to_string(),
                string: false,
                line,
            });
            at += 1;
        }
    }
    Ok(out)
}

/// Direct standard-library imports in this source-only qualification profile.
/// Data lives separately from Rust code; the repository's boundary scan treats every
/// occurrence of a privileged Rust keyword in a .rs file as code, including strings.
pub fn standard_library() -> Vec<String> {
    serde_json::from_str(include_str!("compat_go_stdlib.json")).expect("trusted Go stdlib data")
}

fn import_code(path: &str, stdlib: &[String]) -> Option<(&'static str, &'static str)> {
    if stdlib.iter().any(|package| package == path) {
        return None;
    }
    Some(match path {
        "C" | "runtime/cgo" => ("WASM-NATIVE-LIBRARY", "cgo import"),
        "os/exec" => ("WASM-PROCESS-SPAWN", "process import"),
        _ if path.split('/').any(|part| {
            matches!(
                part.to_ascii_lowercase().as_str(),
                "gpu" | "cuda" | "opencl" | "vulkan"
            )
        }) =>
        {
            ("WASM-GPU", "GPU import")
        }
        _ if path
            .split('/')
            .next()
            .is_some_and(|part| part.contains('.')) =>
        {
            ("WASM-PACKAGE-NOT-IN-CATALOGUE", "module import")
        }
        _ => (
            "WASM-UNSUPPORTED-API",
            "unsupported standard library import",
        ),
    })
}

fn module(file: &File<'_>) -> Result<String, (&'static str, &'static str, usize)> {
    let tokens =
        tokens(file.text).map_err(|line| ("WASM-UNSUPPORTED-API", "module syntax", line))?;
    let mut name = None;
    let mut go = false;
    for (at, token) in tokens.iter().enumerate() {
        if at > 0 && tokens[at - 1].line == token.line && tokens[at - 1].text != ";" {
            continue;
        }
        let value: String = tokens[at + 1..]
            .iter()
            .take_while(|t| t.line == token.line && t.text != ";")
            .map(|t| t.text.as_str())
            .collect();
        match token.text.as_str() {
            "require" | "replace" | "exclude" | "retract" => {
                return Err((
                    "WASM-PACKAGE-NOT-IN-CATALOGUE",
                    "module requirement",
                    token.line,
                ));
            }
            "tool" => return Err(("WASM-INSTALL-HOOK", "module build tool", token.line)),
            "module" if name.is_none() && !value.is_empty() => name = Some(value),
            "go" if value == "1.26.11" && !go => go = true,
            "toolchain" if value == "go1.26.11" => {}
            _ => {
                return Err((
                    "WASM-UNSUPPORTED-API",
                    "module directive or toolchain pin",
                    token.line,
                ));
            }
        }
    }
    name.filter(|_| go)
        .ok_or(("WASM-UNSUPPORTED-API", "missing module or Go pin", 1))
}

pub fn check_tree(files: &[File<'_>], interfaces: &[&str]) -> Vec<Diagnostic> {
    let mut report = Vec::new();
    let stdlib = standard_library();
    let module_name = files
        .iter()
        .enumerate()
        .find(|(_, f)| f.name == "go.mod")
        .and_then(|(source, file)| match module(file) {
            Ok(name) => Some(name),
            Err((code, construct, line)) => {
                report.push(Diagnostic {
                    file: String::new(),
                    code,
                    construct,
                    source,
                    line,
                });
                None
            }
        });
    for (source, file) in files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.name.ends_with("/go.mod"))
    {
        if let Err((code, construct, line)) = module(file) {
            report.push(Diagnostic {
                file: String::new(),
                code,
                construct,
                source,
                line,
            });
        }
    }
    for (source, file) in files.iter().enumerate() {
        if !file.name.ends_with(".go") {
            continue;
        }
        let tokens = match tokens(file.text) {
            Ok(tokens) => tokens,
            Err(line) => {
                report.push(Diagnostic {
                    file: String::new(),
                    code: "WASM-UNSUPPORTED-API",
                    construct: "source syntax",
                    source,
                    line,
                });
                continue;
            }
        };
        let mut at = 0;
        while at < tokens.len() {
            let token = &tokens[at];
            if !token.string && (token.text.starts_with("//go:generate") || token.text == "go") {
                let (code, construct) = if token.text == "go" {
                    ("WASM-PROCESS-SPAWN", "goroutine")
                } else {
                    ("WASM-INSTALL-HOOK", "generation directive")
                };
                report.push(Diagnostic {
                    file: String::new(),
                    code,
                    construct,
                    source,
                    line: token.line,
                });
            }
            if !token.string
                && token.text.starts_with("//go:")
                && !token.text.starts_with("//go:generate")
            {
                let words: Vec<_> = token.text.split_whitespace().collect();
                let permitted = match words.as_slice() {
                    ["//go:wasmimport", interface, _] => {
                        interfaces.contains(interface)
                            && crate::artifact::validate::ALLOWED_IMPORTS.contains(interface)
                    }
                    [
                        "//go:wasmexport",
                        "falcone:host/handler@0.2.0#handle"
                        | "cabi_post_falcone:host/handler@0.2.0#handle"
                        | "cabi_realloc",
                    ] => true,
                    _ => false,
                };
                if !permitted {
                    report.push(Diagnostic {
                        file: String::new(),
                        code: "WASM-UNSUPPORTED-API",
                        construct: "interface or compiler directive",
                        source,
                        line: token.line,
                    });
                }
            }
            if !tokens[at].string && tokens[at].text == "import" {
                at += 1;
                let grouped = tokens.get(at).is_some_and(|t| t.text == "(");
                if grouped {
                    at += 1;
                }
                while at < tokens.len() {
                    let token = &tokens[at];
                    if grouped && token.text == ")" {
                        break;
                    }
                    if token.string {
                        let local = module_name
                            .as_ref()
                            .and_then(|name| token.text.strip_prefix(&format!("{name}/")))
                            .is_some_and(|path| {
                                !path
                                    .split('/')
                                    .any(|p| p.is_empty() || matches!(p, "." | ".."))
                                    && files.iter().any(|f| {
                                        f.name.starts_with(&format!("{path}/"))
                                            && f.name.ends_with(".go")
                                    })
                            });
                        if let Some((code, construct)) =
                            import_code(&token.text, &stdlib).filter(|_| !local)
                        {
                            report.push(Diagnostic {
                                file: String::new(),
                                code,
                                construct,
                                source,
                                line: token.line,
                            });
                        }
                        if !grouped {
                            break;
                        }
                    }
                    at += 1;
                }
            }
            at += 1;
        }
    }
    for diagnostic in &mut report {
        let name = files[diagnostic.source].name;
        diagnostic.file = if name.len() <= 256
            && !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
            && name
                .split('/')
                .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
        {
            name.to_owned()
        } else {
            "<source>".to_owned()
        };
    }
    report
}
