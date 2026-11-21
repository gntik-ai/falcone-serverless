//! Bounded, fail-closed redaction of guest log text.
//!
//! The secrets are the claim strings of the execution credential. A secret of eight or more
//! characters is protected by its eight-character windows: no output line contains eight
//! consecutive characters of it. A secret shorter than eight characters is protected whole.
//! Matching is by character, so a replaced run never splits a character.
//!
//! The work is bounded three times. Building the matcher is capped by [`MAX_WINDOWS`] distinct
//! windows and [`MAX_BUILD_STEPS`] examined windows, both enforced as each window is inserted,
//! and it ends at the execution's deadline. Scanning is capped per execution by
//! [`SCAN_BUDGET_CHARS`]. When any cap is exceeded the execution fails closed: every log line is dropped and the logs become the single line
//! [`MARKER`]. Text is redacted in full and only then cut to [`LINE_CHARS`], so a secret that
//! crosses the cut point is gone before the cut.

use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// What replaces a protected run, and the only line of an over-budget execution.
pub const MARKER: &str = "<redacted>";
/// The most characters of one log line in a result (`logs` of the result schema).
pub const LINE_CHARS: usize = 8192;
/// The most log lines of a result.
pub const MAX_LINES: usize = 1000;
/// The most distinct secret windows the matcher holds.
pub const MAX_WINDOWS: usize = 1 << 20;
/// The most characters one execution scans for redaction.
pub const SCAN_BUDGET_CHARS: usize = 4 << 20;
/// The most secret windows one build examines, duplicates included.
pub const MAX_BUILD_STEPS: usize = 4 << 20;
/// How many windows a build examines between looks at the clock.
const CLOCK_INTERVAL: usize = 4096;
const WINDOW: usize = 8;

/// Keys are already mixed; the set uses them as their own hash.
#[derive(Default)]
struct Identity(u64);

impl Hasher for Identity {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(*byte);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

type Keys = HashSet<u64, BuildHasherDefault<Identity>>;

fn step(hash: u64, c: char) -> u64 {
    (hash ^ u64::from(u32::from(c))).wrapping_mul(0x0000_0100_0000_01b3)
}

/// The set key of a window of `length` characters with FNV-1a hash `hash`.
fn key(hash: u64, length: usize) -> u64 {
    let mut z = hash ^ (length as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The redaction matcher of one execution.
pub struct Redactor {
    keys: Keys,
    /// Which lengths from one to `WINDOW` have any secret, so absent lengths cost nothing.
    lengths: [bool; WINDOW + 1],
    starts: [bool; 256],
    /// Set when building the matcher exceeded a bound: every log is replaced by the marker.
    overflow: bool,
    /// How many secret windows building the matcher examined.
    examined: usize,
}

impl Redactor {
    /// Builds the matcher over the secrets' characters, in work linear in their total length.
    ///
    /// The bounds are enforced as each window is inserted, never after a whole claim: the set
    /// stops at [`MAX_WINDOWS`] distinct windows, the build examines at most
    /// [`MAX_BUILD_STEPS`] windows, and it looks at the clock and at `cancelled` as it goes, so
    /// it ends at `until` or at a cancel whatever the claims hold. Past any bound, or on either
    /// interruption, the matcher fails closed; the caller's outcome is then the interruption.
    pub fn new<'a>(
        secrets: impl IntoIterator<Item = &'a str>,
        until: Instant,
        cancelled: &AtomicBool,
    ) -> Self {
        let mut redactor = Self {
            keys: Keys::default(),
            lengths: [false; WINDOW + 1],
            starts: [false; 256],
            overflow: false,
            examined: 0,
        };
        'build: for secret in secrets {
            let chars: Vec<char> = secret.chars().collect();
            if chars.is_empty() {
                continue;
            }
            let length = chars.len().min(WINDOW);
            redactor.lengths[length] = true;
            for window in chars.windows(length) {
                if redactor.examined >= MAX_BUILD_STEPS
                    || (redactor.examined.is_multiple_of(CLOCK_INTERVAL)
                        && (cancelled.load(Ordering::Acquire) || Instant::now() >= until))
                {
                    redactor.fail_closed();
                    break 'build;
                }
                redactor.examined += 1;
                redactor.starts[start(window[0])] = true;
                let hash = window
                    .iter()
                    .fold(0xcbf2_9ce4_8422_2325, |h, c| step(h, *c));
                redactor.keys.insert(key(hash, length));
                if redactor.keys.len() > MAX_WINDOWS {
                    redactor.fail_closed();
                    break 'build;
                }
            }
        }
        redactor
    }

    fn fail_closed(&mut self) {
        self.keys = Keys::default();
        self.overflow = true;
    }

    /// How many secret windows building the matcher examined.
    #[cfg(test)]
    pub fn examined(&self) -> usize {
        self.examined
    }

    /// Whether the secrets were too many for the matcher.
    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    /// Redacts `text` in full, then cuts the result to [`LINE_CHARS`]. `budget` is the
    /// execution's remaining scan budget in characters; `None` is returned, and the caller
    /// must fail closed, when the text does not fit.
    pub fn line(&self, text: &str, budget: &mut usize) -> Option<String> {
        if self.overflow {
            return None;
        }
        let chars: Vec<char> = text.chars().collect();
        *budget = budget.checked_sub(chars.len())?;
        let mut covered = vec![false; chars.len()];
        for at in 0..chars.len() {
            if !self.starts[start(chars[at])] {
                continue;
            }
            let mut hash = 0xcbf2_9ce4_8422_2325;
            for length in 1..=WINDOW.min(chars.len() - at) {
                hash = step(hash, chars[at + length - 1]);
                if self.lengths[length] && self.keys.contains(&key(hash, length)) {
                    covered[at..at + length].fill(true);
                }
            }
        }
        // Replace each covered run by one marker, stopping once the line is full.
        let mut out = String::new();
        let mut produced = 0;
        let mut in_run = false;
        for (c, hit) in chars.iter().zip(&covered) {
            if produced >= LINE_CHARS {
                break;
            }
            if *hit {
                if !in_run {
                    for m in MARKER.chars().take(LINE_CHARS - produced) {
                        out.push(m);
                        produced += 1;
                    }
                }
                in_run = true;
            } else {
                in_run = false;
                out.push(*c);
                produced += 1;
            }
        }
        Some(out)
    }
}

fn start(c: char) -> usize {
    (u32::from(c).wrapping_mul(0x9e37_79b1) >> 24) as usize
}

/// The collected, already redacted log lines of one execution.
pub struct Logs {
    redactor: Redactor,
    lines: Vec<String>,
    budget: usize,
    failed_closed: bool,
}

impl Logs {
    pub fn new(redactor: Redactor) -> Self {
        let failed_closed = redactor.overflowed();
        Self {
            redactor,
            lines: Vec::new(),
            budget: SCAN_BUDGET_CHARS,
            failed_closed,
        }
    }

