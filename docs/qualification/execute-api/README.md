# Execution API qualification (RUN-01.t3)

Evidence record for the worker execution API of `docs/contracts/worker-execution-api/execution-api-v1.md`.
Every security-relevant size, timeout, deadline, cancellation, budget and isolation criterion has a
case that runs in two places: in process under `cargo test` (the fast loop, real sockets,
`worker/tests/execute_api_local.rs`), and in a restricted pod on the shared cluster
(`worker/tests/cluster_execute_api.rs`). Both run the same case functions
(`worker/src/qualification/execute_api_cases.rs`).

## Status

| Where | Status |
| --- | --- |
| In process, `cargo test --workspace --locked` | Run with the change (see the pull request). |
| Restricted pod, qualification image | UNVERIFIED: the maker sandbox has no cluster. Run by the operator with the commands below; record the pod log here. |
| Restricted pod, production worker image | UNVERIFIED: same. |

## Running it

Both runs are gated by `FS_TEST_CLUSTER=1` and skip with an explicit message without it. Each runs
in a namespace named `fs-test-run01-<run id>` that the test creates and deletes
(`FS_TEST_NAMESPACE_MODE=create`, the default) or in the shared namespace of
`FS_TEST_NAMESPACE_MODE=shared`, in a pod with Pod Security restricted, seccomp `RuntimeDefault`,
a read-only root filesystem, no service account token, dropped capabilities and an arbitrary
non-root UID. Credentials come from the existing External Secrets and are never printed.

```sh
# In-process cases, in the qualification image (built with --features qualification).
FS_TEST_CLUSTER=1 FS_TEST_WAS02_IMAGE=<qualification image@sha256:...> \
  cargo test --locked -p falcone-serverless-worker --test cluster_execute_api \
  cluster_execute_api -- --ignored --exact

# The production worker image, unmodified, as a sidecar; the runner drives it over loopback.
FS_TEST_CLUSTER=1 FS_TEST_WAS02_IMAGE=<qualification image@sha256:...> \
  FS_TEST_WORKER_IMAGE=<production worker image@sha256:...> \
  cargo test --locked -p falcone-serverless-worker --test cluster_execute_api \
  cluster_execute_api_production_image -- --ignored --exact
```

A single case of the first run: `execute-api-qualification --execute-api=<case>`.

## Criteria and their cases

The case names are those of `CASES` in `execute_api_cases.rs`; the production-image cases are
`production-*` in `execute_api_production.rs`.

| Criterion | Case |
| --- | --- |
| Legacy envelope, runtime identity equal to the projection and the pinned Wasmtime | `a_wasm_revision_returns_the_legacy_envelope_with_its_runtime_identity`; `production-envelope` |
| Non-wasm runtime mode or image reference refused, no instance created | `another_runtime_mode_is_refused_and_no_instance_is_created`; `production-runtime-mode` |
| `409` before `503` with every slot busy when the complete body arrived with the headers; otherwise `503` at once without reading the body (an 8 MiB body is not consumed before the answer) | `with_every_slot_busy_*` |
| Undeclared or unprovided import: `422` before guest code | `an_import_the_projection_does_not_declare_*`, `an_ungated_falcone_interface_*`, `a_declared_import_that_the_worker_does_not_provide_*`, `a_component_whose_imports_are_all_declared_and_allowed_runs`; `production-imports` |
| Cancel under full load and an execute listener full of connections stalled in headers and bodies (observed server-side; in the production image the bodies are proven by `100 Continue`); control isolation and recovery; listener routing | `a_cancel_succeeds_with_all_slots_busy_*`, `a_full_control_listener_*`, `each_listener_refuses_*`; `production-saturation` (the cancelled guests are proven started by their own log line, and a pass that cancelled during setup is repeated; the guest-start signal is server-side in the qualification image), `production-routing` |
| Oversized headers, request and action body; idle, drip, stalled body and slow reader closed within one absolute bound; slot recovery | `oversized_*`, `an_oversized_*`, `an_idle_connection_*`, `a_header_drip_*`, `a_stalled_body_*`, `a_slow_response_reader_*`; `production-http-ceilings`, `production-idle` |
| Shortest of request, projection and worker limit; setup and bind under `until`; guest never starts after `until` | `a_request_deadline_*`, `a_projection_limit_*`, `a_worker_wall_clock_limit_*`, `setup_that_crosses_until_*`, `a_cancel_during_a_blocked_bind_*`, `a_cancel_that_lands_as_*`, `a_deadline_that_passes_as_*` (a stage that fails after cancellation or expiry is still `cancelled` or `timeout`), `host_the_handler_is_not_invoked_when_until_passes_after_instantiation`; `production-timeout` |
| Host call near expiry ends at `until`; a late entry is observed, never reaches the host, and the deadline beats the call budget | `a_host_call_blocking_near_expiry_*`, `a_blocked_host_call_that_ends_before_until_*` |
| Memory, fuel, host-call budgets and the worker ceiling | `a_one_mebibyte_memory_budget_*`, `a_memory_budget_above_the_worker_ceiling_*`, `a_small_fuel_budget_*`, `the_call_after_max_host_calls_*`, `storage_open_drop_and_denied_calls_count_*` (storage open, drop and an authorization-denied call each count), `host_a_host_counts_every_entry_*`, `host_storage_open_drop_and_denied_entries_*`; `production-budgets` |
| Cancel or timeout during cold validation of a near-64 MiB artifact, slot free within 500 ms, validated once | `a_cancel_during_cold_validation_*`, `a_short_timeout_during_cold_validation_*`, `a_cold_artifact_is_validated_once_per_execution` |
| Credential redaction; bounded, linear, fail-closed; output bound | `the_credential_is_redacted_*`, `a_maximum_size_log_result_*`, `redaction_*` |

The release-build timing bound of the redaction case (one second for the maximum secrets and 1000
maximum-length lines) is measured by the runner in the release qualification image; a debug
`cargo test` only checks it against a loose bound.

## What the cases cannot show

A guest blocked inside a host call cannot be interrupted by cancellation before RUN-02: the cancel
answer is `uncertain` and the execution ends at `until`. The credential's absence from guest
environment, files and memory is by construction (no WASI imports exist; the guest's only inputs
are `params` and the invocation identity) and is checked here through the redaction case; a probe
that scans guest memory is left to the restricted qualification once OCI-04 and the credential
format (ADR 0006, Proposed) settle.
