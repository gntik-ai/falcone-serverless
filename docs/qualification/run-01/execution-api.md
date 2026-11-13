# RUN-01 execution API qualification

Qualifies the worker execution API ([execution-api-v1.md](../../contracts/worker-execution-api/execution-api-v1.md),
"Worker service") on the restricted fs-test harness. Issue: gntik-ai/falcone-serverless#130
(RUN-01.t3).

## What runs

`worker/src/qualification/execution_api.rs` holds one function per case. Each case starts the real
`falcone-serverless-worker serve` on ephemeral ports in a scratch directory, publishes embedded
fixture components (`worker/src/qualification/api_fixtures/*.wat` and `execute_fixtures/*.wat`, with
their `.wasm`) to the worker's trusted artifact store, and talks to the worker over TCP like the
dispatcher would. Cases synchronize on the worker's structured log events (`listening`,
`guest_started`, `execution_finished`, `connection_admitted`, `connection_rejected`,
`connection_finished`, `connection_closed`, `response_deadline_exceeded`), never on
a wait shorter than a documented deadline; the only timeouts in the cases are fail-safes that turn a
hang into a failure.

| Case | Acceptance criteria |
| --- | --- |
| `identity` | legacy envelope; identity equals the projection and the pinned Wasmtime `49.0.5`; an engine mismatch is refused |
| `runtime-mode` | `runtime_mode` other than `wasm`, or an image reference, gets 409 `runtime_mode_not_supported` first and creates no instance (no artifact is even published) |
| `credential` | credential absent from guest memory (dump of every host-written byte), from environment and files (a WASI import is refused), and from the response and structured logs |
| `imports` | undeclared import with `imports: []`, undeclared with a non-empty list, and declared-but-not-allowed each 422 without running guest code; fully declared components run |
| `budgets` | 1 MiB memory budget fails the 4 MiB fixture (`memory_budget_exceeded`), a sufficient one passes; small fuel stops a loop (`fuel_exhausted`) well before `timeout_ms`; `max_host_calls: N` refuses call N+1 and the host sees exactly N; `budget_above_worker_ceiling` before compile or instantiate; all `retryable: false`, no partial result, redacted logs |
| `timeouts` | the request deadline, the projection limit, and the worker wall clock each win when shortest: 504 with attribution and identity, elapsed time within tolerance, capacity recovered |
| `duplicate-attempt` | the same attempt cannot run twice at once (400 `duplicate_attempt`); a cancel names exactly one attempt |
| `saturation` | with `concurrency` guests running, a non-wasm request still gets 409 and only a request that would run gets 503 busy; then, with the execute listener full of stalled connections (headers and bodies, proven full by the worker's own `connection_admitted` and `connection_finished` events, and by a `connection_rejected` for the extra execute), a cancel on the control listener is acknowledged within `cancel_reserve_ms`; each listener refuses the other's operations |
| `control-stall` | a control listener full of stalled connections does not affect running executions and recovers after its deadlines |
| `http-limits` | oversized headers, request and action body are refused without guest code; no-bytes, dripped-header and stalled-body connections close within their bounds; after each case a normal execute and a cancel succeed |
| `body-deadline` | a body trickled a byte a second, never quiet for the 5 s inactivity bound, is closed at the absolute 30 s body deadline (`body_deadline`); a normal execute and a cancel succeed afterwards |
| `slow-reader` | a one-byte-at-a-time reader is cut off at the absolute response deadline; its slot is released and a normal execute and a cancel succeed |

The redaction adversarial set (maximum distinct secrets and 1000 maximum-length lines in under 1 s
in release; overlapping and prefix-sharing secrets; over budget returns only `<redacted>`) is in
`worker/tests/execute_api_local.rs`, and the `execution_limits` unit tests (every field, not only
the timeout) are in `worker/src/api/tests.rs`.

## Running

Locally (loopback, ordinary `cargo test`; the sandbox may skip socket tests only when it cannot
bind loopback):

```sh
cargo test --workspace --locked
cargo test --release --locked -p falcone-serverless-worker --test execute_api_local
```

In the cluster (operator, opt-in; skips with an explicit message when `FS_TEST_CLUSTER` is unset):

```sh
FS_TEST_CLUSTER=1 KUBECONFIG=... FS_TEST_WAS02_IMAGE=<repo>@sha256:<digest> \
  cargo test --locked -p falcone-serverless-worker --test cluster_execute_api -- --ignored
```

The image is built from `worker/qualification/Containerfile` (it carries the worker, the trusted
compiler and the runner). The production image (`worker/Containerfile`) carries the worker and the
trusted compiler beside it; `worker/tests/production_image.rs` lays out the production runtime stage
from that file and shows that a valid cold request executes there. Running the production image
itself in the cluster, and its build and scan, are the pull request's image checks and the release
gate. Each case runs in its own pod in a namespace `fs-test-run01-<run-id>` that
the test creates and deletes (or, in shared mode, in the namespace named by `FS_TEST_NAMESPACE`,
touching only objects labelled `falcone.test/run=<run-id>`). The pod runs under Pod Security
restricted, seccomp `RuntimeDefault`, a read-only root filesystem and an arbitrary non-root UID.
Credentials never appear: the cases use recognizable canary strings, and a real credential comes
from OpenBao through the existing External Secrets.

## Evidence

Every case prints one JSON line `{"check":"execution_api","case":"<name>","status":"passed",
"elapsed_ms":<n>}` and, when all selected cases passed, `{"check":"execution_api","status":"passed"}`.
The operator retains the pod log of each run as evidence, referenced by artifact ID and SHA-256 on
the RUN-01 issue; nothing here claims a cluster run happened until that evidence exists.
