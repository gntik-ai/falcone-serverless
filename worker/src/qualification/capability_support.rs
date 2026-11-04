//! Serialize ordinary qualification cases; stalled/allocation probes re-exec.
//! This keeps parallel test cases from competing for the production thread cap.
pub fn serial_case() -> std::sync::MutexGuard<'static, ()> {
    static CASE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    CASE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
