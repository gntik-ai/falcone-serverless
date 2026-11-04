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

Allocation probes re-exec in isolated children, warm up, and compare Linux
`VmHWM`. Oversized input remains on untouched zero pages. The fixed host growth
bound is 256 KiB; eager String/Vec controls over the same fixtures must exceed
32 MiB. Probe markers prevent an accidentally empty subprocess test from passing.
The stalled-binding probe uses fresh invocations in a separate process and
verifies the production four-thread cap, fast exhaustion, timeout and recovery.

Operator qualification (not run by agents): with the runner's `KUBECONFIG` and
an operator-built, digest-pinned `FS_TEST_WAS02_IMAGE` already set, run exactly:

```sh
FS_TEST_CLUSTER=1 cargo test --locked -p falcone-serverless-worker --test cluster_capability_gates -- --nocapture
```

Unset `FS_TEST_CLUSTER` reports `not run: FS_TEST_CLUSTER unset`, skips client
construction, and keeps workspace CI green. Any set value other than `1`, or
missing/invalid inputs when opted in, fails closed. The pod uses RuntimeDefault
seccomp, a read-only root, an arbitrary non-root UID and no service-account
token. Namespace create/cleanup and labelled shared-mode cleanup reuse the
existing namespace guard and `FS_TEST_NAMESPACE_MODE`/`FS_TEST_NAMESPACE` rules.
No live cluster evidence is claimed by the in-process suite. Sandbox socket
checks report `NOT RUN: EPERM (sandbox), PR CI runs them`; the pod mode fails
closed if it cannot create its required listeners.
