#[path = "support/go_wasi.rs"]
mod cases;
#[test]
fn csprng_is_linked_narrowly_and_bounded() {
    cases::csprng_is_linked_narrowly_and_bounded();
}
#[test]
fn stdout_is_bounded_and_invocation_local() {
    cases::stdout_is_bounded_and_invocation_local();
}

#[test]
fn one_memory_and_only_declared_functions_are_linked() {
    cases::one_memory_and_only_declared_functions_are_linked();
}
