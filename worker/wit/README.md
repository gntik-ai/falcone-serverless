# Falcone host capability contract

package: falcone:host@0.1.0

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
are structured and redacted by the host. No runtime bindings consume this
contract yet; this change is merge-only and adds no guest execution behavior.
