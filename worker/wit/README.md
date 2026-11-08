# Falcone host capability contract

package: falcone:host@0.2.0

The package stays pre-1.0 until WAS-03 qualification. Consumers pin the exact
version. A breaking change requires a new version; once a revision pins a version,
publish a corrected version rather than modifying that pinned contract. Raising
a ceiling is compatible; lowering one breaks guests.

World `function` imports exactly `storage`, `http`, `events`, `workflows`,
`logging`, `cancellation`, `progress` and `invocation-context`. Linking is
deny-by-default: only these versioned Falcone interfaces are permitted. This
package grants no WASI filesystem, sockets, generic HTTP, environment or secret
interface. WAS-03 owns any separately qualified WASI language profile.

World `function` also exports exactly one interface, `handler`, the guest entry point
the worker calls once per invocation:

`handle: func(params: list<u8>) -> result<list<u8>, handler-error>` with
`variant handler-error { invalid-input(string), failed(string) }`.

`params` is the JSON legacy envelope and is at most `max-host-to-guest-bytes`; the
result is the JSON result envelope and is at most `max-guest-to-host-bytes`. The
worker checks the export and this exact signature on the component's type before it
instantiates the component, so a component whose `handle` is missing or typed
differently never runs a start function. The handler is not a host capability: the
per-call ceilings below and the shared `host-error` do not apply to it, and the
guest deadline and cancellation are enforced by the host around the call.

## Version history

- `0.2.0`: adds the exported `handler` interface and `handler-error`. The eight imports
  are byte-identical to `0.1.0` apart from the version. Every `@0.1.0` name was replaced:
  the import gate, the capability host, the native-cache interface version (so a native
  cache compiled for `0.1.0` can never match a `0.2.0` key), the manifest schema
  (`interfaceVersion`) and the language-profile matrix. A `0.1.0` import is rejected as
  `undeclared_import`.
- `0.1.0`: the eight imports only. No guest could receive params or return a result.

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
are structured and redacted by the host. No language profile is enabled yet, so
nothing changes at runtime until a WAS-03 qualification publishes one.
