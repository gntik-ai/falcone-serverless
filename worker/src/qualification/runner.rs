//! Qualification sequencing; the harness is the execution and OS boundary.
use crate::outcome::{Outcome, RunResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Standard,
    EpochOnly,
    FuelOnly,
}

pub trait Harness {
    fn engine_config(&mut self) -> Result<(), String>;
    fn compile_valid(&mut self) -> Result<(), String>;
    fn load_executable_memory(&mut self) -> Result<(), String>;
    fn start_ticker(&mut self, tick_ms: u64) -> Result<(), String>;
    fn stop_ticker(&mut self) -> Result<(), String>;
    fn run(&mut self, mode: Mode) -> RunResult;
    fn root_fs_read_only(&mut self) -> Outcome;
    fn tmp_writable(&mut self) -> Outcome;
    fn non_root_uid(&mut self) -> Outcome;
}

fn timed_run(
    harness: &mut impl Harness,
    mode: Mode,
    classify: fn(RunResult) -> Outcome,
) -> Outcome {
    if let Err(reason) = harness.start_ticker(crate::settings::EPOCH_TICK_MS) {
        return Outcome::failed(reason, "ticker");
    }
    let result = harness.run(mode);
    match harness.stop_ticker() {
        Ok(()) => classify(result),
        Err(reason) => Outcome::failed(reason, "ticker"),
    }
}

