//! Log redaction: the execution credential never leaves the host binding layer in a log line.
//!
//! One Aho-Corasick automaton is built per execution from the credential's distinct strings.
//! The patterns are every 8-character window of a secret and every secret shorter than 8, so
//! the guarantee is exactly the stated one however text came to hold a secret: no output line
//! contains 8 or more consecutive characters of a secret, or a whole shorter secret. Work is
//! linear in the secret set, the captured lines and the matches, and it has one fixed total
//! budget; past the budget the result fails closed to a single `<redacted>` line, never to
//! unredacted text.
//!
//! The order matters. Each line is redacted whole, before it is cut to its output limit, so a
//! secret that crosses the cut, ends at it or is longer than it is replaced as one piece. All
//! occurrences are found, overlapping ones included, and their union is replaced.

use aho_corasick::{AhoCorasick, MatchKind};
use std::collections::BTreeSet;

/// What replaces redacted text, and what a failed-closed result consists of.
pub const MARKER: &str = "<redacted>";

/// The most credential bytes the automaton is built from. Each secret contributes one pattern
/// per 8-character window, so the pattern set is at most eight times this.
pub const SECRET_BYTES_MAX: usize = 256 * 1024;
/// The longest raw line that is scanned; a longer one is replaced by the marker whole.
pub const LINE_BYTES_MAX: usize = 64 * 1024;
/// The most raw log bytes one execution scans.
pub const INPUT_BYTES_MAX: usize = 16 * 1024 * 1024;
/// The most matches one execution resolves; adversarial nesting of secrets runs into it.
pub const MATCHES_MAX: usize = 32 * 1024 * 1024;
/// The most lines kept.
pub const LINES_MAX: usize = crate::api::MAX_LOG_LINES;

/// Raw lines as the guest produced them, bounded at capture.
#[derive(Default)]
pub struct Capture {
    lines: Vec<String>,
    bytes: usize,
    over_budget: bool,
}

impl Capture {
    pub fn push(&mut self, line: String) {
        if self.over_budget || self.lines.len() >= LINES_MAX {
            return;
        }
        if line.len() > LINE_BYTES_MAX {
            // Too long to scan within the budget: fail closed for this line alone.
            self.lines.push(MARKER.to_owned());
            return;
        }
        if self.bytes + line.len() > INPUT_BYTES_MAX {
            self.over_budget = true;
            self.lines.clear();
            return;
        }
        self.bytes += line.len();
        self.lines.push(line);
    }
}

/// A secret's patterns: each 8-character window, or the whole secret when it is shorter.
fn windows(secret: &str, patterns: &mut BTreeSet<Vec<u8>>) {
    let starts: Vec<usize> = secret.char_indices().map(|(at, _)| at).collect();
    if starts.len() < 8 {
        patterns.insert(secret.as_bytes().to_vec());
        return;
    }
    for first in 0..=starts.len() - 8 {
        let end = starts.get(first + 8).copied().unwrap_or(secret.len());
        patterns.insert(secret.as_bytes()[starts[first]..end].to_vec());
    }
}

/// The matcher for one execution's credential.
pub struct Redactor {
    automaton: Option<AhoCorasick>,
    /// Set when the secret set could not be matched within its budget.
    failed_closed: bool,
}

