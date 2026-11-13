//! RUN-01 execution API, local run. Starts the real `falcone-serverless-worker serve` on
//! loopback ephemeral ports and drives it over TCP with the qualification cases that also run
//! in the restricted fs-test pod (`cluster_execute_api.rs`). Guest-executing cases here are the
//! fast feedback loop; every security-relevant case also has its restricted-cluster run.

#![cfg(target_os = "linux")]

#[path = "../src/qualification/execution_api.rs"]
mod cases;

macro_rules! local_cases {
    ($($name:ident),* $(,)?) => {
        /// The names of the cases run here.
        const LOCAL: &[&str] = &[$(stringify!($name)),*];
        $(
            #[test]
            fn $name() {
                cases::$name();
            }
        )*
    };
}

local_cases! {
    identity,
    runtime_mode,
    credential,
    imports,
    budgets,
    timeouts,
    duplicate_attempt,
    saturation,
    control_stall,
    http_limits,
    body_deadline,
    slow_reader,
}

/// The in-pod runner serves exactly the cases run here: a case added to one and not the other
/// is caught.
#[test]
fn the_pod_runner_and_the_local_run_have_the_same_cases() {
    let served: Vec<String> = cases::CASES
        .iter()
        .map(|(name, _)| name.replace('-', "_"))
        .collect();
    assert_eq!(served, LOCAL);
}

#[test]
fn the_pod_runner_refuses_an_unknown_case() {
    assert_eq!(
        format!("{:?}", cases::run("no-such-case")),
        format!("{:?}", std::process::ExitCode::FAILURE)
    );
}

// The redactor is a pure module: its adversarial set runs here, where the release-mode bound is
// asserted (`cargo test --release`); debug builds get a generous bound instead.
mod redaction {
    use falcone_serverless_worker::redact::{MARKER, MAX_LINE_CHARS, MAX_LINES, Redactor};
    use std::time::{Duration, Instant};

    #[test]
    fn the_maximum_secret_set_and_full_logs_finish_within_the_documented_bound() {
        // 4 MiB of distinct credential strings (8 bytes each) and 1000 maximum-length lines
        // made of them. Documented bound: under 1 s in release.
        let count = 4 * 1024 * 1024 / 8;
        let secrets: Vec<String> = (0..count).map(|n| format!("{n:08x}")).collect();
        let lines: Vec<String> = (0..MAX_LINES)
            .map(|n| {
                let mut line = String::with_capacity(MAX_LINE_CHARS);
                while line.len() + 9 <= MAX_LINE_CHARS {
                    line.push_str(&secrets[(n * 97 + line.len()) % count]);
                    line.push(' ');
                }
                line
            })
            .collect();
        let started = Instant::now();
        let out = Redactor::new(secrets.iter().map(String::as_str)).lines(lines);
        let elapsed = started.elapsed();
        assert_eq!(out.len(), MAX_LINES);
        for line in &out {
            // Every secret was replaced; the last marker may be cut by the clip.
            let words: Vec<&str> = line.split_whitespace().collect();
            let (last, rest) = words.split_last().unwrap();
            assert!(rest.iter().all(|word| *word == MARKER));
            assert!(MARKER.starts_with(last));
        }
        let bound = if cfg!(debug_assertions) { 30 } else { 1 };
        assert!(
            elapsed < Duration::from_secs(bound),
            "redaction took {elapsed:?}"
        );
    }

    #[test]
    fn overlapping_and_prefix_sharing_secrets_are_fully_redacted_and_over_budget_fails_closed() {
        let redact = |secrets: &[&str], line: &str| {
            Redactor::new(secrets.iter().copied())
                .lines(vec![line.to_owned()])
                .remove(0)
        };
        assert_eq!(redact(&["abcd", "cdef"], "-abcdef-"), "-<redacted>-");
        assert_eq!(redact(&["abc", "abcdef"], "x abcdef y"), "x <redacted> y");
        let secret = "k".repeat(64);
        let tiny = Redactor::with_budget([secret.as_str()], 100);
        assert_eq!(
            tiny.lines(vec![secret.clone(), "plain".into()]),
            vec![MARKER.to_owned()]
        );
    }
}
