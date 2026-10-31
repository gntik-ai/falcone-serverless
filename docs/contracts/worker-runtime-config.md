# Worker runtime configuration

Contract version: v1

This closed TOML contract provides `ctr-worker-runtime-config` for the Wasm worker,
chart and controller. Adding a field requires v2. Unknown keys are rejected at every
level. No runtime consumer, server, engine or cluster operation is implemented here.

## Fields

Names containing a dot refer to fields in the `[egress]` TOML table. `required`
means the field must be supplied; `none` means there is no default. All other
fields can be omitted, including the entire egress table. Pool identity and
Falcone endpoints must be nonempty. Pool identity is a Falcone-assigned opaque
identifier, not authorization derived from guest fields or Kubernetes labels.

| name | type | unit | default | required |
| --- | --- | --- | --- | --- |
| version | u32 | none | 1 | false |
| listen_port | u16 | port | 8080 | false |
| metrics_port | u16 | port | 9090 | false |
| probe_port | u16 | port | 8081 | false |
| startup_probe_path | string | path | /startupz | false |
| readiness_probe_path | string | path | /readyz | false |
| pool_id | string | none | none | true |
| concurrency | u32 | invocations/replica | 1 | false |
| default_wall_clock_ms | u64 | ms | 30000 | false |
| max_wall_clock_ms | u64 | ms | 300000 | false |
| default_memory_bytes | u64 | bytes | 134217728 | false |
| max_memory_bytes | u64 | bytes | 536870912 | false |
| drain_ms | u64 | ms | 30000 | false |
| cancel_reserve_ms | u64 | ms | 5000 | false |
| checkpoint_reserve_ms | u64 | ms | 10000 | false |
| kill_grace_ms | u64 | ms | 5000 | false |
| cache_path | string | path | /var/cache/falcone-worker | false |
| egress.resolver | string | path | /etc/resolv.conf | false |
| egress.allow_list | array<string> | URL origin | [] | false |
| falcone_api_endpoint | string | URL | none | true |
| falcone_dispatcher_endpoint | string | URL | none | true |
| dispatcher_key_path | string | path | /var/run/falcone/dispatcher-keys | false |

Each default budget must be at or below its configured maximum. The sum of
`cancel_reserve_ms`, `checkpoint_reserve_ms` and `kill_grace_ms` must be at or
below `drain_ms`, without overflow. The deployment's termination grace period
must be at least the drain budget; chart enforcement belongs to OPS-01/RUN-02.
Budget defaults are conservative contract values; runtime enforcement belongs
to RUN-02. Concurrency 1 is the initial default consistent with fresh guest state
(FR-RUN-05, design:116,332), not a measured capacity claim.

Egress defaults to deny-all. The resolver path identifies the pod's resolv.conf
as the nameserver source; the loader does not open it or perform DNS lookups.
Allow-list entries identify permitted HTTP(S) URL origins. Destination checks
are always on: private, link-local and metadata destinations are blocked;
every redirect is re-checked and DNS answers are pinned against rebinding.
There is no v1 switch to disable any of these checks. Enforcement belongs to
SEC-03/RUN-02, and is not provided by this parser.

Only the dispatcher verification-key mount path is configured, never a key,
execution token or standing credential value. The loader neither opens that
path nor creates or opens the cache path. Service identity and token format
belong to SEC-01.t1. Credentials remain in the host binding layer.

## Restricted pod posture

These deployment requirements are fixed, not configurable TOML fields.
`runAsNonRoot=true` requires arbitrary non-root UID compatibility; there is no
fixed `runAsUser`. The writable cache is a dedicated volume under the otherwise
read-only root filesystem. Worker service accounts have no automatic API token.

| requirement | required value |
| --- | --- |
| readOnlyRootFilesystem | true |
| runAsNonRoot | true |
| allowPrivilegeEscalation | false |
| capabilities.drop | ALL |
| seccompProfile | RuntimeDefault |
| automountServiceAccountToken | false |
| standingFalconeCredential | none |

## Canonical example

`--check-config PATH` parses and validates the file and prints the effective
configuration as JSON. Errors go to stderr with a nonzero exit code. It runs no
guest, starts no server, contacts no endpoint and accesses no cluster.

```toml
version = 1
listen_port = 8080
metrics_port = 9090
probe_port = 8081
startup_probe_path = "/startupz"
readiness_probe_path = "/readyz"
pool_id = "tenant-a-interactive"
concurrency = 1
default_wall_clock_ms = 30000
max_wall_clock_ms = 300000
default_memory_bytes = 134217728
max_memory_bytes = 536870912
drain_ms = 30000
cancel_reserve_ms = 5000
checkpoint_reserve_ms = 10000
kill_grace_ms = 5000
cache_path = "/var/cache/falcone-worker"
falcone_api_endpoint = "https://falcone.example.test/api"
falcone_dispatcher_endpoint = "https://dispatcher.example.test"
dispatcher_key_path = "/var/run/falcone/dispatcher-keys"

[egress]
resolver = "/etc/resolv.conf"
allow_list = []
```

## Design basis

Current snapshot citations in `docs/falcone-serverless-design.md`:
FR-RUN-01 at line 112; bounded probes/drain at line 115 (FR-RUN-04);
arbitrary-UID restricted posture and no automatic API token at lines 159,312
(FR-SEC-03 and deployment authority); destination policy at line 162
(FR-SEC-06); allow-listed HTTP, redirects and rebinding checks at line 351;
host-held credentials at line 356. Proposed ADRs 0004–0007 provide context,
without changing their approval status. R3 requires a second reviewer and
operator approval before merge (`AGENTS.md:34`).
