//! Bounded redaction of guest log lines.
//!
//! The work is linear and has a fixed budget. Each line is truncated to 8192 characters first;
//! then one leftmost-longest multi-pattern matcher, built once per execution, replaces every
//! credential string in one pass over each line. If the pattern bytes plus the line bytes exceed
//! `WORK_BUDGET`, or the matcher cannot be built, the logs are dropped and a single marker line
//! is returned: the failure mode never returns unredacted text.

use crate::api::{MAX_LOG_CHARS, MAX_LOG_LINES};
use aho_corasick::{AhoCorasick, MatchKind};

pub const REDACTED: &str = "<redacted>";

/// The fixed redaction budget of one execution: credential-string bytes plus log-line bytes
/// (12 MiB). A request near the 8 MiB ceiling with 1000 maximum-length lines can exceed it.
pub const WORK_BUDGET: usize = 12 * 1024 * 1024;

fn truncate(line: &mut String, chars: usize) {
    if let Some((end, _)) = line.char_indices().nth(chars) {
        line.truncate(end);
    }
}

/// Redacts every credential string from the log lines of one execution.
pub fn redact_logs(secrets: &[String], lines: Vec<String>) -> Vec<String> {
    redact_with_budget(secrets, lines, WORK_BUDGET)
}

pub fn redact_with_budget(
    secrets: &[String],
    mut lines: Vec<String>,
    budget: usize,
) -> Vec<String> {
    if lines.is_empty() {
        return lines;
    }
    lines.truncate(MAX_LOG_LINES);
    for line in &mut lines {
        truncate(line, MAX_LOG_CHARS);
    }
    let patterns: Vec<&str> = secrets
        .iter()
        .map(String::as_str)
        .filter(|secret| !secret.is_empty())
        .collect();
    let work = patterns
        .iter()
        .map(|pattern| pattern.len())
        .chain(lines.iter().map(String::len))
        .try_fold(0_usize, usize::checked_add);
    if work.is_none_or(|work| work > budget) {
        return vec![REDACTED.to_owned()];
    }
    if patterns.is_empty() {
        return lines;
    }
    let Ok(matcher) = AhoCorasick::builder()
        .match_kind(MatchKind::LeftmostLongest)
        .build(&patterns)
    else {
        return vec![REDACTED.to_owned()];
    };
    for line in &mut lines {
        let mut redacted = String::with_capacity(line.len());
        matcher.replace_all_with(line, &mut redacted, |_, _, out| {
            out.push_str(REDACTED);
            true
        });
        // Each replacement can lengthen a line; the contract bounds the result.
        truncate(&mut redacted, MAX_LOG_CHARS);
        *line = redacted;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn secrets(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn lines(values: &[&str]) -> Vec<String> {
        secrets(values)
    }

    #[test]
    fn a_credential_string_is_replaced_wherever_it_appears() {
        let out = redact_logs(
            &secrets(&["tok-9f3a"]),
            lines(&[
                "info tok-9f3a at start",
                "no secret here",
                "tok-9f3atok-9f3a",
            ]),
        );
        assert_eq!(
            out,
            [
                "info <redacted> at start",
                "no secret here",
                "<redacted><redacted>"
            ]
        );
    }

    #[test]
    fn short_credentials_are_redacted_too() {
        let out = redact_logs(&secrets(&["a", "Q"]), lines(&["banana Quail"]));
        assert_eq!(
            out,
            ["b<redacted>n<redacted>n<redacted> <redacted>u<redacted>il"]
        );
    }

    #[test]
    fn overlapping_and_prefix_sharing_secrets_are_fully_redacted() {
        // Leftmost-longest: "abcd" wins over its prefix "abc", and "cdef" overlapping the
        // match "abcd" does not leak the tail "ef".
        let out = redact_logs(
            &secrets(&["abc", "abcd", "cdef", "ef"]),
            lines(&["abcdef", "abcx", "xabcdefx"]),
        );
        assert_eq!(
            out,
            [
                "<redacted><redacted>",
                "<redacted>x",
                "x<redacted><redacted>x"
            ]
        );
        for line in &out {
            for leaked in ["abc", "cdef"] {
                assert!(!line.contains(leaked));
            }
        }
    }

    #[test]
    fn empty_secrets_are_ignored_and_no_secrets_keeps_the_lines() {
        assert_eq!(redact_logs(&secrets(&[""]), lines(&["plain"])), ["plain"]);
        assert_eq!(redact_logs(&[], lines(&["plain"])), ["plain"]);
        assert!(redact_logs(&secrets(&["x"]), Vec::new()).is_empty());
    }

    #[test]
    fn lines_are_truncated_before_redaction_and_stay_within_the_contract() {
        // A secret that straddles the 8192nd character is cut off before matching, so what
        // remains of it must not survive either.
        let mut straddling = "a".repeat(MAX_LOG_CHARS - 2);
        straddling.push_str("SECRET");
        let out = redact_logs(&secrets(&["SECRET"]), vec![straddling]);
        assert_eq!(out[0], format!("{}SE", "a".repeat(MAX_LOG_CHARS - 2)));
        // Replacements lengthen the line; the result is still bounded.
        let expanded = redact_logs(&secrets(&["x"]), vec!["x".repeat(MAX_LOG_CHARS)]);
        assert_eq!(expanded[0].chars().count(), MAX_LOG_CHARS);
        // At most 1000 lines are kept.
        assert_eq!(
            redact_logs(&[], vec!["l".to_owned(); MAX_LOG_LINES + 5]).len(),
            MAX_LOG_LINES
        );
    }

    #[test]
    fn multibyte_text_is_truncated_on_a_character_boundary() {
        let out = redact_logs(&secrets(&["k"]), vec!["é".repeat(MAX_LOG_CHARS + 10)]);
        assert_eq!(out[0].chars().count(), MAX_LOG_CHARS);
    }

    #[test]
    fn over_budget_returns_only_the_marker() {
        let secret = vec!["s".repeat(100)];
        let big = vec!["l".repeat(1000), "SECRET is s".repeat(10)];
        assert_eq!(redact_with_budget(&secret, big.clone(), 1000), [REDACTED]);
        // The same work within a larger budget is redacted normally.
        let out = redact_with_budget(&secret, big, 1_000_000);
        assert_eq!(out.len(), 2);
        // Exactly at the budget is within it; one byte below is not.
        let exact = 100 + 3;
        assert_eq!(
            redact_with_budget(&secret, vec!["abc".into()], exact),
            ["abc"]
        );
        assert_eq!(
            redact_with_budget(&secret, vec!["abc".into()], exact - 1),
            [REDACTED]
        );
    }

    /// Distinct credential strings of `count` entries, each `width` bytes.
    fn distinct(count: usize, width: usize) -> Vec<String> {
        (0..count).map(|n| format!("{n:0>width$}")).collect()
    }

    #[test]
    fn adversarial_load_is_linear_and_finishes_within_the_documented_bound() {
        // The documented bound is one second in release. A debug build is an order of magnitude
        // slower and runs a smaller set under a proportionally larger bound.
        let (count, bound) = if cfg!(debug_assertions) {
            (20_000, Duration::from_secs(30))
        } else {
            (400_000, Duration::from_secs(1))
        };
        let width = 8;
        let secrets = distinct(count, width);
        let lines: Vec<String> = (0..MAX_LOG_LINES)
            .map(|n| {
                let mut line = secrets[n % count].clone();
                line.push_str(&"x".repeat(MAX_LOG_CHARS));
                line
            })
            .collect();
        assert!(count * width + MAX_LOG_LINES * MAX_LOG_CHARS <= WORK_BUDGET);
        let start = Instant::now();
        let out = redact_logs(&secrets, lines);
        let elapsed = start.elapsed();
        assert!(elapsed < bound, "redaction took {elapsed:?}");
        assert_eq!(out.len(), MAX_LOG_LINES);
        assert!(out.iter().all(|line| line.starts_with(REDACTED)));
        assert!(out.iter().all(|line| !line.contains(&secrets[0])));
    }

    #[test]
    fn the_largest_request_exceeds_the_budget_and_fails_closed_quickly() {
        let secrets = distinct(1000, 8);
        // 1000 maximum-length lines plus enough pattern bytes to pass the budget.
        let padding = vec!["p".repeat(5 * 1024 * 1024)];
        let all: Vec<String> = secrets.into_iter().chain(padding).collect();
        let lines = vec!["y".repeat(MAX_LOG_CHARS); MAX_LOG_LINES];
        let start = Instant::now();
        let out = redact_logs(&all, lines);
        assert_eq!(out, [REDACTED]);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