pub fn run_checks(harness: &mut impl Harness, mut out: impl std::io::Write) -> std::io::Result<u8> {
    use crate::outcome::{self, Status};
    let engine = outcome::operation(harness.engine_config(), "engine");
    let compile = if engine.status == Status::Failed {
        Outcome::unverified("engine unavailable; compilation not attempted")
    } else {
        outcome::operation(harness.compile_valid(), "compile")
    };
    let load = if compile.status != Status::Failed && compile.status != Status::Unverified {
        outcome::operation(harness.load_executable_memory(), "load")
    } else {
        Outcome::unverified("compiled fixtures unavailable; load not attempted")
    };
    let runs = if load.status != Status::Failed && load.status != Status::Unverified {
        [
            timed_run(harness, Mode::Standard, outcome::run_valid),
            timed_run(harness, Mode::EpochOnly, outcome::interrupt_epoch),
            outcome::interrupt_fuel(harness.run(Mode::FuelOnly)),
        ]
    } else {
        std::array::from_fn(|_| {
            Outcome::unverified("compile/load evidence unavailable; run not attempted")
        })
    };
    let [valid, epoch, fuel] = runs;
    let records = [
        ("engine_config", engine),
        ("compile_valid", compile),
        ("load_executable_memory", load),
        ("run_valid", valid),
        ("interrupt_epoch", epoch),
        ("interrupt_fuel", fuel),
        ("root_fs_read_only", harness.root_fs_read_only()),
        ("tmp_writable", harness.tmp_writable()),
        ("non_root_uid", harness.non_root_uid()),
    ];
    let mut failed = false;
    let mut unverified = false;
    for (check, outcome) in records {
        failed |= outcome.status == Status::Failed;
        unverified |= outcome.status == Status::Unverified;
        serde_json::to_writer(
            &mut out,
            &serde_json::json!({
                "check": check, "status": outcome.status,
                "reason": outcome.reason, "error_class": outcome.error_class,
            }),
        )?;
        writeln!(out)?;
    }
    out.flush()?;
    Ok(if failed {
        1
    } else if unverified {
        2
    } else {
        0
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{self, RunError};
    use serde_json::{Value, json};

    #[derive(Default)]
    struct Fake {
        events: Vec<String>,
        compile_error: bool,
        load_error: bool,
        root: u8,
        wrong_result: bool,
        ticker_error: bool,
    }

    impl Harness for Fake {
        fn engine_config(&mut self) -> Result<(), String> {
            self.events.push("engine".into());
            Ok(())
        }
        fn compile_valid(&mut self) -> Result<(), String> {
            self.events.push("compile".into());
            if self.compile_error {
                Err("compile denied".into())
            } else {
                Ok(())
            }
        }
        fn load_executable_memory(&mut self) -> Result<(), String> {
            self.events.push("load".into());
            if self.load_error {
                Err("load denied".into())
            } else {
                Ok(())
            }
        }
        fn start_ticker(&mut self, tick_ms: u64) -> Result<(), String> {
            self.events.push(format!("start:{tick_ms}"));
            if self.ticker_error {
                Err("ticker unavailable".into())
            } else {
                Ok(())
            }
        }
        fn stop_ticker(&mut self) -> Result<(), String> {
            self.events.push("stop".into());
            Ok(())
        }
        fn run(&mut self, mode: Mode) -> RunResult {
            self.events.push(format!("run:{mode:?}"));
            match mode {
                Mode::Standard => Ok(if self.wrong_result { 41 } else { 42 }),
                Mode::EpochOnly => Err(RunError::Interrupt),
                Mode::FuelOnly => Err(RunError::OutOfFuel),
            }
        }
        fn root_fs_read_only(&mut self) -> Outcome {
            self.events.push("root".into());
            match self.root {
                0 => outcome::root_fs_read_only(
                    Some("1 0 0:1 / / ro - overlay overlay rw"),
                    Err("read only".into()),
                ),
                1 => outcome::root_fs_read_only(None, Err("denied".into())),
                _ => outcome::root_fs_read_only(
                    Some("1 0 0:1 / / rw - overlay overlay rw"),
                    Err("denied".into()),
                ),
            }
        }
        fn tmp_writable(&mut self) -> Outcome {
            self.events.push("tmp".into());
            outcome::tmp_writable(Ok(()))
        }
        fn non_root_uid(&mut self) -> Outcome {
            self.events.push("uid".into());
            outcome::non_root_uid(Some(
                "Uid: 1000770000 1000770000 1000770000 1000770000\nGid: 1000770000 1000770000 1000770000 1000770000",
            ))
        }
    }

    fn execute(fake: &mut Fake) -> (u8, Vec<Value>) {
        let mut bytes = Vec::new();
        let code = run_checks(fake, &mut bytes).unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.ends_with('\n'));
        let lines = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 9);
        (code, lines)
    }

    #[test]
    fn nine_records_and_ticker_boundaries_match_observed_evidence() {
        let mut fake = Fake::default();
        let (code, lines) = execute(&mut fake);
        assert_eq!(code, 0);
        assert_eq!(
            fake.events,
            [
                "engine",
                "compile",
                "load",
                "start:10",
                "run:Standard",
                "stop",
                "start:10",
                "run:EpochOnly",
                "stop",
                "run:FuelOnly",
                "root",
                "tmp",
                "uid"
            ]
        );
        let expected = [
            ("engine_config", "engine succeeded"),
            ("compile_valid", "compile succeeded"),
            ("load_executable_memory", "load succeeded"),
            ("run_valid", "run returned 42"),
            ("interrupt_epoch", "observed epoch interruption"),
            ("interrupt_fuel", "observed fuel exhaustion"),
            (
                "root_fs_read_only",
                "root mount is ro and write probe failed",
            ),
            ("tmp_writable", "/tmp random payload readback verified"),
            (
                "non_root_uid",
                "uid=1000770000 gid=1000770000 (effective); uid set=[1000770000, 1000770000, 1000770000, 1000770000], gid set=[1000770000, 1000770000, 1000770000, 1000770000]",
            ),
        ];
        for (line, (check, reason)) in lines.iter().zip(expected) {
            assert_eq!(
                *line,
                json!({"check": check, "status": "passed", "reason": reason, "error_class": null})
            );
        }
    }

    #[test]
    fn failed_evidence_takes_precedence_over_unverified() {
        assert_eq!(
            execute(&mut Fake {
                root: 1,
                ..Fake::default()
            })
            .0,
            2
        );
        assert_eq!(
            execute(&mut Fake {
                root: 2,
                ..Fake::default()
            })
            .0,
            1
        );
        assert_eq!(
            execute(&mut Fake {
                root: 1,
                wrong_result: true,
                ..Fake::default()
            })
            .0,
            1
        );
    }

    #[test]
    fn compile_or_load_failure_never_runs_a_guest() {
        for compile_error in [true, false] {
            let mut fake = Fake {
                compile_error,
                load_error: !compile_error,
                ..Fake::default()
            };
            let (code, lines) = execute(&mut fake);
            assert_eq!(code, 1);
            let failed = if compile_error { 1 } else { 2 };
            assert_eq!(lines[failed]["status"], "failed");
            assert_eq!(
                lines[failed]["error_class"],
                if compile_error { "compile" } else { "load" }
            );
            for line in &lines[3..6] {
                assert_eq!(line["status"], "unverified");
            }
            assert!(
                !fake
                    .events
                    .iter()
                    .any(|event| event.starts_with("run:") || event.starts_with("start:"))
            );
            if compile_error {
                assert!(!fake.events.iter().any(|event| event == "load"));
                assert_eq!(lines[2]["status"], "unverified");
            }
        }
    }

    #[test]
    fn ticker_failure_is_not_execution_evidence() {
        let mut fake = Fake {
            ticker_error: true,
            ..Fake::default()
        };
        let (code, lines) = execute(&mut fake);
        assert_eq!(code, 1);
        assert_eq!(lines[3]["error_class"], "ticker");
        assert_eq!(lines[4]["error_class"], "ticker");
        assert!(
            !fake
                .events
                .iter()
                .any(|event| event == "run:Standard" || event == "run:EpochOnly")
        );
        assert!(fake.events.iter().any(|event| event == "run:FuelOnly"));
    }
}
