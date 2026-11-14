//! Redaction of the credential from guest logs, bounded in time and in output.
//!
//! Each line is cut to the result's line limit first. One multi-pattern matcher is built once
//! per execution, so the work is linear in the secret bytes plus the line bytes, plus one step
//! per match. Every occurrence of every secret is found, overlapping ones included, and the
//! union of the matched spans is replaced, so no part of a secret survives next to another.
//! A fixed work budget caps the secrets and the matches; past it the logs are dropped and the
//! result carries one marker line, never unredacted text.

use aho_corasick::{AhoCorasick, MatchKind};

/// What a redacted span becomes.
pub const MARKER: &str = "<redacted>";
/// The most characters in one log line of a result (`Logs::MAX_LINE_CHARS`).
pub const MAX_LINE_CHARS: usize = 8192;
/// The most lines in the logs of a result (`Logs::MAX_LINES`).
pub const MAX_LINES: usize = 1000;
/// The work budget of one execution: the most distinct secrets, the most secret bytes and the
/// most matches. A credential or a log past it gets the marker line instead of its logs.
pub const MAX_SECRETS: usize = 100_000;
pub const MAX_SECRET_BYTES: usize = 1024 * 1024;
pub const MAX_MATCHES: usize = 8_000_000;

fn truncated(line: &str) -> &str {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => &line[..end],
        None => line,
    }
}

/// `line` with every span that any secret covers replaced by one marker, or None when
/// `matches` (the budget left) ran out.
fn replaced(matcher: &AhoCorasick, line: &str, matches: &mut usize) -> Option<String> {
    // The furthest end of a match starting at each byte, 0 for none. Matches arrive by end
    // position; recording them by start makes the union a single sweep.
    let mut furthest = vec![0usize; line.len() + 1];
    let mut any = false;
    for found in matcher.find_overlapping_iter(line) {
        *matches = matches.checked_sub(1)?;
        any = true;
        let slot = &mut furthest[found.start()];
        *slot = (*slot).max(found.end());
    }
    if !any {
        return Some(line.to_owned());
    }
    let mut out = String::with_capacity(line.len());
    let (mut index, mut copied) = (0, 0);
    while index < line.len() {
        if furthest[index] <= index {
            index += 1;
            continue;
        }
        let mut end = furthest[index];
        let mut next = index + 1;
        while next < end {
            end = end.max(furthest[next]);
            next += 1;
        }
        out.push_str(&line[copied..index]);
        out.push_str(MARKER);
        index = end;
        copied = end;
    }
    out.push_str(&line[copied..]);
    Some(out)
}

/// The logs of a result: at most `MAX_LINES` lines of at most `MAX_LINE_CHARS` characters,
/// with every occurrence of every secret replaced. Replacement can lengthen a line, so the
/// bound is applied again to the output.
pub fn redact_logs(lines: Vec<String>, secrets: &[String]) -> Vec<String> {
    if lines.is_empty() {
        return lines;
    }
    let fail_closed = || vec![MARKER.to_owned()];
    let bytes: usize = secrets.iter().map(String::len).sum();
    if secrets.len() > MAX_SECRETS || bytes > MAX_SECRET_BYTES {
        return fail_closed();
    }
    let matcher = if secrets.is_empty() {
        None
    } else {
        match AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(secrets)
        {
            Ok(matcher) => Some(matcher),
            Err(_) => return fail_closed(),
        }
    };
    let mut matches = MAX_MATCHES;
    let mut out = Vec::new();
    for line in lines.iter().take(MAX_LINES) {
        let line = truncated(line);
        let mut redacted = match &matcher {
            None => line.to_owned(),
            Some(matcher) => match replaced(matcher, line, &mut matches) {
                Some(redacted) => redacted,
                None => return fail_closed(),
            },
        };
        if let Some((end, _)) = redacted.char_indices().nth(MAX_LINE_CHARS) {
            redacted.truncate(end);
        }
        out.push(redacted);
    }
    out
}
