# Falcone host capability contract

package: falcone:host@0.2.0

The package stays pre-1.0 until WAS-03 qualification. Consumers pin the exact
version. A breaking change requires a new version; once a revision pins 0.2.0,
publish a corrected version rather than modifying that pinned contract. Raising
a ceiling is compatible; lowering one breaks guests.

## Version notes

- 0.2.0 adds the exported `handler` interface, so a guest can receive its
  parameters and return a result. The eight imports and their signatures are
  unchanged. This is a breaking change: a component must export
  `falcone:host/handler@0.2.0`, and every import is named `@0.2.0`.
  No enabled language profile depended on 0.1.0.
- 0.1.0 declared imports only. A `@0.1.0` import is an `undeclared_import` now.

World `function` imports exactly `storage`, `http`, `events`, `workflows`,
`logging`, `cancellation`, `progress` and `invocation-context`, and exports
exactly `handler`. Linking is deny-by-default: only these versioned Falcone
interfaces are permitted. This package grants no WASI filesystem, sockets,
generic HTTP, environment or secret interface. WAS-03 owns any separately
qualified WASI language profile.

## Handler export

`handler.handle: func(params: list<u8>) -> result<list<u8>, handler-error>` is
the only export. `params` is the JSON legacy envelope and must not exceed
`max-host-to-guest-bytes`; the returned list is the JSON result envelope and must
not exceed `max-guest-to-host-bytes`. `handler-error` is
`variant { invalid-input(string), failed(string) }`.

The artifact validator checks the exported instance and the exact `handle`
signature on the raw component, before any compilation or instantiation. A
missing, extra or mistyped export fails with `missing_handler_export`. The
execute path maps outcomes to bounded codes only and never returns partial
output: `limit_exceeded`, `timeout`, `cancelled`, `trap`, `invalid_input`,
`handler_failed`. After the guest returns, whether it succeeded or failed, the
host checks cancellation first and then the guest deadline, so a cancelled or
expired invocation never reports an application error.

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
