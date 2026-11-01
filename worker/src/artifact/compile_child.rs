//! Trusted compile-only executable. The parent sets OS limits before sending stdin.
use std::io::{self, Read};
use std::process::ExitCode;

// The fixed WAS-01 source guard requires all engine use here through a crate alias.
use wasmtime as engine;

fn main() -> ExitCode {
    let mut input = Vec::new();
    // EOF is the protocol barrier: no engine work before the parent applies limits.
    if io::stdin().read_to_end(&mut input).is_err() {
        return ExitCode::from(65);
    }
    let mut config = engine::Config::new();
    // Keep in sync with cache_key::ENGINE_CONFIG; all other options are pinned defaults.
    config.consume_fuel(true).epoch_interruption(true);
    let result = engine::Engine::new(&config)
        .and_then(|engine| engine.precompile_component(&input).map(|_| ()));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = format!("{error:#}").to_ascii_lowercase();
            if [
                "out of memory",
                "cannot allocate memory",
                "memory allocation",
                "failed to allocate",
                "mmap",
            ]
            .iter()
            .any(|needle| message.contains(needle))
            {
                ExitCode::from(66)
            } else {
                ExitCode::from(65)
            }
        }
    }
}
