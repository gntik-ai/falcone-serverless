//! Redaction of the execution credential from the logs a worker returns or emits.
//!
//! The credential strings never reach a guest; they are redacted from guest log lines because a
//! guest may echo request data it was legitimately given. The work is bounded and linear:
//!
//! 1. every line is clipped to [`MAX_LINE_CHARS`] characters **before** any matching;
//! 2. one multi-pattern matcher is built once per execution, and every line is scanned once;
//! 3. matches are merged, so overlapping and prefix-sharing secrets leave no residue;
//! 4. a fixed work budget ([`WORK_BUDGET`]) covers the pattern bytes, the scanned bytes and the
//!    matches. Over budget, the logs are dropped and a single [`MARKER`] line is returned,
//!    never unredacted text.

use aho_corasick::{AhoCorasick, MatchKind};

/// What replaces every redacted span, and the single line returned when the budget is exceeded.
pub const MARKER: &str = "<redacted>";
/// The most log lines of a result.
pub const MAX_LINES: usize = 1000;
/// The longest log line of a result, in characters.
pub const MAX_LINE_CHARS: usize = 8192;
/// The fixed work budget of one execution's redaction: credential bytes, scanned bytes and
/// matches together. It leaves room for 1000 maximum-length lines and a million matches beside
/// 4 MiB of credential strings, and bounds the time to well under a second in a release build.
pub const WORK_BUDGET: usize = 16 * 1024 * 1024;

/// The credential strings of one execution, ready to redact lines.
pub struct Redactor<'a> {
    secrets: Vec<&'a str>,
    budget: usize,
}

impl<'a> Redactor<'a> {
    pub fn new(secrets: impl IntoIterator<Item = &'a str>) -> Self {
        Self::with_budget(secrets, WORK_BUDGET)
    }

    /// A redactor with an explicit work budget, for tests of the over-budget path.
    pub fn with_budget(secrets: impl IntoIterator<Item = &'a str>, budget: usize) -> Self {
        Self {
            secrets: secrets
                .into_iter()
                .filter(|secret| !secret.is_empty())
                .collect(),
            budget,
        }
    }

    /// The lines to return or emit: at most [`MAX_LINES`], each at most [`MAX_LINE_CHARS`]
    /// characters, none containing a credential string. Fails closed.
    pub fn lines(&self, lines: Vec<String>) -> Vec<String> {
        let lines: Vec<String> = lines
            .into_iter()
            .take(MAX_LINES)
            .map(|line| clip(line, MAX_LINE_CHARS))
            .collect();
        if lines.is_empty() {
            return lines;
        }
        let fail_closed = || vec![MARKER.to_owned()];
        let pattern_bytes: usize = self.secrets.iter().map(|secret| secret.len()).sum();
        let scanned: usize = lines.iter().map(String::len).sum();
        let mut work = pattern_bytes.saturating_add(scanned);
        if work > self.budget {
            return fail_closed();
        }
        if self.secrets.is_empty() {
            return lines;
        }
        // Overlapping search needs standard match semantics; the merge below makes the result
        // equal to redacting the union of every occurrence of every secret.
        let Ok(matcher) = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(&self.secrets)
        else {
            return fail_closed();
        };
        let mut redacted = Vec::with_capacity(lines.len());
        for line in lines {
            let mut spans: Vec<(usize, usize)> = Vec::new();
            for found in matcher.find_overlapping_iter(&line) {
                work = work.saturating_add(1);
                if work > self.budget {
                    return fail_closed();
                }
                merge(&mut spans, found.start(), found.end());
            }
            let Some(line) = replace(&line, &spans) else {
                return fail_closed();
            };
            redacted.push(clip(line, MAX_LINE_CHARS));
        }
        redacted
    }
}

/// Adds `start..end` to spans that are sorted and disjoint. Matches arrive in order of their
/// end, so the new span can only overlap or touch the last ones.
fn merge(spans: &mut Vec<(usize, usize)>, mut start: usize, end: usize) {
    while let Some(&(last_start, last_end)) = spans.last() {
        if last_end < start {
            break;
        }
        start = start.min(last_start);
        spans.pop();
    }
    spans.push((start, end));
}

