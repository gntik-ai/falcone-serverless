# Artifact validation and bounded compilation

Contract: ctr-artifact-validation-compile
Version: 1

This versioned contract separates raw Wasm validation, bounded trusted compilation
and verified native cache loading. It specifies a documentation boundary for
WAS-02.t3; no validator, compiler, supervisor or cache loader is implemented here.

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
| `cache_key_mismatch` | An entry's key is incomplete or differs from the expected key. |

## Design basis

[`FR-WAS-04`](../falcone-serverless-design.md#was-wasm-qualification-and-capability-contract) requires separate validation, bounded compilation and trusted native cache generation. [`WAS-02`](../falcone-serverless-design.md#was-wasm-qualification-and-capability-contract-1) acceptance 2/3 requires trusted cache loading, compiler limits, buffer limits and cancellation fixtures. The [runtime contract and artifact security](../falcone-serverless-design.md#runtime-contract-and-artifact-security) defines raw Wasm validation and trusted cache identity; the [host capability model](../falcone-serverless-design.md#host-capability-model) limits fuel/epoch to guest computation. The [trusted compiler image and Job discussion](../falcone-serverless-design.md#technology-and-deployment-model) motivates process separation without choosing a transport or workload kind.

[AGENTS.md](../../AGENTS.md) forbids deserializing tenant precompiled artifacts and requires a second reviewer plus operator approval for R3. [ADR 0003](../adr/0003-wasmtime-compilation-strategy.md) remains Proposed; this contract does not claim WAS-01 qualification or select an engine/backend.

Source builds belong to `ctr-source-build-api` ([source-build.md](source-build.md)); revision metadata belongs to `ctr-revision-runtime-manifest` (WAS-02.t2). Compiler image/runtime configuration and build authorization belong to DEP-01. This contract defines none of those interfaces. No credentials, keys, secret paths or Secret values are specified here.

The executable fixtures are documentation gates, not runtime validation or containment evidence. Rollout is merge-only with no runtime consumer; revert this document and its test before a consumer pins Version 1. Future runtime implementation and semantic review must preserve every boundary above.
