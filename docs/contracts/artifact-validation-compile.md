# Artifact validation and bounded compilation

Contract: ctr-artifact-validation-compile
Version: 2

This versioned contract separates raw Wasm validation, bounded trusted compilation
and verified native cache loading. Version 2 adds the unwired WAS-02.t5 component validator and Linux compile child.
It implements neither guest execution nor native cache loading or publication.

## Input fields

All fields are required. References must be immutable, digest-pinned and authorized for the requesting tenant before fetching raw Wasm modules or components.

| Field | Meaning |
| --- | --- |
| `raw_wasm_ref` | Authorized reference to raw Wasm module or component bytes; never a native cache. |
| `profile` | Qualified language/runtime profile selecting allowed imports, features and guest resource ceilings. |
| `engine_version` | Exact qualified engine version. |
| `wasi_version` | Exact qualified WASI version for the profile. |
| `interface_version` | Exact version of the permitted WIT capability interfaces. |
| `target_architecture` | Qualified compilation target architecture. |
| `engine_config` | Exact qualified engine configuration, including epoch interruption and fuel settings. |
| `limits` | CPU, compiler memory, wall-clock deadline and input-size budgets, bounded by platform/profile policy; callers cannot raise ceilings. |

## Output fields

Every result contains the following fields. Failure yields `rejected`, never a usable native artifact. Only successful validation and bounded trusted compilation may yield `accepted`. Failed validation reports only inspected metadata; unavailable metadata or digest is explicitly absent, never fabricated.

| Field | Meaning |
| --- | --- |
| `verdict` | `accepted` or `rejected`; rejected results stop compilation and cache publication. |
| `imports` | Declared imports checked against the profile and versioned capability interfaces. |
| `features` | Declared Wasm features checked against the qualified profile and engine. |
| `artifact_digest` | Digest of the exact validated raw Wasm bytes, not the native cache bytes. |
| `diagnostics` | Bounded structured entries using the diagnostic codes below, with no sensitive binding data. |

## Compile limits

- `AVC-R4`: A supervisor outside the compiler enforces every compile limit over the entire compile process tree. Missing or unenforceable budgets fail closed. On any exceeded limit, terminate and reap the tree before returning an error; publish no cache entry.
  - CPU consumption must not exceed the `cpu_seconds` budget.
  - Compiler memory consumption must not exceed the `memory_bytes` budget.
  - Elapsed compilation time must not pass the `wall_clock_deadline`.
  - Raw Wasm input size must not exceed the `input_bytes` budget before validation or compilation.
- `AVC-R5`: Validation rejects any declared memory maximum above the profile guest limit (`guest_memory_maximum`). Every declared memory is checked; unsupported memory declarations fail closed under the profile. Compiler memory and guest memory budgets are distinct.
- `AVC-R6`: Guest/host `buffer_limits` belong to `ctr-wit-capability-host` (falcone-serverless:WAS-02.t1), currently `worker/wit/README.md`; this contract does not redefine them.

Numeric defaults and qualification evidence belong to WAS-01 and the selected profile; this contract defines no new ceilings.

### Version 2 enforcement

The parent pipes stdin and applies RLIMIT_CPU and RLIMIT_AS with
`rustix::process::prlimit(Pid::from_child)` before sending any bytes. Missing,
zero or unenforceable budgets fail closed. The child reads stdin to EOF before
creating the engine; it writes no files, persists no output and spawns no
subprocesses. Thus its process tree consists of the child alone. Core dumps are
disabled by the parent. The child compiles through the component API and discards
the result. This is compile evidence, not a usable or published cache entry.

