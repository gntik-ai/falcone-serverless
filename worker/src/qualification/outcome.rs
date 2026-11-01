//! Evidence classifiers shared by the real runner and orchestration tests.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Passed,
    Failed,
    Unverified,
}

#[derive(Debug, Serialize)]
pub struct Outcome {
    pub status: Status,
    pub reason: String,
    pub error_class: Option<&'static str>,
}

impl Outcome {
    fn passed(reason: impl Into<String>) -> Self {
        Self {
            status: Status::Passed,
            reason: reason.into(),
            error_class: None,
        }
    }

    pub fn failed(reason: impl Into<String>, error_class: &'static str) -> Self {
        Self {
            status: Status::Failed,
            reason: reason.into(),
            error_class: Some(error_class),
        }
    }

    pub fn unverified(reason: impl Into<String>) -> Self {
        Self {
            status: Status::Unverified,
            reason: reason.into(),
            error_class: None,
        }
    }
}

#[derive(Debug)]
pub enum RunError {
    Interrupt,
    OutOfFuel,
    Other(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interrupt => f.write_str("Interrupt"),
            Self::OutOfFuel => f.write_str("OutOfFuel"),
            Self::Other(reason) => f.write_str(reason),
        }
    }
}

pub type RunResult = Result<i32, RunError>;

pub fn operation(result: Result<(), String>, class: &'static str) -> Outcome {
    match result {
        Ok(()) => Outcome::passed(format!("{class} succeeded")),
        Err(reason) => Outcome::failed(reason, class),
    }
}

pub fn run_valid(result: RunResult) -> Outcome {
    match result {
        Ok(42) => Outcome::passed("run returned 42"),
        Ok(value) => Outcome::failed(format!("expected 42, got {value}"), "result"),
        Err(error) => Outcome::failed(error.to_string(), "trap"),
    }
}

pub fn interrupt_epoch(result: RunResult) -> Outcome {
    match result {
        Err(RunError::Interrupt) => Outcome::passed("observed epoch interruption"),
        other => Outcome::failed(format!("expected Interrupt, got {other:?}"), "trap"),
    }
}

pub fn interrupt_fuel(result: RunResult) -> Outcome {
    match result {
        Err(RunError::OutOfFuel) => Outcome::passed("observed fuel exhaustion"),
        other => Outcome::failed(format!("expected OutOfFuel, got {other:?}"), "trap"),
    }
}

pub fn root_fs_read_only(mountinfo: Option<&str>, write: Result<(), String>) -> Outcome {
    if write.is_ok() {
        return Outcome::failed("root write probe succeeded", "posture");
    }
    let options = mountinfo.and_then(|text| {
        text.lines()
            .rev()
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .find(|fields| fields.get(4) == Some(&"/"))
            .and_then(|fields| fields.get(5).copied())
    });
    match options {
        Some(options) if options.split(',').any(|option| option == "ro") => {
            Outcome::passed("root mount is ro and write probe failed")
        }
        Some(options) if options.split(',').any(|option| option == "rw") => {
            Outcome::failed("root mount is rw despite failed write probe", "posture")
        }
        _ => Outcome::unverified("root mountinfo evidence unavailable"),
    }
}

pub fn tmp_writable(round_trip: Result<(), String>) -> Outcome {
    match round_trip {
        Ok(()) => Outcome::passed("/tmp random payload readback verified"),
        Err(reason) => Outcome::failed(reason, "posture"),
    }
}

