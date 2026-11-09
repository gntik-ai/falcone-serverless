# Falcone host capability contract

package: falcone:host@0.2.0

The package stays pre-1.0 until WAS-03 qualification. Consumers pin the exact
version. A breaking change requires a new version; once a revision pins 0.2.0,
publish a corrected version rather than modifying that pinned contract. Raising
a ceiling is compatible; lowering one breaks guests.

World `function` imports exactly `storage`, `http`, `events`, `workflows`,
`logging`, `cancellation`, `progress` and `invocation-context`. Linking is
deny-by-default: only these versioned Falcone interfaces are permitted. This
package grants no WASI filesystem, sockets, generic HTTP, environment or secret
interface. WAS-03 profiles must use these Falcone imports only.

## Per-call ceilings

max-guest-to-host-bytes: 1048576
max-host-to-guest-bytes: 1048576
host-call-timeout-ms: 30000
cancellation: Blocking calls are cancelled independently of fuel/epoch.

Every interface declares these same machine-readable lines in its WIT doc
comments. Byte limits cover the aggregate serialized data in each direction,
including metadata and HTTP headers. Each is a 1 MiB ceiling, with a 30 second
host-call timeout. Blocking calls need host cancellation and deadlines even
when guest fuel/epoch interruption is enabled. The cancellation interface also
provides a state query.

The contract test resolves WIT with wit-parser, checks the interface doc comments
and compares the package and limits here with WIT. These static checks do not
enforce limits, authorization or linking at runtime: RUN-01 and RUN-02 must
implement and test those boundaries.

## Scope, handles and errors

All authorization and tenant/workspace routing come from verified invocation
context. Object/checkpoint keys, event destinations and workflow names select
only within host-authorized bindings, never caller-chosen scope. HTTP is limited
to allowed destinations; the host must validate redirects, DNS rebinding,
metadata/private ranges, sizes and deadlines. Credentials remain in host
bindings. Invocation context is read-only and takes no parameters.

Object, checkpoint and workflow handles are resources, execution-scoped and
invalid after completion. Checkpoints are bound by the host to revision, schema,
input digest and a fenced attempt. Progress reporting is explicit application
state; it does not promise arbitrary-memory restoration.

`cancellation` defines the shared `host-error` variant used by storage, HTTP,
events, workflows, progress and logging results:

- `denied`: the verified capability context does not authorize the operation.
- `limit-exceeded`: a host buffer or operation limit was exceeded.
- `timeout`: the host-call deadline expired.
- `cancelled`: the invocation or operation was cancelled.

Failures must not expose credentials or internal binding details. Log fields
are structured and redacted by the host. This change is merge-only; no runtime
adapter or language profile is enabled.

## Version 0.2.0 and handler execution

Version 0.2.0 breaks the 0.1.0 contract by exporting `falcone:host/handler@0.2.0`.
The same eight imports retain their signatures and move to 0.2.0 together.
`handler.handle(params: list<u8>) -> result<list<u8>, handler-error>` carries the
legacy JSON input and result envelopes. `handler-error` has exactly
`invalid-input(string)` and `failed(string)` cases.

Only UTF-8 canonical string encoding is accepted. The artifact gate checks the
single handler instance export, its complete function signature, imports and
canonical encoding options before `Component::new`, instantiation or start code.
Non-UTF-8 canonical options, including async intrinsics and nested components,
are rejected with `unsupported_string_encoding`; invalid handler types use
`invalid_handler_signature`, and missing or different exports use
`missing_handler_export`.

`host::execute` and `host::Prepared` retain the Host until every completion has
passed `outcome::classify`. Uncertainty keeps its existing highest precedence.
Completion checks cancellation first, then the absolute guest deadline, on both
success and error paths. No rejected invocation returns partial output.
Params use the configured host-to-guest byte ceiling. Wasmtime's return-lifting
fuel uses the configured guest-to-host ceiling and charges list/UTF-8 string bytes
before allocating result or error payloads. Standard lifting owns the result
before guest post-return cleanup runs. The ceiling counts payload bytes, so
exactly-limit payloads are accepted without a fixed metadata deduction.
Call hooks select that budget only after guest code returns. Guest code,
including realloc and post-return cleanup, and host imports retain the runtime's
existing memory-based fuel allowance for lazy cursors. Import payload preflight
still returns the WIT `limit-exceeded` error instead of trapping on cursor
metadata, and in-limit logging fields and HTTP headers keep their existing budget.
Wasmtime 49.0.5 keeps its fuel-exhaustion error type private; the execution path
maps that exact pinned engine diagnostic to `buffer_limit_exceeded`.

The local CLI is `falcone-serverless-worker execute COMPONENT PARAMS [LIMITS]`.
COMPONENT and PARAMS are files; LIMITS is an optional version-1 limits JSON file.
It writes the result bytes on success and only a stable error code on failure.
It supplies no platform grants or credentials. This explicit source-based path
is not the supervised compiler/runtime adapter required for deployment.
Its absolute deadline currently begins in `Prepared::new`, before validation
and compilation, so preparation consumes part of the invocation budget. Epoch
interruption does not interrupt compilation. WAS-03 runtime adapter work must
resolve the guest-budget start point and retain the AVC-R3/AVC-R10 separate
trusted compiler requirement; this qualification API does not relax those gates.

`guest_execute` tests cover the public execution and validation boundaries. The
allocation probes re-execute the existing `compile-cache-qualification` binary,
keeping the counting allocator within ADR 0014's approved link scope. They bound
host heap growth below 4 KiB for a rejected 1 MiB payload at both 128 B and 1 MiB
limits and prove allocator sensitivity with eager negative controls; separate
checks pin the exact-limit and one-byte-over verdicts.
`cluster_guest_execute` has separate restricted-pod cases, including four
allocation cases. It is explicitly ignored without `FS_TEST_CLUSTER`; the
operator supplies the pinned `FS_TEST_WAS02_IMAGE` and runs it with
`--include-ignored` in the existing namespace-mode harness.
