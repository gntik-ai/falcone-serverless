//! Deterministic ordering for ordinary qualification cases. Isolated budget
//! probes explicitly run concurrent guests; allocation probes also re-exec.
pub fn serial_case() -> std::sync::MutexGuard<'static, ()> {
    static CASE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    CASE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