pub fn non_root_uid(status: Option<&str>) -> Outcome {
    let ids = |key: &str| -> Option<Vec<u32>> {
        let line = status?.lines().find_map(|line| line.strip_prefix(key))?;
        let ids = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<u32>, _>>()
            .ok()?;
        (ids.len() == 4).then_some(ids)
    };
    let (Some(uids), Some(gids)) = (ids("Uid:"), ids("Gid:")) else {
        return Outcome::unverified("/proc/self/status identity evidence unavailable");
    };
    let reason = format!(
        "uid={} gid={} (effective); uid set={uids:?}, gid set={gids:?}",
        uids[1], gids[1]
    );
    if uids.contains(&0) {
        Outcome::failed(reason, "posture")
    } else {
        Outcome::passed(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmp_failures_are_posture_failures() {
        for error in ["write denied", "read denied", "readback differs"] {
            let result = tmp_writable(Err(error.into()));
            assert_eq!(result.status, Status::Failed);
            assert_eq!(result.error_class, Some("posture"));
        }
    }

    fn status(outcome: Outcome) -> String {
        serde_json::to_value(outcome).unwrap()["status"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn execution_requires_the_exact_result_or_trap() {
        assert_eq!(status(run_valid(Ok(42))), "passed");
        assert_eq!(status(run_valid(Ok(41))), "failed");
        assert_eq!(status(interrupt_epoch(Err(RunError::Interrupt))), "passed");
        for result in [
            Err(RunError::OutOfFuel),
            Err(RunError::Other("stack overflow".into())),
            Ok(0),
        ] {
            assert_eq!(status(interrupt_epoch(result)), "failed");
        }
        assert_eq!(status(interrupt_fuel(Err(RunError::OutOfFuel))), "passed");
        for result in [
            Err(RunError::Interrupt),
            Err(RunError::Other("stack overflow".into())),
            Ok(0),
        ] {
            assert_eq!(status(interrupt_fuel(result)), "failed");
        }
        assert_eq!(status(run_valid(Err(RunError::OutOfFuel))), "failed");
    }

    #[test]
    fn compile_and_load_errors_keep_their_class() {
        for class in ["compile", "load"] {
            let outcome = operation(Err("denied".into()), class);
            assert_eq!(outcome.error_class, Some(class));
            assert_eq!(status(outcome), "failed");
            assert_eq!(status(operation(Ok(()), class)), "passed");
        }
    }
    #[test]
    fn filesystem_evidence_is_required_and_last_root_mount_wins() {
        let ro = "1 0 0:1 / / ro,relatime - overlay overlay rw\n";
        let rw = "2 0 0:1 / / rw,relatime - overlay overlay rw\n";
        assert_eq!(
            status(root_fs_read_only(Some(ro), Err("read only".into()))),
            "passed"
        );
        assert_eq!(
            status(root_fs_read_only(Some(rw), Err("permission denied".into()))),
            "failed"
        );
        assert_eq!(
            status(root_fs_read_only(
                Some(&format!("{ro}{rw}")),
                Err("denied".into())
            )),
            "failed"
        );
        assert_eq!(
            status(root_fs_read_only(
                Some(&format!("{rw}{ro}")),
                Err("denied".into())
            )),
            "passed"
        );
        for evidence in [None, Some(""), Some("1 0 0:1 / /tmp ro - tmpfs tmpfs rw")] {
            assert_eq!(
                status(root_fs_read_only(evidence, Err("denied".into()))),
                "unverified"
            );
        }
        assert_eq!(status(root_fs_read_only(Some(ro), Ok(()))), "failed");
        assert_eq!(status(root_fs_read_only(None, Ok(()))), "failed");
        assert_eq!(status(tmp_writable(Ok(()))), "passed");
        assert_eq!(status(tmp_writable(Err("denied".into()))), "failed");
    }

    #[test]
    fn missing_options_on_the_last_root_mount_are_not_read_only_evidence() {
        let mountinfo = "1 0 0:1 / / ro - overlay overlay rw\n2 0 0:1 / /";
        assert_eq!(
            status(root_fs_read_only(Some(mountinfo), Err("denied".into()))),
            "unverified"
        );
    }

    #[test]
    fn non_root_identity_requires_proc_evidence() {
        let status_text = "Name:\trunner\nUid:\t1000770000\t1000770000\t1000770000\t1000770000\nGid:\t1000770000\t1000770000\t1000770000\t1000770000\n";
        let outcome = non_root_uid(Some(status_text));
        assert!(outcome.reason.contains("uid=1000770000"));
        assert!(outcome.reason.contains("gid=1000770000"));
        assert_eq!(status(outcome), "passed");
        assert_eq!(
            status(non_root_uid(Some(&status_text.replace("1000770000", "0")))),
            "failed"
        );
        assert_eq!(
            status(non_root_uid(Some(&status_text.replacen(
                "1000770000",
                "0",
                1
            )))),
            "failed"
        );
        for evidence in [None, Some(""), Some("Uid: unknown\nGid: unknown")] {
            assert_eq!(status(non_root_uid(evidence)), "unverified");
        }
    }
}
