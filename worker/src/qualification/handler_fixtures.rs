//! Handler-export fixtures for the guest execute path: checked-in binaries, so the
//! qualification image (which copies only `worker/src`) needs no WAT parser. Each has its
//! WAT source beside it in `handler_fixtures/`, and a test keeps the pair in sync.
//! Nothing here is an artifact encoder.

pub const ECHO: &[u8] = include_bytes!("handler_fixtures/echo.wasm");
pub const MODES: &[u8] = include_bytes!("handler_fixtures/modes.wasm");
pub const START_LOGS_WITHOUT_HANDLE: &[u8] =
    include_bytes!("handler_fixtures/start-logs-without-handle.wasm");
pub const START_LOGS_WITH_HANDLE: &[u8] =
    include_bytes!("handler_fixtures/start-logs-with-handle.wasm");
pub const WRONG_RESULT: &[u8] = include_bytes!("handler_fixtures/wrong-result.wasm");
