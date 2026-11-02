//! Real Candidate lifecycle tests need their own supervisor process. Keep them
//! in this integration-test executable, apart from tests spawning trusted HTTP
//! references and testers: a process-wide subreaper adopts newly orphaned roots.
//! Candidate serializes its instances here; unrelated child spawns must not run
//! concurrently in this executable. Cargo can run other executables in parallel.

#[path = "../src/oci_contract.rs"]
pub mod contract;

use std::{
    fs, thread,
    time::{Duration, Instant},
};

#[test]
fn process_lifecycle_signals_kills_and_reaps_without_network() {
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = contract::Candidate::spawn(
        8080,
        std::ffi::OsStr::new("/bin/sleep"),
        &["60".into()],
        &probe,
    )
    .unwrap();
    let pid = child.pid();
    assert!(child.terminate().unwrap());
    drop(child);
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "child was not reaped"
    );
    let child = contract::Candidate::spawn(
        8080,
        std::ffi::OsStr::new("/bin/sleep"),
        &["60".into()],
        &probe,
    )
    .unwrap();
    let pid = child.pid();
    drop(child);
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "early-error cleanup"
    );
}

#[test]
fn sigterm_ignore_is_bounded_and_killed_after_ten_seconds() {
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = contract::Candidate::spawn(
        8080,
        std::ffi::OsStr::new("/bin/sh"),
        &[
            "-c".into(),
            "trap '' TERM; : > \"$TMPDIR/ready\"; exec /bin/sleep 60".into(),
        ],
        &probe,
    )
    .unwrap();
    let pid = child.pid();
    let ready = Instant::now();
    while !probe.scratch().join("ready").exists() {
        assert!(ready.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(10));
    }
    let start = Instant::now();
    assert!(!child.terminate().unwrap());
    drop(child);
    assert!(start.elapsed() >= Duration::from_secs(10));
    assert!(start.elapsed() < Duration::from_secs(15));
    assert!(rustix::process::test_kill_process(pid).is_err());
}

#[test]
fn cleanup_reaps_descendants_of_the_candidate() {
    let probe = contract::FilesystemProbe::new().unwrap();
    let child = contract::Candidate::spawn(
        8080,
        std::ffi::OsStr::new("/bin/sh"),
        &[
            "-c".into(),
            "/bin/sleep 60 & echo $! > \"$TMPDIR/child-pid\"; wait".into(),
        ],
        &probe,
    )
    .unwrap();
    let ready = Instant::now();
    let descendant = loop {
        if let Ok(text) = fs::read_to_string(probe.scratch().join("child-pid"))
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            break rustix::process::Pid::from_raw(pid).unwrap();
        }
        assert!(ready.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(10));
    };
    drop(child);
    assert!(
        rustix::process::test_kill_process(descendant).is_err(),
        "descendant was not killed/reaped"
    );
}

#[test]
fn escaped_sigterm_ignoring_child_is_killed_and_reaped() {
    use rustix::process::{
        Pid, PidfdFlags, Signal, WaitOptions, pidfd_open, pidfd_send_signal, waitpid,
    };
    let probe = contract::FilesystemProbe::new().unwrap();
    let mut child = contract::Candidate::spawn(8080, std::ffi::OsStr::new("/bin/sh"), &[
        "-c".into(),
        "/usr/bin/setsid /bin/sh -c 'trap \"\" TERM; echo $$ > \"$TMPDIR/escaped-pid\"; exec /bin/sleep 60' & wait".into(),
    ], &probe).unwrap();
    let ready = Instant::now();
    let descendant = loop {
        if let Ok(text) = fs::read_to_string(probe.scratch().join("escaped-pid"))
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            break Pid::from_raw(pid).unwrap();
        }
        assert!(ready.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(10));
    };
    // Keep a process handle so a red test also cleans up without numeric signals.
    let handle = pidfd_open(descendant, PidfdFlags::empty()).unwrap();
    let start = Instant::now();
    child.terminate().unwrap();
    drop(child);
    let survived = rustix::process::test_kill_process(descendant).is_ok();
    if survived {
        let _ = pidfd_send_signal(&handle, Signal::KILL);
        while waitpid(Some(descendant), WaitOptions::NOHANG)
            .ok()
            .flatten()
            .is_none()
            && start.elapsed() < contract::TERM_GRACE + Duration::from_secs(2)
        {
            thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(!survived, "escaped descendant survived cleanup");
    assert!(start.elapsed() < contract::TERM_GRACE + Duration::from_secs(2));
}