fn replace(line: &str, spans: &[(usize, usize)]) -> Option<String> {
    if spans.is_empty() {
        return Some(line.to_owned());
    }
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    for &(start, end) in spans {
        out.push_str(line.get(at..start)?);
        out.push_str(MARKER);
        at = end;
    }
    out.push_str(line.get(at..)?);
    Some(out)
}

fn clip(line: String, limit: usize) -> String {
    match line.char_indices().nth(limit) {
        Some((end, _)) => line[..end].to_owned(),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(secrets: &[&str], line: &str) -> String {
        Redactor::new(secrets.iter().copied())
            .lines(vec![line.to_owned()])
            .remove(0)
    }

    #[test]
    fn every_occurrence_is_replaced() {
        assert_eq!(
            one(&["s3cr3t"], "a s3cr3t b s3cr3t"),
            "a <redacted> b <redacted>"
        );
    }

    #[test]
    fn prefix_sharing_secrets_leave_no_residue() {
        assert_eq!(one(&["abc", "abcdef"], "x abcdef y"), "x <redacted> y");
        assert_eq!(one(&["abcdef", "abc"], "x abcdef y"), "x <redacted> y");
    }

    #[test]
    fn overlapping_secrets_leave_no_residue() {
        assert_eq!(one(&["abcd", "cdef"], "-abcdef-"), "-<redacted>-");
        assert_eq!(one(&["cdef", "abcd", "bc"], "-abcdef-"), "-<redacted>-");
    }

    #[test]
    fn nested_secrets_and_a_one_character_secret_are_replaced() {
        assert_eq!(one(&["abcdef", "cd"], "abcdef"), "<redacted>");
        assert_eq!(one(&["x"], "axbxc"), "a<redacted>b<redacted>c");
    }

    #[test]
    fn multibyte_secrets_are_replaced_on_character_boundaries() {
        assert_eq!(
            one(&["é", "日本語"], "caf\u{e9} 日本語!"),
            "caf<redacted> <redacted>!"
        );
    }

    #[test]
    fn lines_are_clipped_before_matching() {
        // The secret sits wholly beyond the clip, so it is dropped with the rest of the tail.
        let line = format!("{}secret", "a".repeat(MAX_LINE_CHARS));
        let out = one(&["secret"], &line);
        assert_eq!(out, "a".repeat(MAX_LINE_CHARS));
        // Redaction that grows a line is clipped again.
        let line = "x ".repeat(MAX_LINE_CHARS / 2);
        assert_eq!(one(&["x"], &line).chars().count(), MAX_LINE_CHARS);
        // Touching secrets merge into one marker.
        assert_eq!(one(&["x"], "xxx"), "<redacted>");
    }

    #[test]
    fn at_most_one_thousand_lines_are_kept() {
        let lines = vec!["l".to_owned(); MAX_LINES + 5];
        assert_eq!(Redactor::new([]).lines(lines).len(), MAX_LINES);
    }

    #[test]
    fn over_budget_returns_only_the_marker() {
        let long = "k".repeat(64);
        let redactor = Redactor::with_budget([long.as_str()], 100);
        assert_eq!(
            redactor.lines(vec![long.clone(), "plain".into()]),
            vec![MARKER.to_owned()]
        );
        // No lines, nothing to hide.
        assert!(redactor.lines(Vec::new()).is_empty());
    }

    #[test]
    fn a_match_storm_exceeds_the_budget_and_fails_closed() {
        // 40 nested secrets over one run of 'a' produce far more matches than the budget.
        let secrets: Vec<String> = (1..=40).map(|n| "a".repeat(n)).collect();
        let redactor = Redactor::with_budget(secrets.iter().map(String::as_str), 2_000);
        let out = redactor.lines(vec!["a".repeat(200)]);
        assert_eq!(out, vec![MARKER.to_owned()]);
    }
}