    /// Adds one log line, redacted. Past [`MAX_LINES`] lines are dropped; past the scan
    /// budget the execution fails closed.
    pub fn push(&mut self, text: &str) {
        if self.failed_closed || self.lines.len() >= MAX_LINES {
            return;
        }
        match self.redactor.line(text, &mut self.budget) {
            Some(line) => self.lines.push(line),
            None => {
                self.failed_closed = true;
                self.lines.clear();
            }
        }
    }

    /// The lines of the result: every one redacted, or the single marker line.
    pub fn finish(&mut self) -> Vec<String> {
        if self.failed_closed {
            vec![MARKER.to_owned()]
        } else {
            std::mem::take(&mut self.lines)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(600)
    }

    fn live() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn full_stdout_lines_are_kept_whole_by_the_record() {
        // A 65,536-byte stdout flush is 16 lines of STDOUT_LINE_CHARS, as platform formats them.
        let mut logs = Logs::new(Redactor::new(["not-in-output"], soon(), &live()));
        let line = format!(
            "info {} stream=stdout",
            "x".repeat(crate::host::STDOUT_LINE_CHARS)
        );
        for _ in 0..65_536 / crate::host::STDOUT_LINE_CHARS {
            logs.push(&line);
        }
        let kept = logs.finish();
        assert_eq!(kept.len(), 16);
        assert!(kept.iter().all(|kept| *kept == line));
    }

    fn redact(secrets: &[&str], text: &str) -> String {
        let redactor = Redactor::new(secrets.iter().copied(), soon(), &live());
        redactor.line(text, &mut SCAN_BUDGET_CHARS.clone()).unwrap()
    }

    #[test]
    fn a_secret_of_eight_characters_or_more_loses_every_eight_character_window() {
        assert_eq!(redact(&["abcdefghijkl"], "x abcdefgh y"), "x <redacted> y");
        assert_eq!(redact(&["abcdefghijkl"], "x efghijkl y"), "x <redacted> y");
        // Seven characters of the secret are allowed through.
        assert_eq!(redact(&["abcdefghijkl"], "abcdefg"), "abcdefg");
    }

    #[test]
    fn a_short_secret_is_redacted_whole_and_only_whole() {
        assert_eq!(
            redact(&["pw1234"], "a pw1234 b pw123 c"),
            "a <redacted> b pw123 c"
        );
    }

    #[test]
    fn overlapping_and_prefix_sharing_secrets_are_fully_redacted() {
        let out = redact(
            &["abcdefghij", "abcdefghijklmnop", "ijklmnopqr"],
            "<abcdefghijklmnopqr>",
        );
        assert_eq!(out, "<<redacted>>");
    }

    #[test]
    fn multibyte_characters_are_never_split() {
        let out = redact(&["ééééééééé"], "a ééééééééé b");
        assert_eq!(out, "a <redacted> b");
    }

    #[test]
    fn redaction_precedes_the_cut() {
        let secret = "s".repeat(9000);
        let out = redact(&[&secret], &format!("{secret} tail"));
        assert_eq!(out, "<redacted> tail");
        let crossing = "k3y!k3y!k3y!";
        let text = format!("{}{crossing}", "a".repeat(LINE_CHARS - 5));
        let out = redact(&[crossing], &text);
        assert!(!out.contains("k3y!k3y!"), "{}", &out[out.len() - 20..]);
        assert!(out.chars().count() <= LINE_CHARS);
    }

    #[test]
    fn the_output_never_exceeds_the_line_limit() {
        let out = redact(&["a"], &"a".repeat(100_000));
        assert_eq!(out.chars().count(), MARKER.chars().count());
        let out = redact(&["a"], &"ab".repeat(100_000));
        assert_eq!(out.chars().count(), LINE_CHARS);
    }

    #[test]
    fn over_budget_fails_closed_to_one_marker_line() {
        let mut logs = Logs::new(Redactor::new(["secretvalue"], soon(), &live()));
        logs.push("kept line");
        for _ in 0..(SCAN_BUDGET_CHARS / 8192 + 2) {
            logs.push(&"x".repeat(8192));
        }
        assert_eq!(logs.finish(), [MARKER]);
        let mut logs = Logs::new(Redactor::new(
            (0..MAX_WINDOWS + 1)
                .map(|i| format!("{i:012}"))
                .collect::<Vec<_>>()
                .iter()
                .map(String::as_str),
            soon(),
            &live(),
        ));
        logs.push("anything");
        assert_eq!(logs.finish(), [MARKER]);
    }

    /// A string of `length` characters in which every eight-character window is distinct: the
    /// characters come from a 20,992-symbol alphabet by a full-period generator.
    fn distinct(length: usize, seed: u64) -> String {
        let mut state = seed;
        (0..length)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                char::from_u32(0x4E00 + ((state >> 33) % 20_992) as u32).unwrap()
            })
            .collect()
    }

