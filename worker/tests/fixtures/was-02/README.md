# Compile/cache fixtures

`valid.wasm` is the eight-byte empty component. `invalid.wasm` has component
magic followed by a malformed section. `native.elf` is rejected at intake.
The precompiled-input case generates real native output with the locked engine
in the test, avoiding a checked-in architecture-specific engine artifact.

The qualification fixture encoder expands `cpu-heavy.wat` to 64 functions with
8192 multiply/add pairs each and `memory-heavy.wat` to one function with 131072
pairs. An independent WAT parser checks their exact bytes. Neither workload is
instantiated or executed. CPU failure uses 1 s / 1 GiB / 300 s; memory failure
uses 60 s / 128 MiB / 300 s. Both controls use 120 s / 4 GiB / 300 s.

These are qualification budgets, not production defaults. The operator builds
and pins the image containing the qualification runner for the later cluster
evidence run on #52.

## Capability gates (#109)

`worker/src/qualification/fixtures/capability-*.wat` are raw component sources
embedded in the existing `compile-cache-qualification` binary. Its
`--capability-gates` mode runs the same assertions as `capability_invocations.rs`
and `capability_http.rs`, then the compile/cache CPU, memory and recovery cases
above. No image or Containerfile change is part of this task.

The host links exact versioned world interfaces, with storage, HTTP, events and
workflows selected from verified grants. External bindings receive the host's
verified tenant/workspace context. The HTTP transport is trusted host code: it
must use `PinnedRequest::connect`, retain the authority only for Host/TLS SNI,
and neither resolve again nor follow redirects. Each redirect returns to the
host gate. No real platform bindings are supplied by this qualification.

Argument preflight uses `WasmStr` and derived lazy records in `WasmList`, checks
the complete 1 MiB aggregate before UTF-8 host copies, and stops at first excess.
Wasmtime's finite hostcall accounting is sized for lazy cursors relative to the
256 MiB store memory limit; the host copy ceiling remains 1 MiB. UTF-16 and
Latin-1 decoding inside `to_str` allocates in proportion to capped guest memory;
UTF-8 validation still reads the guest range and consumes CPU.