| Limit | Measured quantity | Mechanism | Code | Fixture test (`artifact_validation.rs`) |
| --- | --- | --- | --- | --- |
| `input_bytes` | Raw input length in bytes | Check before parsing or spawning | `compile_size_limit_exceeded` | `validation_order_and_input_boundary` |
| `cpu_seconds` | Child CPU time, whole-second Linux granularity | Parent RLIMIT_CPU; SIGXCPU or non-parent SIGKILL | `compile_cpu_limit_exceeded` | `cpu_budget_bounds_real_compilation` |
| `memory_bytes` | Virtual address space, **not RSS** | Parent RLIMIT_AS; allocation/mmap failure, SIGABRT or engine-reported OOM | `compile_memory_limit_exceeded` | `address_space_budget_bounds_real_compilation` |
| `wall_clock_deadline` | Monotonic elapsed time, including pipe transfer | Parent kills and waits at deadline | `compile_deadline_exceeded` | `deadline_reaps_and_allows_recovery` |
| Caller cancellation | Shared atomic cancellation state | Parent kills and waits when fired | `compile_cancelled` | `cancellation_reaps_and_allows_recovery` |
| Guest/host buffers | Aggregate serialized transfer bytes per direction | Check the existing WIT ceiling before copying | `buffer_limit_exceeded` | `bounded_transfer_preserves_destination_on_rejection` |

Every spawned-child path terminates and waits before returning, including setup,
I/O and unknown-exit failures. Parent deadline/cancel kills retain their own code;
unknown exits fail with `compile_failed`, never success. The tests use test-only
budgets calibrated for a small fixture and a deterministically expanded heavy
fixture. Production budgets remain owned by WAS-01/profile policy.

Validation order is size, raw Wasm magic (non-Wasm including ELF is rejected as
precompiled input), full component-model validation, component-only check, then
exact top-level interface-instance imports from `falcone:host@0.1.0` world
`function`. Nested core modules are validated too. AVC-R5/profile declared-memory
checks and guest memory/epoch enforcement remain RUN-01 work; this module does
not claim to implement them. No runtime consumer is wired into the worker.


## Cancellation boundaries

- `AVC-R7` (`deadline_termination`): At the deadline, the supervisor terminates and reaps the entire compile process tree before returning.
- `AVC-R8` (`caller_gone_cancellation`): When the caller goes away, cancel compilation and have the supervisor terminate and reap the entire compile process tree before returning.
- `AVC-R9` (`no_partial_cache_write`): Neither deadline termination nor caller-gone cancellation writes a partial cache entry; discard staged output and publish no entry. Cache publication is atomic and occurs only after successful completion and verification; a concurrent cancellation before publication prevents publication.
- `AVC-R12` (`guest_computation_only`): Fuel/epoch interruption covers guest computation only, never compiler execution; compile termination is supervisor-enforced. Blocking host calls also require independent host deadlines and cancellation under the capability-host contract.

## Isolation and cache rules

- `AVC-R1` (`validate_before_compile`): Validate raw Wasm before compilation; validation failure stops compilation. Validate format, imports, features, versions and guest limits before the trusted compiler receives the validated bytes. Authorization of a reference does not validate its contents.
- `AVC-R2` (`never_deserialize`): Reject all non-raw-Wasm tenant input, including native precompiled and serialized engine artifacts; never deserialize tenant-supplied artifacts. Tenant precompiled input is rejected without deserialization. Tenant serialized input is rejected without deserialization. Source builds are a separate contract; there is no native fallback.
- `AVC-R3` (`separate_trusted_process`): Compilation runs in a separate trusted process outside the worker tenant execution path. This contract is transport-neutral and chooses neither a Deployment nor a Job, but transport never permits in-process compilation. Only trusted platform compiler components generate native caches from the validated raw Wasm bytes. Restricted pods and existing isolation gates remain mandatory; no tenant native code is loaded or executed.
- `AVC-R10` (`no_in_process_compile`): The worker never compiles tenant Wasm in-process.
- `AVC-R11` (`verified_entries_only`): The worker loads only verified trusted-compiler cache entries; without one it requests a trusted compile or refuses execution. Verify trusted compiler provenance, integrity, successful completion and the complete expected cache key before loading. Mismatches and unverified, incomplete or unavailable entries fail closed; no tenant-provided native artifact substitutes for an entry.