    #[test]
    fn an_oversized_claim_stops_at_the_cap_while_it_is_inserted_not_after() {
        // 2.5 M characters is as much claim as one request can carry: far past the cap.
        let claim = distinct(2_500_000, 7);
        let started = Instant::now();
        let redactor = Redactor::new([claim.as_str()], soon(), &live());
        let elapsed = started.elapsed();
        assert!(redactor.overflowed());
        // The cap is exact: the window that crosses it is the last one examined.
        assert_eq!(redactor.examined(), MAX_WINDOWS + 1);
        // A release build builds this in well under a second; a debug build gets a wide bound.
        let bound = if cfg!(debug_assertions) { 20 } else { 1 };
        assert!(elapsed < Duration::from_secs(bound), "{elapsed:?}");
        // And the answer is exactly the marker line, never text.
        let mut logs = Logs::new(redactor);
        logs.push(&claim.chars().take(64).collect::<String>());
        assert_eq!(logs.finish(), [MARKER]);
    }

    #[test]
    fn the_work_over_repeated_claims_is_bounded_by_the_fixed_step_budget() {
        // The same window over and over never grows the set, so only the step budget stops it.
        let repeated = "ab".repeat(1 << 20);
        let redactor = Redactor::new([repeated.as_str(); 8], soon(), &live());
        assert!(redactor.overflowed());
        assert_eq!(redactor.examined(), MAX_BUILD_STEPS);
    }

    #[test]
    fn construction_under_a_passed_deadline_fails_closed_after_at_most_one_clock_interval() {
        let claim = distinct(500_000, 11);
        let redactor = Redactor::new([claim.as_str()], Instant::now(), &live());
        assert!(redactor.overflowed());
        assert_eq!(redactor.examined(), 0);
        let mut logs = Logs::new(redactor);
        logs.push("anything");
        assert_eq!(logs.finish(), [MARKER]);
    }

    #[test]
    fn a_cancel_during_construction_ends_it_within_one_clock_interval() {
        // The claims are an iterator that raises the cancel flag when the build asks for the
        // second one: the cancel arrives while the matcher is being built, at a point the
        // test controls, so no timing is involved.
        // The first claim ends on a clock interval, where the next look at the flag falls.
        let first = distinct(10 * CLOCK_INTERVAL + WINDOW - 1, 5);
        let second = distinct(200_000, 6);
        let cancelled = AtomicBool::new(false);
        let claims = [first.as_str(), second.as_str()]
            .into_iter()
            .enumerate()
            .map(|(index, claim)| {
                if index == 1 {
                    cancelled.store(true, Ordering::Release);
                }
                claim
            });
        let redactor = Redactor::new(claims, soon(), &cancelled);
        assert!(redactor.overflowed());
        // The first claim was inserted in full; the second stopped at its first window.
        assert_eq!(redactor.examined(), 10 * CLOCK_INTERVAL);
        let mut logs = Logs::new(redactor);
        logs.push("anything");
        assert_eq!(logs.finish(), [MARKER]);
    }

    #[test]
    fn a_cancel_that_is_already_set_stops_construction_before_the_first_window() {
        let claim = distinct(500_000, 13);
        let cancelled = AtomicBool::new(true);
        let redactor = Redactor::new([claim.as_str()], soon(), &cancelled);
        assert!(redactor.overflowed());
        assert_eq!(redactor.examined(), 0);
    }
}
