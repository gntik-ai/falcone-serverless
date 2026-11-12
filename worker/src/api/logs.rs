//! Bounded, fail-closed redaction of the log lines of one execution.
//!
//! The credential strings are collected once and compiled into one leftmost-longest
//! Aho-Corasick matcher per execution. Every line is cut to `MAX_LINE_CHARS` characters before it
//! is scanned, the lines are capped at `MAX_LINES`, and the whole execution shares one fixed work
//! budget (`WORK_BUDGET` units: the secret set, plus the bytes the matcher reads). A secret set
//! or a scan that would go over any limit never degrades to unredacted text: the logs are
//! dropped and one `MARKER` line stands in for them.
//!
//! A match is expanded to the union of every occurrence that overlaps it, so secrets that
//! overlap or share a prefix are redacted whole; leftmost-longest matching alone would leave the
//! tail of the second of two overlapping secrets.

use super::wire::redacted;
use aho_corasick::{AhoCorasick, Input, MatchKind};
use std::collections::HashSet;

/// What replaces a secret, and the only line of logs that went over budget.
pub const MARKER: &str = "<redacted>";
/// The most log lines of a result (`logs` of the result schema).
pub const MAX_LINES: usize = 1000;
/// The most characters of one log line (`logs` of the result schema).
pub const MAX_LINE_CHARS: usize = 8192;
/// A shorter string is not treated as a secret: a one-byte secret would redact every occurrence of
/// a letter. It also bounds how far the marker can grow a line.
pub const MIN_SECRET_BYTES: usize = 4;
/// The longest single secret.
pub const MAX_SECRET_BYTES: usize = 4096;
/// The most distinct secrets.
pub const MAX_SECRETS: usize = 4096;
/// The most bytes of distinct secrets together.
pub const MAX_SECRET_SET_BYTES: usize = 256 * 1024;
/// The fixed total work of one execution's redaction, in bytes.
pub const WORK_BUDGET: usize = 16 * 1024 * 1024;

enum Mode {
    /// The secret set was over budget: every log is replaced by the marker.
    Closed,
    /// No secret of at least `MIN_SECRET_BYTES`: lines are only cut.
    Plain,
    Matching {
        matcher: AhoCorasick,
        /// The longest secret, in bytes: how far past the cut a straddling secret can reach.
        guard: usize,
        set_bytes: usize,
    },
}

/// The redaction of one execution, built once from its credential strings.
pub struct Redactor {
    mode: Mode,
}
redacted!(Redactor);

impl Redactor {
    pub fn new<'a>(secrets: impl IntoIterator<Item = &'a str>) -> Self {
        let mut distinct: HashSet<&str> = HashSet::new();
        let (mut set_bytes, mut guard) = (0usize, 0usize);
        for secret in secrets {
            if secret.len() < MIN_SECRET_BYTES {
                continue;
            }
            if secret.len() > MAX_SECRET_BYTES {
                return Self::closed();
            }
            if !distinct.insert(secret) {
                continue;
            }
            set_bytes += secret.len();
            guard = guard.max(secret.len());
            if distinct.len() > MAX_SECRETS || set_bytes > MAX_SECRET_SET_BYTES {
                return Self::closed();
            }
        }
        if distinct.is_empty() {
            return Self { mode: Mode::Plain };
        }
        match AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(distinct)
        {
            Ok(matcher) => Self {
                mode: Mode::Matching {
                    matcher,
                    guard,
                    set_bytes,
                },
            },
            Err(_) => Self::closed(),
        }
    }

    fn closed() -> Self {
        Self { mode: Mode::Closed }
    }

    /// The lines to return: at most `MAX_LINES`, each at most `MAX_LINE_CHARS` characters, with
    /// every secret replaced by `MARKER`. Over budget, only one `MARKER` line.
    pub fn redact(&self, lines: Vec<String>) -> Vec<String> {
        if lines.is_empty() {
            return Vec::new();
        }
        let (matcher, guard, set_bytes) = match &self.mode {
            Mode::Closed => return vec![MARKER.to_owned()],
            Mode::Plain => {
                return lines
                    .into_iter()
                    .take(MAX_LINES)
                    .map(|line| cut(line, MAX_LINE_CHARS))
                    .collect();
            }
            Mode::Matching {
                matcher,
                guard,
                set_bytes,
            } => (matcher, *guard, *set_bytes),
        };
        let mut work = set_bytes;
        let mut out = Vec::with_capacity(lines.len().min(MAX_LINES));
        for line in lines.into_iter().take(MAX_LINES) {
            let cut_at = line
                .char_indices()
                .nth(MAX_LINE_CHARS)
                .map_or(line.len(), |(at, _)| at);
            // A secret that starts before the cut can end after it: scan as far as the longest
            // secret reaches to recognise it, but emit only text from before the cut.
            let mut end = if cut_at < line.len() {
                (cut_at + guard).min(line.len())
            } else {
                cut_at
            };
            while !line.is_char_boundary(end) {
                end += 1;
            }
            work = work.saturating_add(end);
            if work > WORK_BUDGET {
                return vec![MARKER.to_owned()];
            }
            let Some(blocks) = occurrences(matcher, &line[..end], &mut work) else {
                return vec![MARKER.to_owned()];
            };
            out.push(cut(replace(&line[..end], &blocks, cut_at), MAX_LINE_CHARS));
        }
        out
    }
}

/// The byte ranges of `text` covered by any secret, overlapping occurrences merged. `None` when
/// the work budget is exhausted.
fn occurrences(matcher: &AhoCorasick, text: &str, work: &mut usize) -> Option<Vec<(usize, usize)>> {
    let mut find = |from: usize| -> Option<Option<(usize, usize)>> {
        let found = matcher.find(Input::new(text).span(from..text.len()));
        let reached = found.map_or(text.len(), |found| found.end());
        *work = work.saturating_add(reached.saturating_sub(from).max(1));
        (*work <= WORK_BUDGET).then_some(found.map(|found| (found.start(), found.end())))
    };
    let mut blocks = Vec::new();
    let mut next = find(0)?;
    while let Some((start, mut end)) = next {
        // Every occurrence that starts inside the block extends it. Probing from start + 1 visits
        // each start position in order; the first one at or past `end` is the next block's start.
        let mut from = start + 1;
        next = loop {
            match find(from)? {
                Some((probe_start, probe_end)) if probe_start < end => {
                    end = end.max(probe_end);
                    from = probe_start + 1;
                }
                other => break other,
            }
        };
        blocks.push((start, end));
    }
    Some(blocks)
}

/// `text` with each block replaced by `MARKER`, showing only what lies before `cut_at`. The bytes
/// past `cut_at` were scanned only to recognise a secret that starts before it; a block that
/// starts before the cut is replaced whole, and no plain text from past the cut is emitted, so a
/// replacement that is shorter than its secret never pulls later text into the line.
fn replace(text: &str, blocks: &[(usize, usize)], cut_at: usize) -> String {
    let mut out = String::with_capacity(cut_at.min(text.len()) + MARKER.len());
    let mut copied = 0;
    for &(start, end) in blocks {
        if start >= cut_at {
            break;
        }
        out.push_str(&text[copied..start]);
        out.push_str(MARKER);
        copied = end;
    }
    if copied < cut_at {
        out.push_str(&text[copied..cut_at]);
    }
    out
}

fn cut(mut line: String, chars: usize) -> String {
    if let Some((at, _)) = line.char_indices().nth(chars) {
        line.truncate(at);
    }
    line
}
