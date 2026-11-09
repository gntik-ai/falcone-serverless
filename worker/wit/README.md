# Falcone host capability contract

package: falcone:host@0.2.0

The package stays pre-1.0 until WAS-03 qualification. Consumers pin the exact
version. A breaking change requires a new version; once a revision pins 0.1.0,
publish a corrected version rather than modifying that pinned contract. Raising
a ceiling is compatible; lowering one breaks guests.

World `function` imports exactly `storage`, `http`, `events`, `workflows`,
`logging`, `cancellation`, `progress` and `invocation-context`. Linking is
deny-by-default: only these versioned Falcone interfaces are permitted. This
package grants no WASI filesystem, sockets, generic HTTP, environment or secret
interface. WAS-03 owns any separately qualified WASI language profile.

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
are structured and redacted by the host. The capability host and qualification execution path consume this contract.
This change is merge-only; no runtime adapter or profile is deployed.

## Version 0.2.0

This breaking version adds `falcone:host/handler@0.2.0` as the sole export
of world `function`. The eight imports keep their signatures and resolve at
`@0.2.0`; imports pinned to `@0.1.0` are rejected. No deployed profile is enabled.

`handle(params: list<u8>) -> result<list<u8>, handler-error>` receives the
legacy JSON envelope and returns the JSON result envelope. `handler-error` has
`invalid-input(string)` and `failed(string)` cases. Params use the configured
host-to-guest ceiling; result and error payloads use the configured guest-to-host
ceiling, counting UTF-8 bytes without ABI metadata. No partial result is returned.
Only UTF-8 canonical string encoding is accepted, including on host imports.
Validation rejects missing exports, incorrect signatures and unsupported encodings
before compilation or instantiation. Cancellation takes precedence over deadline
expiry for both successful and application-error completions.

The qualification CLI is `falcone-serverless-worker execute COMPONENT PARAMS`,
where both paths name bounded files. It writes only a complete result to stdout,
or a stable error code to stderr. It grants no external bindings. The library
`host::execute` accepts a verified Host with configured limits; compilation in
this qualification path is in-process. A production adapter must use the existing
supervised compiler and trusted cache rather than this fixture compilation path.

Restricted-pod coverage uses `cluster_handler_execution` with
`FS_TEST_CLUSTER=1`, the existing `FS_TEST_WAS02_IMAGE` digest-pinned qualification
image and namespace switches, and `--include-ignored`. The pod selects
`compile-cache-qualification --handler-execution` and reports each security case
separately. Ordinary tests visibly skip the live case when the gate is unset.