Allocation probes re-exec in isolated children, warm up, reset the peak of the
measurement-only counting allocator (`worker/src/qualification/counting_alloc.rs`,
ADR 0014) and read the peak host heap allocation across the call. Guest linear
memory is not host allocation and is not counted; oversized input remains on
untouched zero pages. The fixed host allocation bound is 256 KiB; eager
String/Vec controls over the same fixtures must exceed 32 MiB, and a sensitivity
test proves a 512 KiB host allocation reads above the bound. Probe markers prevent an accidentally empty subprocess test from passing.
The stalled-binding probe uses fresh invocations in a separate process and
verifies that four detached calls to one stalled binding fail fast across fresh
invocations while 20 concurrent guests still use a different healthy binding
(2,000 successful calls). The process budget bounds every live helper to 24
(ADR 0007's qualification concurrency of 20 plus four reserved slots), including
concurrently stalled calls and binding destructors. A completely detached
process budget fails fast even for a fresh binding identity. Active contention
waits within the call deadline; permits stay owned by helpers until their captures
have been dropped. A panic/unwinding probe holds 24 binding destructors open
and verifies that no replacement helper starts until those destructors exit.
Probe stages poll helper entry and exit for up to 10 s instead of assuming that
a call timeout schedules its helper. Stalled bindings stay gated until explicitly
released. Fail-fast checks allow 2 s against a 30 s call deadline; the ordinary
20 ms timeout checks allow a 2 s scheduling tolerance. The concurrent saturation
stage cancels guests only after all 24 helpers have entered the binding.
The per-binding detached count is process-wide and removed
only on helper exit, independent of invocation/store lifetime. Binding identity
uses the trusted shared binding instance, with weak ownership preventing address
reuse while a ticket remains. Runtime integration must preserve shared binding
identity and size worker admission to this qualification capacity.

Storage keys are limited to 1 KiB before copying. Combined with the limit of
1,024 live object/checkpoint/execution handles per invocation, retained storage
keys cannot exceed 1 MiB. Dropping handles frees capacity; handle numbers remain
monotonic to avoid reusing a dropped handle. A separate isolated memory regression
opens repeated near-1 MiB keys from a 2 MiB guest, observes limit-exceeded and
zero binding calls, and checks growth against the same 256 KiB bound. The prior
implementation retained about 64 MiB for the 64-key reproduction.

HTTP redirects never downgrade HTTPS to HTTP. Authorization and cookie headers
supplied by guests stay only on the same origin (host, scheme and port), even
when the next origin is also allow-listed. Other headers retain their values.
Checkpoint read/write/progress, structured log redaction, redirect hop limits and
the pinned connection helper have explicit regression coverage. The public
instantiation helper is qualification-only: future runtime integration must
compile tenant sources through the supervised trusted compiler first.
Its store limiter also caps each of at most four guest tables to 65,536 elements,
independently of the linear-memory cap. Workflow start reserves handle capacity
before calling the binding, so exhaustion cannot start untracked workflows.
Mixed public/private DNS answers are denied in either order, also on redirects.

Operator qualification (not run by agents): with the runner's `KUBECONFIG` and
an operator-built, digest-pinned `FS_TEST_WAS02_IMAGE` already set, run exactly:

```sh
FS_TEST_CLUSTER=1 cargo test --locked -p falcone-serverless-worker --test cluster_capability_gates -- --ignored --nocapture
```

The live test is ignored by default with the visible reason
`not run: FS_TEST_CLUSTER unset`, skips client construction, and keeps workspace
CI green. The operator command explicitly selects it with `--ignored` and sets
`FS_TEST_CLUSTER=1`. Any set value other than `1`, or missing/invalid inputs when
opted in, fails closed. The pod uses RuntimeDefault
seccomp, a read-only root, an arbitrary non-root UID and no service-account
token. Namespace create/cleanup and labelled shared-mode cleanup reuse the
existing namespace guard and `FS_TEST_NAMESPACE_MODE`/`FS_TEST_NAMESPACE` rules.
No live cluster evidence is claimed by the in-process suite. Sandbox socket
checks report `NOT RUN: EPERM (sandbox), PR CI runs them`; the pod mode fails
closed if it cannot create its required listeners.

## Limits and cancellation contract (#125)

The library exports `limits::Limits::parse` for the closed JSON contract with
`version: 1`. All ten fields are required unsigned integers. Missing fields,
wrong types, zero, `u64::MAX`, values above the validation ceiling, unknown
fields and unsupported versions fail with typed errors. No contract field has
a default. The ceilings are validation bounds, not production profile budgets:

| Field | v1 validation ceiling |
| --- | ---: |
| compile_cpu_seconds | 3600 |
| compile_memory_bytes | 68719476736 |
| compile_wall_clock_ms | 3600000 |
| compile_input_bytes | 67108864 |
| guest_memory_bytes | 536870912 |
| guest_to_host_bytes | 1048576 |
| host_to_guest_bytes | 1048576 |
| guest_deadline_ms | 300000 |
| host_call_deadline_ms | 30000 |
| termination_budget_ms | 30000 |

The two buffer ceilings and host-call ceiling remain owned by
`falcone:host@0.1.0`; this contract can lower them, never raise them. The other
ceilings make this qualification contract finite and are not RUN-02 profiles.
`Host::new` retains the earlier 256 MiB / 1 MiB / 30 s behavior.
`Host::with_limits` consumes an already validated contract. Its guest deadline
requires an epoch-enabled engine, fails closed without one, starts immediately
before instantiation, and includes guest start functions. Its timer stops and
joins when the store drops. Shared-engine epoch ticks check each store's own
wall deadline before interrupting it. The blocked-call fixture explicitly opts
out of guest interruption through the qualification-only constructor modifier.
No production invocation path consumes this contract.

`compile-cache-qualification --limits-cancellation` emits exactly one JSON
record per fixture: `compile_cpu`, `compile_memory`, `guest_memory`,
`guest_to_host`, `storage_reply`, `http_reply`, `guest_deadline`,
`host_call_deadline`, and `termination`. Any failure makes the mode exit
nonzero; an unknown mode also exits nonzero. Missing execution or event evidence
cannot pass. Sensitivity tests reject injected success on a cancelled call,
a missing memory event, an over-budget duration, and a skipped fixture.

The embedded `limits-v1.json` contains qualification values only. Compilation
uses the existing CPU and memory fixtures and budgets above, converted to
`CompileLimits` in the binary. RLIMIT_AS measures virtual address space, not
RSS; CPU accounting has whole-second granularity. Each failure asserts the
specific typed diagnostic, an empty cache directory, a reaped compiler, and a
subsequent successful control compilation.

The source-only image embeds `limits-*.wat`. The memory fixture begins at two
64 KiB pages with a lowered three-page host cap, handles a failed two-page grow,
and still produces a host-side `guest-memory-limit` event. A one-page grow and
a fresh valid fixture then succeed; a four-page initial memory is rejected.
The event fixture sends 50 and 51 bytes with 14 bytes of aggregate overhead to
a 64-byte guest-to-host limit. The reply fixtures count canonical allocations
inside guest memory: storage delivers 60 bytes plus four bytes of overhead;
HTTP delivers 54 bytes plus ten bytes of status/list overhead. One byte more
returns `limit-exceeded` and allocates no guest data, after the binding ran.
Storage-key metadata also respects the configured aggregate before copying.

The guest loop uses a 1000 ms epoch deadline. The blocked call uses a 100 ms
host-call deadline with epoch and fuel disabled and the cancellation flag unset.
Both allow a **2000 ms scheduling tolerance**. Binding entry is observed rather
than assumed after a sleep. The guest sees only WIT `timeout`; host evidence is
`host-call-uncertain` while the binding remains blocked and
`host-call-cancelled` only after helper exit/join is observed. Stalling fixtures
serialize and release their bindings, including during assertion unwinding.

The termination fixture re-execs only the trusted `current_exe`. Its embedded
loop ignores the host cancellation flag; a pipe handshake proves the child
received the cancellation request. Starting at that request, the parent waits
the 5000 ms termination budget, calls `Child::kill` (SIGKILL on Linux), waits,
and checks the reaped signal-9 status within the budget plus the same 2000 ms
tolerance. Another valid guest is held inside its own binding across the kill
and then completes. A drop guard kills and reaps the child on error paths.
This is a test-only fallback, does not select ADR 0004's production process
model, and does not imply rollback of external effects or safe replay.

The new cluster suite reuses the existing `FS_TEST_*` gates and namespace
ownership guard. It requires all nine distinct passing fixture records from a
successful restricted pod; missing, duplicate, failed and unverified records
are rejected. The live test is ignored by default with
`not run: FS_TEST_CLUSTER unset`. Explicitly requesting it without valid opt-in
inputs fails as UNVERIFIED. Fake-cluster tests cover the gates, posture,
create/shared ownership and cleanup after failure without constructing a client.
Operator only, after the R3 merge gate, with valid KUBECONFIG and a digest-pinned
FS_TEST_WAS02_IMAGE already supplied:

```sh
FS_TEST_CLUSTER=1 cargo test --locked -p falcone-serverless-worker --test cluster_limits_cancellation -- --ignored --nocapture
```

Agents do not run that command. Cluster evidence remains unverified until the
operator records the run on #52; no images or runtime resources are published
by this source change.
