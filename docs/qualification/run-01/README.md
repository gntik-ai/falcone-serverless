# RUN-01.t3 qualification: the worker execution API

The execution API of `docs/contracts/worker-execution-api/execution-api-v1.md` is qualified in two
places. Every case runs the real execute and control listeners, the real cache and the trusted
compile child, with real guests; none uses a mock of the service.

- **Local** (`cargo test --workspace --locked`): `worker/tests/execute_api_local.rs` runs each case
  on loopback with the HTTP deadlines scaled down. The redaction matcher, the limits and the
  contract tables also have unit tests (`worker/src/server/redact.rs`, `worker/src/api/tests.rs`,
  `worker/tests/execution_api_contract.rs`).
- **Restricted pod** (the shared cluster, gated by `FS_TEST_CLUSTER`):
  `worker/tests/cluster_execute_api.rs` runs each case in its own pod of the qualification image in
  an `fs-test-*` namespace that it creates and deletes (Pod Security `restricted`, seccomp
  `RuntimeDefault`, read-only root filesystem, arbitrary non-root UID, no service account token),
  with the **documented production deadlines** (5 s execute, 2 s control) instead of the scaled
  ones. The qualification image is built from `worker/qualification/Containerfile` with
  `--features qualification`, which turns on the test-only hooks; the production image
  (`worker/Containerfile`) is built without it and contains none.

Both run the same code: `worker/src/qualification/execution_api.rs`. The test-only hooks (setup,
host-call, entry and cache-stage probes, and the validation counter) exist only under
`cfg(test)` or the `qualification` feature.

## Cases

Each case is selected in the pod with `compile-cache-qualification --execute-api=<name>` and
passes when the pod succeeds and prints `{"check":"execute_api","status":"passed"}`.

| Pod case | Test (`cluster_execute_api`) | Criterion |
| --- | --- | --- |
| `identity` | `identity` | Legacy envelope; identity equals the projection and the pinned Wasmtime version; validation runs once per execution. |
| `non-wasm` | `non_wasm` | A non-wasm runtime mode or an image revision gets `409 runtime_mode_not_supported`; nothing is loaded, bound or run. |
| `engine-version` | `engine_version` | A projection for another engine is refused. |
| `credential` | `credential` | The credential is absent from guest-visible data and from logs; WASI (environment, files) is not importable. |
| `redaction` | `redaction` | Redaction before the cut: a secret over the cut, crossing it, ending at it, several of which only some fit, overlapping secrets, a piece of a secret, and the over-budget marker. |
| `imports` | `imports` | `undeclared_import` (422) for an undeclared allowed interface with `imports: []`, with a non-empty list, and for a declared interface that is not allowed; a positive all-declared case. |
| `listeners` | `listeners` | Each listener answers `404` for the other's operations and runs nothing. |
| `saturation` | `saturation` | With `concurrency` blocked guests and the execute listener full of stalled connections, an extra execute gets `503 busy` and a cancel on the control listener takes effect within 500 ms. |
| `saturation-control` | `saturation_control` | A control listener full of stalled connections does not affect running executions, and recovers when its deadlines close them. |
| `admission-order` | `admission_order` | With every slot busy: a complete non-wasm request still gets `409`; a wasm request, or one whose body has not arrived, gets `503 busy` without its body being read. |
| `update-cancel` | `update_cancel` | Unknown pairs `404`, malformed `400`, a stale update `409`. |
| `http` | `http` | Oversized head, oversized request and oversized action body are refused; a silent connection, a dripped head and a stalled body are closed within their bounds; a normal execute and a cancel succeed after each. |
| `slow-reader` | `slow_reader` | A reader taking a maximum-size response one byte at a time is cut off at the absolute response deadline; with `concurrency` such readers a normal execute and a cancel still succeed. |
| `timeout-selection` | `timeout_selection` | The request deadline, the projection limit and the worker wall clock each win when shortest: `504` with attribution and identity, elapsed within tolerance, capacity recovered. |
| `budgets` | `budgets` | 1 MiB vs 4 MiB memory, a fuel loop, N+1 host calls with exactly N seen, and an above-ceiling refusal before compile. |
| `deadline` | `deadline` | Setup crossing `until`, or instantiation ending after it, never starts the guest; a blocked host call ends at `until`; a late host call is refused unseen; the deadline outranks the host-call budget. |
| `cache-stalls` | `cache_stalls` | Stalls during cache read, compilation and publication: `timeout`, slot released within the budget, next execute admitted, no partial entry. |
| `validation` | `validation` | A cold artifact near 64 MiB cancelled or timed out during validation: `cancelled` or `timeout`, slot released within 500 ms, nothing published. |
| (production image) | `production_image_ships_the_compile_child` | `falcone-serverless-worker --check-install` in the **production** image finds and starts `artifact-compile-child` beside the worker. Needs `FS_TEST_WORKER_IMAGE`. |

## Running

```sh
FS_TEST_CLUSTER=1 KUBECONFIG=... FS_TEST_WAS02_IMAGE=<qualification image>@sha256:... \
  FS_TEST_WORKER_IMAGE=<worker image>@sha256:... \
  cargo test -p falcone-serverless-worker --test cluster_execute_api -- --include-ignored
```

Without `FS_TEST_CLUSTER` every cluster test prints `UNVERIFIED: FS_TEST_CLUSTER unset` and makes
no client call. `FS_TEST_NAMESPACE_MODE=shared` uses `FS_TEST_NAMESPACE` and touches only
objects labelled `falcone.test/run=<id>`.

## Status

The implementation sandbox has no cluster, so the pod cases have **not** been run: their status
is `UNVERIFIED` until an operator runs them. The local run covers the same cases on loopback.
