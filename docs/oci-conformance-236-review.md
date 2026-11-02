# OCI conformance #236 review handoff

The tester must reject extra candidate listeners, clean up escaped descendants,
and exercise the reference's UTC deadline syntax. This is test-only tooling;
it does not change the worker runtime or deploy anything.

## Base evidence for the PR description

At immutable base `28adcb6c681121ad4ae12d0edad31b3832336afa`:

- `worker/src/oci_contract.rs:770-774` skips every TCP listener whose port
  differs from `$PORT`. The dual-listener regression requires the diagnostic
  to name the second wildcard listener, so this base behavior fails it.
- `worker/src/oci_contract.rs:830-834` signals only the direct child's numeric
  PID and ignores ESRCH. At `:851-852`, Drop signals the original process group
  and then waits without a bound. A descendant that changes group and ignores
  TERM can survive; the lifecycle regression requires it to be killed and
  reaped within TERM_GRACE plus a small bound.
- `worker/src/bin/oci-contract-reference.rs:37` accepts only one literal UTC
  deadline. `worker/src/oci_contract.rs:196` always sends that literal. Fractional
  deadlines over HTTP and a reference accepting only that literal expose this
  gap: the former must succeed and the latter must fail OCI-V1-REQUEST.

## Follow-up to the independent checker

At `b750aec7ae1506636ff75eaaf8eaedb60a56d41b`, deadline checks precede
every resume and kill. A procfs read crossing the absolute deadline therefore
prevents cleanup signals entirely. The synthetic regression uses the named
ReadCrossingDeadline scenario at the public ProcessTree seam and requires a
deadline error, a pidfd KILL attempt, and no repeated cleanup from Drop.

Red-before-green evidence: the ProcessTree tests were first run through a
temporary rustc harness extracting the repository's unchanged `process_seam`
test module and linking the production `oci_contract.rs`. Before each fix,
`/tmp/oci236-process-seam` with the corresponding test-name filter failed:

- `deadline_crossing_still_kills_retained_processes_and_drop_does_not_retry`:
  the deadline left the stopped candidate alive.
- `stop_wait_leaves_time_to_kill_and_reap_within_the_absolute_deadline`:
  termination returned TimedOut instead of killing and reaping the candidate.
- `reused_child_pid_outside_the_stopped_parent_is_never_signalled`:
  cleanup killed and reaped the unrelated process.

The persistent tests run normally in `cargo test --workspace --locked`;
the temporary harness is not needed by CI.

HTTP and binding inspection must leave TERM_GRACE plus CLEANUP_BOUND inside
the original deadline. If a read nevertheless overruns the deadline, cleanup
makes a final nonblocking KILL attempt through each retained handle, returns
the error, and never starts a new discovery, wait, or Drop budget. A task in
uninterruptible kernel sleep may not exit immediately even after SIGKILL;
such incomplete cleanup is never reported as success.

Child discovery also re-reads the stopped parent's child list after pidfd_open,
before retaining or signalling the new handle. This excludes a PID that was
auto-reaped and reused outside the tree before the first identity sample.
The unused snapshot-only binding helper has been removed; wildcard, IPv6 and
other-loopback coverage now exercises the production ProcessTree seam.

The existing real escaped-descendant regression uses setsid, which also
changes the process group, and retains its SIGTERM-ignoring child. Its
TERM_GRACE plus two-second assertion allows scheduling slack around the
documented one-second cleanup bound. This coverage and the filesystem
regressions remain in place.

## Rule 3 residual risk

Only the candidate process tree is untrusted. Outside processes are trusted
platform components. An outside process signalling or tracing the candidate,
or passing it file descriptors, is outside this check's threat model; it could
open a forbidden listener itself. Final task-state reads cover transitions
originating within the stopped tree. No outside-cooperation regression is
claimed. Unknown or inconsistent listener ownership still fails closed, so
host noise can produce an inspection-unstable result; clean synthetic procfs
coverage proves the compliant verdict remains reachable.

R3 still requires independent review and operator approval before merge.
No publication or approval is performed by the implementer.

## Maker validation

Formatting, workspace Clippy with `-D warnings`, and controller gofmt, vet
and tests pass. Cargo.lock remains byte-identical to the immutable base.
All 12 ProcessTree seam tests pass. The full OCI contract suite has 27 passing
tests, including UTC syntax and scratch cleanup crossing the deadline.

The following checks are SKIPPED for sandbox restrictions, not code failures;
PR CI must run them without weakening or skipping any test:

- `cargo test --workspace --locked` stops in unchanged compiler isolation
  tests. `launcher::abstract_transport_closes_second_connection_without_disturbing_owner`
  reports `Control` at `compiler/tests/isolation/launcher.rs:163`; the transport
  maps UnixListener bind errors to Control. The other blocked test,
  `seccomp::installed_filter_denies_pathname_sockets_and_preserves_unnamed_pairs`,
  reports `Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }`
  at `compiler/tests/isolation/seccomp.rs:204` when binding a Unix socket.
- `cargo test --locked -p falcone-serverless-worker --test oci_function_contract`
  cannot run its eight socket-dependent tests: `compliant_http_yields_no_violations`,
  `compliant_process_and_scratch_writes_pass`,
  `dual_listener_process_diagnostic_names_the_wildcard`, `each_http_rule_is_isolated`,
  `legacy_replayers_and_boundary_envelopes_pass`,
  `process_rules_and_filesystem_locations_are_isolated`,
  `reference_deadlines_are_checked_over_http`, and
  `tester_rejects_a_reference_that_only_accepts_one_deadline_literal`.
  Each reports the same EPERM error above at `worker/tests/oci_function_contract.rs:62`
  while allocating its local TCP port.
- `cargo test --locked -p falcone-serverless-worker --test oci_process_lifecycle`
  cannot run `cleanup_reaps_descendants_of_the_candidate`,
  `escaped_sigterm_ignoring_child_is_killed_and_reaped`,
  `process_lifecycle_signals_kills_and_reaps_without_network`, or
  `sigterm_ignore_is_bounded_and_killed_after_ten_seconds`.
  All four fail before spawning a candidate with
  `procfs PID namespace mismatch: /proc must describe the candidate PID namespace`.
