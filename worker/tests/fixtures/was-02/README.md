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

Version 1 requires every field; `Limits::parse` returns a typed error with the
field name for missing, zero, unbounded, invalid or unsupported values. No tenant
value can raise the existing 1 MiB buffer, 256 MiB guest memory or 30 s host-call
ceilings. Compiler qualification ceilings are 120 s CPU and 4 GiB virtual address
space (RLIMIT_AS, not RSS). Compiler fixture controls retain the existing 300 s
wall deadline. These are qualification values, pending production profile review.

The qualification profile uses a 100 ms guest deadline, 50 ms independent
host-call deadline and 500 ms termination budget. Timing assertions allow 2 s
of scheduler tolerance, separately from the configured budget. This is evidence
under the test scheduler, not a real-time guarantee. CPU and memory failures must
leave the compile cache empty, and a following small component must compile.

Wasmtime epoch deadlines are relative to the engine's current epoch. Each store
with a pending guest deadline owns a recurring 5 ms epoch ticker until the store
is dropped. An early tick from another store may re-arm this store to current+1;
a tick racing that re-arm cannot lose the deadline because subsequent ticks are
guaranteed while this store remains alive. The deterministic regression forces
another store's tick and then this store's tick after the wall-clock check and
before re-arm. It does not use sleeps to select that interleaving.

Cancellation is not proof of rollback. Once a mutating operation is handed to
the helper, only a binding reply acknowledges completion. An unacknowledged
write, publish, workflow start/signal, progress/checkpoint or HTTP operation is
recorded as `Uncertain` on cancellation, timeout or helper failure. HTTP is
conservatively mutating even for nominally read-only methods. A later epoch or
memory trap cannot erase that record. Reads may report `Cancelled`. WIT stays
unchanged; the typed invocation outcome carries information beyond its host-error
variant. Retrying an `Uncertain` invocation requires platform reconciliation.

Buffer sizes include canonical aggregate overhead, as in the capability gates.
Guest-to-host excess is rejected before calling the binding; host-to-guest excess
is rejected after the binding produces its reply and before lowering into guest
memory. A host result cannot be measured before the binding produces it.

The cancellation-ignoring termination probe re-execs a fixed trusted qualification
binary and terminates/reaps only that child, while a concurrent valid guest runs.
It neither selects nor accepts ADR 0004's production process model. Ordinary helper
permit/detach accounting is retained, including its destructor qualification.

Cluster evidence is **unverified** in an offline maker run. The operator builds
and digest-pins the existing qualification image and runs:

```sh
FS_TEST_CLUSTER=1 cargo test --locked -p falcone-serverless-worker --test cluster_limits_cancellation -- --ignored
```

The live test is ignored unless explicitly selected and requires the environment
gate. It reuses the restricted namespace guard, arbitrary non-root UID,
RuntimeDefault seccomp, read-only root and labelled shared-namespace cleanup.
No cluster access or credentials are needed by ordinary fixture tests.

`Host::new` uses the explicit compatibility profile (including the existing 30 s
call cap); it supplies no parsing defaults. `Host::with_limits` accepts only a
validated contract. The small runtime fixtures use 2 MiB guest memory and 128-byte
aggregates to exercise configurable enforcement, with the deadlines above.
`--limits-cancellation` on `compile-cache-qualification` emits one JSON result per
case; any failed assertion, compiler outcome, recovery or termination check makes
the runner fail. The three guest WAT sources are also embedded under
`worker/src/qualification/limits_fixtures` for source-only image builds; tests
compare the embedded sources with these raw fixture files to prevent drift.
The instantiation helper remains qualification-only and consumes the caller's
engine configuration. Deadline fixtures enable epoch instrumentation explicitly;
the blocked-read control and cancellation-ignoring child deliberately disable
both fuel and epochs. This helper is not a production engine/budget admission API.