impl Redactor {
    pub fn new(secrets: &[String]) -> Self {
        let closed = || Self {
            automaton: None,
            failed_closed: true,
        };
        let total: usize = secrets.iter().map(String::len).sum();
        if total > SECRET_BYTES_MAX {
            return closed();
        }
        let mut patterns: BTreeSet<Vec<u8>> = BTreeSet::new();
        for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
            windows(secret, &mut patterns);
        }
        if patterns.is_empty() {
            return Self {
                automaton: None,
                failed_closed: false,
            };
        }
        match AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(patterns)
        {
            Ok(automaton) => Self {
                automaton: Some(automaton),
                failed_closed: false,
            },
            Err(_) => closed(),
        }
    }

    /// The redacted lines, or the single marker line when anything exceeded its budget.
    pub fn redact(&self, capture: Capture) -> Vec<String> {
        if self.failed_closed || capture.over_budget {
            return vec![MARKER.to_owned()];
        }
        let mut matches_left = MATCHES_MAX;
        let mut lines = Vec::with_capacity(capture.lines.len());
        for line in capture.lines {
            match self.line(line, &mut matches_left) {
                Some(line) => lines.push(line),
                None => return vec![MARKER.to_owned()],
            }
        }
        lines
    }

    fn line(&self, line: String, matches_left: &mut usize) -> Option<String> {
        let Some(automaton) = &self.automaton else {
            return Some(line);
        };
        // Matches come in order of their end, so a longer secret that starts earlier can arrive
        // after a shorter one inside it: sort by start, then merge the union.
        let mut found_spans: Vec<(usize, usize)> = Vec::new();
        for found in automaton.find_overlapping_iter(line.as_bytes()) {
            *matches_left = matches_left.checked_sub(1)?;
            found_spans.push((found.start(), found.end()));
        }
        found_spans.sort_unstable();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for (start, end) in found_spans {
            match spans.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => spans.push((start, end)),
            }
        }
        if spans.is_empty() {
            return Some(line);
        }
        let mut out = String::with_capacity(line.len());
        let mut at = 0;
        for (start, end) in spans {
            out.push_str(&line[at..start]);
            out.push_str(MARKER);
            at = end;
        }
        out.push_str(&line[at..]);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(secrets: &[&str], lines: &[&str]) -> Vec<String> {
        let secrets: Vec<String> = secrets.iter().map(|s| (*s).to_owned()).collect();
        let mut capture = Capture::default();
        for line in lines {
            capture.push((*line).to_owned());
        }
        Redactor::new(&secrets).redact(capture)
    }

    #[test]
    fn a_secret_is_replaced_wherever_it_occurs() {
        assert_eq!(
            redact(
                &["s3cr3t-value"],
                &["a s3cr3t-value b s3cr3t-value", "none"]
            ),
            ["a <redacted> b <redacted>", "none"]
        );
    }

    #[test]
    fn a_short_secret_is_replaced_whole() {
        assert_eq!(redact(&["xyz"], &["pre xyz post"]), ["pre <redacted> post"]);
    }

    #[test]
    fn overlapping_and_prefix_sharing_secrets_leave_no_piece_behind() {
        let out = redact(
            &["abcdefghij", "ghijklmnopqrstuv", "abcdef"],
            &["x abcdefghijklmnopqrstuv y"],
        );
        assert_eq!(out, ["x <redacted> y"]);
    }

    #[test]
    fn a_long_secret_containing_a_shorter_one_is_replaced_whole() {
        // The short secret ends first; the long one starts earlier and must still win.
        let out = redact(
            &["QRSTUV", "ABCDEFGHIJKLMNOPQRSTUVWXYZ"],
            &["a ABCDEFGHIJKLMNOPQRSTUVWXYZ b"],
        );
        assert_eq!(out, ["a <redacted> b"]);
    }

    #[test]
    fn any_eight_consecutive_characters_of_a_secret_are_replaced() {
        // Seven characters of a longer secret are not a secret; eight are.
        assert_eq!(
            redact(
                &["ABCDEFGHIJKL"],
                &["x ABCDEFG y", "x BCDEFGHI y", "x GHIJKL y"]
            ),
            ["x ABCDEFG y", "x <redacted> y", "x GHIJKL y"]
        );
    }

    /// The most distinct secrets the budget admits, against the most lines at the output limit,
    /// finishes within a fixed time: under 1 s in release, 60 s in an unoptimised build.
    #[test]
    fn the_largest_secret_set_against_the_most_lines_is_bounded() {
        let secrets: Vec<String> = (0..16_000).map(|n| format!("SECRET-{n:08}")).collect();
        let total: usize = secrets.iter().map(String::len).sum();
        assert!(total <= SECRET_BYTES_MAX);
        let mut capture = Capture::default();
        for line in 0..1000 {
            let mut text = "x".repeat(8192 - 20);
            text.push_str(&secrets[line * 7]);
            capture.push(text);
        }
        let started = std::time::Instant::now();
        let lines = Redactor::new(&secrets).redact(capture);
        let bound = if cfg!(debug_assertions) { 60 } else { 1 };
        assert!(started.elapsed() < std::time::Duration::from_secs(bound));
        assert_eq!(lines.len(), 1000);
        assert!(lines.iter().all(|line| line.ends_with("<redacted>")));
    }

    #[test]
    fn a_line_over_the_scan_limit_fails_closed_for_that_line() {
        let long = "q".repeat(LINE_BYTES_MAX + 1);
        assert_eq!(
            redact(&["zzzzzzzzzz"], &[&long, "ok"]),
            ["<redacted>", "ok"]
        );
    }

    #[test]
    fn exceeding_the_input_budget_fails_closed_to_one_marker() {
        let line = "a".repeat(LINE_BYTES_MAX);
        let lines: Vec<&str> = std::iter::repeat_n(line.as_str(), 300).collect();
        assert_eq!(redact(&["zzzzzzzzzz"], &lines), ["<redacted>"]);
    }

    #[test]
    fn too_many_secret_bytes_fail_closed_to_one_marker() {
        let secret = "k".repeat(SECRET_BYTES_MAX + 1);
        assert_eq!(redact(&[secret.as_str()], &["plain"]), ["<redacted>"]);
    }
}
