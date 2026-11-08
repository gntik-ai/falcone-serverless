pub use falcone_serverless_worker::host;
#[path = "../src/qualification/capability_support.rs"]
mod capability_support;

#[path = "../src/qualification/counting_alloc.rs"]
mod counting_alloc;

#[path = "../src/qualification/capability_invocations.rs"]
mod qualification;

/// The host never copies guest result or error data that the transfer ceiling refuses:
/// a result or error text over the ceiling is measured in guest memory and dropped
/// unread, so the host heap stays far below the size of the refused data.
mod guest_execute_allocation {
    use super::counting_alloc;
    use falcone_serverless_worker::{host, limits};
    use host::execute::execute;
    use host::{Bindings, Context, Host, HostError, Operation, Reply};
    use limits::Limits;
    use std::sync::Arc;
    use wasmtime::component::Component;

    struct Silent;
    impl Bindings for Silent {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            Ok(Reply::Empty)
        }
    }

    const MODES: &[u8] = include_bytes!("../src/qualification/handler_fixtures/modes.wasm");
    const CEILING: usize = 1_048_576;
    /// Both refused payloads are 1.5 MiB; a copy of either would exceed this on its own.
    const HEAP_BOUND: usize = 1_048_576;

    fn peak_during(mode: u8, bytes: usize) -> (usize, &'static str) {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true).epoch_interruption(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let component = Component::new(&engine, MODES).unwrap();
        let host = Host::with_limits(
            Context::default(),
            Arc::new(Silent),
            Limits::compatibility(),
        );
        let params = [&[mode][..], &(bytes as u32).to_le_bytes()].concat();
        let before = counting_alloc::reset_peak();
        let result = execute(&component, host, &params);
        let peak = counting_alloc::peak();
        let retained = counting_alloc::live().saturating_sub(before);
        assert!(retained < HEAP_BOUND, "host heap retains {retained} bytes");
        (peak.saturating_sub(before), result.unwrap_err().code)
    }

    /// Runs only in a fresh child process (ADR 0014 invariant 4): the parent test
    /// below re-executes this binary with `FALCONE_REFUSED_HEAP_PROBE` set.
    #[test]
    fn refused_heap_probe() {
        let Ok(probe) = std::env::var("FALCONE_REFUSED_HEAP_PROBE") else {
            return;
        };
        let mode = probe.as_bytes()[0];
        let (delta, code) = peak_during(mode, CEILING + CEILING / 2);
        assert_eq!(code, "buffer_limit_exceeded", "mode {}", mode as char);
        assert!(
            delta < HEAP_BOUND,
            "mode {}: host heap grew by {delta} bytes",
            mode as char
        );
        println!("refused-heap-probe passed: {} {delta}", mode as char);
    }

    #[test]
    fn refused_results_and_error_texts_are_not_copied_into_host_memory() {
        let _case = super::capability_support::serial_case();
        for mode in ['s', 'G'] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "guest_execute_allocation::refused_heap_probe",
                    "--exact",
                    "--nocapture",
                ])
                .env("FALCONE_REFUSED_HEAP_PROBE", mode.to_string())
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .contains(&format!("refused-heap-probe passed: {mode} ")),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