## Cache key

Every component below is mandatory and must match the worker's expected configuration. A change to any component invalidates reuse. An entry with a missing component is unverifiable and must not load.

| Component | Meaning |
| --- | --- |
| `artifact_digest` | Digest of the exact validated raw Wasm bytes. |
| `engine_version` | Exact trusted compiler/worker engine version. |
| `target_architecture` | Exact qualified target architecture. |
| `engine_config` | Complete qualified engine configuration, including `epoch` interruption and `fuel` settings. |
| `interface_version` | Exact versioned capability interface contract. |

The `engine_config` component includes the exact `epoch` interruption and `fuel` settings. Cache provenance is independently verified; a matching key alone does not establish trust.

## Diagnostic codes

Diagnostics identify the failed element and use these stable codes. Diagnostic payloads are bounded by host limits and contain no raw artifact bytes or sensitive binding data. Errors never authorize continued compilation or execution.

| Code | Meaning |
| --- | --- |
| `invalid_module` | Malformed/non-raw Wasm or guest memory declaration violating the selected profile. |
| `precompiled_rejected` | Tenant native precompiled or serialized engine input rejected without deserialization. |
| `compile_cpu_limit_exceeded` | Compile process-tree CPU budget exceeded. |
| `compile_memory_limit_exceeded` | Compile process-tree memory budget exceeded. |
| `compile_deadline_exceeded` | Deadline expired; supervisor terminated compilation. |
| `compile_size_limit_exceeded` | Raw input-size budget exceeded before validation or compilation. |
| `compile_cancelled` | Caller went away or compilation was otherwise cancelled. |
| `unsupported_feature` | Wasm feature or requested version unsupported by the qualified profile. |
| `undeclared_import` | Import outside the declared, permitted versioned interfaces. |
| `trusted_cache_unavailable` | No verified trusted-compiler cache entry is available. |
| `buffer_limit_exceeded` | Aggregate guest/host transfer exceeds the capability-host ceiling; destination is unchanged. |
| `compile_failed` | Trusted compiler failed, a required budget could not be applied, or an unknown child exit occurred. |
| `cache_key_mismatch` | An entry's key is incomplete or differs from the expected key. |

## Design basis

[`FR-WAS-04`](../falcone-serverless-design.md#was-wasm-qualification-and-capability-contract) requires separate validation, bounded compilation and trusted native cache generation. [`WAS-02`](../falcone-serverless-design.md#was-wasm-qualification-and-capability-contract-1) acceptance 2/3 requires trusted cache loading, compiler limits, buffer limits and cancellation fixtures. The [runtime contract and artifact security](../falcone-serverless-design.md#runtime-contract-and-artifact-security) defines raw Wasm validation and trusted cache identity; the [host capability model](../falcone-serverless-design.md#host-capability-model) limits fuel/epoch to guest computation. The [trusted compiler image and Job discussion](../falcone-serverless-design.md#technology-and-deployment-model) motivates process separation without choosing a transport or workload kind.

[AGENTS.md](../../AGENTS.md) forbids deserializing tenant precompiled artifacts and requires a second reviewer plus operator approval for R3. [ADR 0003](../adr/0003-wasmtime-compilation-strategy.md) remains Proposed; this contract does not claim WAS-01 qualification or select an engine/backend.

Source builds belong to `ctr-source-build-api` ([source-build.md](source-build.md)); revision metadata belongs to `ctr-revision-runtime-manifest` (WAS-02.t2). Compiler image/runtime configuration and build authorization belong to DEP-01. This contract defines none of those interfaces. No credentials, keys, secret paths or Secret values are specified here.

The contract test remains a documentation gate; WAS-02.t5 adds Linux executable validation and compile-containment fixtures. These do not claim WAS-01 runtime qualification. Rollout is merge-only with no runtime consumer; revert Version 2 and its implementation before a consumer pins it. Future runtime implementation and semantic review must preserve every boundary above.
