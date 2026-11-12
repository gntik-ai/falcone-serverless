# RUN-01 restricted qualification: the worker execution API

Task RUN-01.t3. The cases below run in a Pod Security restricted pod (seccomp `RuntimeDefault`, read-only
root filesystem, arbitrary non-root UID) in an `fs-test-run01-<run-id>` namespace that the test creates
and deletes, with `FS_TEST_CLUSTER=1`, `FS_TEST_WAS02_IMAGE=<repo>@sha256:<digest>` and a `KUBECONFIG`:

```sh
FS_TEST_CLUSTER=1 cargo test --locked -p falcone-serverless-worker --test cluster_execute_api -- --include-ignored
```

Without the gate the cases skip with `UNVERIFIED: FS_TEST_CLUSTER unset; no client call`. In
`FS_TEST_NAMESPACE_MODE=shared` the test creates only run-labelled objects (`falcone.test/run=<id>`) and
deletes only those. Each case is one pod running `compile-cache-qualification --execute-api=<case>`: the
service starts on loopback in the pod, with its production limits, and the case drives it over a real
socket. Every case ends with the recovery check: a normal execute and a cancel both succeed and every
connection, execute and control slot is released.

## Evidence

A passing case prints `{"case", "group", "status": "passed", "elapsed_ms"}` and then
`{"check": "execute_api", "status": "passed"}`; the harness requires the second record. The cases of
the `saturation` and `http` groups are recorded together.

The cluster test keeps these records before it deletes the pod and its namespace: the sanitized
`{"case", "group", "status", "elapsed_ms"}` (and a short `reason` token on a failure) of every case are
printed and appended to `execute_api-evidence.jsonl` in `FS_TEST_EVIDENCE_DIR` (default: the cargo target's
`tmp` directory). Nothing else of the pod's log is kept.

| Case | Group | Criterion |
| --- | --- | --- |
| `identity` | api | AC1: result envelope, activation and attempt fields, every identity field matches the projection, `parse_result` accepts it against the request |
| `non-wasm` | api | AC2: `runtime_mode_not_supported` (409) for `oci` and unknown modes, before any other check, no instance, no compile, no artifact fetch, no request value in the reply |
| `legacy` | api | AC3: `missing_path`, `non_json_body`, `body_too_large` in contract order, no guest |
| `credential` | api | AC4: no ambient environment or preopen (declared `wasi:` imports are refused), no credential in the first 4 KiB of guest memory, credential redacted from logs, absent from the worker's environment |
| `imports` | api | AC5: `undeclared_import` (422) for `imports: []`, for a list missing an import and for a declared import that is not allow-listed, with no compile or instance; a fully declared component runs |
| `saturation` | saturation | AC6, AC9: `concurrency` guests that have reported progress through the update operation (so are running, not merely admitted) and then block, an extra execute gets `503 busy` at once, a cancel is acknowledged within the deadline while every execute slot is held, then recovery |
| `saturation-stalled` | saturation | AC2, AC6, AC9: with every execute slot held by a blocked guest and the overflow pool full of execute requests that sent valid headers and stalled their bodies, a complete non-wasm request still gets its non-retryable 409 and a wasm one `503 busy`; more stalled bodies replace the oldest ones; they hold no connection (only the running guests are counted); a cancel is acknowledged within the deadline while the stalled bodies are still open, then recovery |
| `saturation-headers` | saturation | AC6, AC9: with every execute slot held by a blocked guest, more peers than the connection limit send an incomplete request line and headers (announcing execute, then cancel); they hold no execute or control slot, the head pool replaces its oldest member, a cancel is acknowledged within the deadline, then recovery |
| `http-headers` | http | AC8: headers over `MAX_HEADERS` (16 KiB) get 431 or a close, no guest |
| `http-request` | http | AC8: a request over `MAX_REQUEST` (8 MiB) gets 413 before its body is read |
| `http-action-body` | http | AC8: an action body over 1 MiB gets the contract's `413 body_too_large`, no guest, no artifact fetch |
| `http-silent` | http | AC8: a connection that sends nothing is closed within `HEADER_DEADLINE` (5 s) |
| `http-drip` | http | AC8: header bytes dripped faster than `IO_TIMEOUT` are closed at the absolute `HEADER_DEADLINE` |
| `http-stall` | http | AC8: a body that stalls is closed within `IO_TIMEOUT` (5 s), no guest |
| `http-control-body` | http | AC8: a cancel or update body over the 16 KiB control ceiling gets 413 before the body is read, no guest |
| `http-body-drip` | http | AC8: body bytes dripped faster than `IO_TIMEOUT` are closed at the absolute `BODY_DEADLINE` (30 s), no guest |

The local feedback loop for the same cases is `worker/tests/execute_api_local.rs`, which also runs the
pod cases on loopback outside a pod.
