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

## Limits and cancellation (WAS-02 ac3, #125)

The limits contract is `worker/src/limits/mod.rs`: version 1, one JSON document with
`version` and nine mandatory fields. A missing, zero, `u64::MAX` or above-ceiling value, an
unknown field and an unknown version are each rejected with a typed `LimitsError` that
names the field (`worker/tests/limits_config.rs`). The ceilings equal the limits the worker
enforced before the contract (1 MiB per buffer direction, 256 MiB guest memory, 30 s per
host call, 120 s / 4 GiB compiler); none is weaker. `Limits::qualification()` is the
qualification value set, not a production default. The operator approves production values.

| Field | Ceiling | Qualification value |
|---|---|---|
| `compiler_cpu_seconds` | 120 | 120 |
| `compiler_memory_bytes` | 4 GiB | 4 GiB |
| `guest_memory_bytes` | 256 MiB | 256 MiB |
| `guest_to_host_buffer_bytes` | 1 MiB | 1 MiB |
| `host_to_guest_buffer_bytes` | 1 MiB | 1 MiB |
| `guest_deadline_ms` | 300000 | 30000 |
| `host_call_deadline_ms` | 30000 | 30000 |
| `termination_budget_ms` | 30000 | 5000 |
| `interruption_tolerance_ms` | 30000 | 2000 |

`Host::with_limits` takes the buffer, guest-memory and host-call values from the contract;
`Host::new` uses `Limits::qualification()`. Buffer limits count canonical-ABI bytes: a
`write` payload may be at most `limit - 8` and a `read` result at most `limit - 4` bytes.
One byte more is `limit-exceeded`; for guest-to-host data the binding is never called.

Guest interruption needs an engine built with `host::epoch_engine()` (a default engine has
no epoch instrumentation, which is how the blocked-host-call fixture disables it). One
`EpochClock` per engine ticks every 10 ms (`EPOCH_TICK`) for as long as any store holds a
`DeadlineGuard` from `EpochClock::arm`, and idles when none does. Because the ticker is
recurring while a deadline is pending, another store's tick, or this store's own tick
between the wall-clock check and the re-arm, can never move the deadline past the last
tick. The interruption bound asserted by the fixtures is
`guest deadline + EPOCH_TICK + interruption_tolerance_ms`. The host-call and cancellation
bounds use `interruption_tolerance_ms` too. The tolerances are named, generous values for
shared, contended runners; they are not performance targets.

Fixtures are WAT text compiled at run time with the locked toolchain; no precompiled
artifact is committed or loaded:

- `infinite-loop.wat`: never yields; only epoch interruption or process termination ends it.
- `memory-exhaustion.wat`: grows memory with no declared maximum until the guest-memory limit
  denies a growth; the host reports the guest-memory-limit outcome and the process lives on.
- `storage-io.wat`: one granted object; `write(len)` is the guest-to-host payload fixture,
  `read()` the host-to-guest result fixture and, against a binding that never returns, the
  blocked host call.
- `complete.wat`: the valid control that must still run after every failure.
- `cpu-heavy.wat`, `memory-heavy.wat`: the compiler-exhaustion inputs above.

`worker/src/qualification/limits_fixtures.rs` runs each fixture and records it passed,
failed or unverified. A check whose helper is absent (the compile child, or the
termination child) is unverified, never passed. The termination fallback is behind the
`host::TerminationFallback` trait; the only implementation, `ProcessFallback`, runs the
execution in a supervised child process that is killed and reaped. It is qualification-only
and does not select the process model of ADR 0004, which is still Proposed.

The race regression (`lost_tick_race_cannot_strand_a_never_yielding_guest`) re-executes the
test binary as a child. On a shared engine it drives another store's tick, then this store's
own tick between the wall-clock check and the re-arm, using the test-only `Clock` and
`Probe` seams of `EpochClock::arm_with`. A watchdog outside the engine kills the child after
60 s, so a lost deadline fails the test instead of hanging CI.

Operator qualification (not run by agents): with the runner's `KUBECONFIG` and an
operator-built, digest-pinned `FS_TEST_WAS02_IMAGE` already set, run exactly:

```sh
FS_TEST_CLUSTER=1 cargo test --locked -p falcone-serverless-worker --test cluster_limits_cancellation -- --ignored --nocapture
```

The pod (restricted Pod Security, seccomp RuntimeDefault, read-only root, arbitrary non-root
UID, no service-account token) runs `compile-cache-qualification --limits-cancellation` with
the fixtures in `FS_WAS02_FIXTURES`. Without `FS_TEST_CLUSTER=1` the test is ignored and
reports `UNVERIFIED: not run: FS_TEST_CLUSTER unset`; requesting it anyway fails. The namespace
guard and `FS_TEST_NAMESPACE_MODE` rules are the existing ones, and no other namespace is touched.
