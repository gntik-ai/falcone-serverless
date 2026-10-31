//! Standalone WAS-01 qualification; never invoked by the worker service.
#[path = "../qualification/outcome.rs"]
mod outcome;

#[path = "../qualification/settings.rs"]
mod settings;

#[path = "../qualification/runner.rs"]
mod runner;

#[path = "../qualification/engine.rs"]
mod engine;

fn main() -> std::process::ExitCode {
    let mut harness = engine::RealHarness::new();
    match runner::run_checks(&mut harness, std::io::stdout().lock()) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("qualification output failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
