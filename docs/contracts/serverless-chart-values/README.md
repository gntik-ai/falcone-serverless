# Serverless chart values v1

Contract: `ctr-serverless-chart-values` v1. Schema:
[`chart-values.v1.schema.json`](chart-values.v1.schema.json), JSON Schema 2020-12,
`$id: ctr-serverless-chart-values/v1`. Only local `#/$defs/` references are allowed.
Every values object rejects unknown properties. These are installation inputs,
not a Helm renderer, runtime enablement, image provenance proof or permission grant.
The chart owner is gntik-ai/falcone-charts; #114 consumes this contract and #119
owns signature enforcement. No dependency or image is resolved by this contract.

## Images and registry normalization

`controller`, `worker` and `compiler` are required objects. Each requires `image`,
an object with **exactly** `repository` and `digest`. `digest` is mandatory and
matches `^sha256:[0-9a-f]{64}$` (71 characters). No tag key, repository tag or
embedded `@digest` is accepted. The optional `replicas` field is bounded below.

The only registry is Docker Hub (operator decision, 2026-11-07, #114).
Repositories are at most 255 characters, lowercase Docker reference paths:
components use alphanumeric runs separated by one dot, one or two underscores,
or one or more hyphens. Empty components and leading/trailing separators fail.
An explicit registry must be exactly `docker.io`; ports, `localhost`, `ghcr.io`,
`quay.io` and any other first component containing a dot or colon fail.
Unqualified repositories have one component (`name`) or two (`ns/name`).
After an explicit `docker.io/`, paths may have more components and contain dots.

Consumers normalize `name` to `docker.io/library/name`, `library/name` to
`docker.io/library/name` and `ns/name` to `docker.io/ns/name`. An explicit
`docker.io/name` uses Docker Hub's implicit `library/` namespace too. Explicit
`docker.io/ns/name` stays unchanged. The schema validates these forms; it does
not rewrite input. Digests provide immutability, not trusted-build provenance.

## Fields and bounds

Only the image objects, `watchedPoolNamespaces` and `controllerNamespace` are
required at the top level. Optional fields have no implicit value unless a
default is stated; JSON Schema defaults are annotations, applied by consumers.

| Field | Type, bounds and source |
| --- | --- |
| `controller.image`, `worker.image`, `compiler.image` | Required digest-pinned Docker Hub image objects, as above (#279 and the operator's registry decision). |
| `controller.replicas` | Integer 1–5. At least one controller; bounded small HA group. |
| `worker.replicas` | Integer 0–100. Zero allows idle pools; ceiling bounds initial pod fan-out. |
| `compiler.replicas` | Integer 0–10. Zero allows compile capacity to be disabled; ceiling bounds trusted compilation fan-out. |
| `poolLimits.maxPools` | Integer 1–1000. At least one pool when specified; ceiling bounds materialized Deployment/Service fan-out independently of function inventory. |
| `poolLimits.maxReplicasPerPool` | Integer 1–100. Positive scaling ceiling matching the worker replica ceiling. |
| `namespaceMode` | Absent or `preprovisioned` only (default). Matches `preflight/values.go`; ADR 0011's `create` and `shared` are not activated here. |
| `watchedPoolNamespaces` | Required non-empty array of unique DNS-1123 labels, 1–63 characters each. No wildcard or all-namespace watch (`ctr-controller-config` v1 and preflight). |
| `controllerNamespace` | Required DNS-1123 label, 1–63 characters; existing namespace holding the controller's Leases. |
| `namespaceBindingsPath` | Non-empty absolute path string, default `/etc/falcone/namespace-bindings.json`; operator-owned file (`ctr-controller-config`). This configures no guest filesystem access. |
| `leaderElectionLeaseName` | Fixed `falcone-pool-controller-leader` (`ctr-resource-ownership`). |
| `scalerLeaseName` | Fixed `falcone-pool-controller-scaler` (`ctr-resource-ownership`). |
| `leaseDurationSeconds` | Integer 1–3600, default 15. |
| `renewDeadlineSeconds` | Integer 1–3600, default 10. |
| `retryPeriodSeconds` | Integer 1–3600, default 2. |
| `falconeDesiredStateEndpoint`, `falconeStatusEndpoint` | HTTPS endpoint strings only, without userinfo, query or fragment. Operator-supplied platform endpoints (`ctr-controller-config`); no embedded credentials. |
| `serviceIdentitySecretName` | Existing Secret name only; lowercase DNS subdomain, 1–253 characters. No Secret value. |
| `platformRegistryPullSecretName` | Same name-only type; required only when OCI is enabled. Existing mirror pull Secret in each watched OCI namespace (ADR 0010 D6). |
| `metricsPort` | Integer 1–65535, default 9090. |
| `probePort` | Integer 1–65535, default 8081. |
| `prerequisites` | Optional object of existing facility references, listed below. |
| `oci` | Optional object; `enabled` is boolean, default false. `intakeMirrorRepository`, if present, uses the same Docker Hub repository rule. |
| `watchedOCINamespaces` | Array of unique DNS-1123 labels, each 1–63 characters; conditional requirements below. |

The replica and pool bounds are **provisional v1 installation guardrails chosen
for #279**, not measured capacity, quotas or an ADR-approved sizing guarantee.
ADR 0007 defers sizing to workload evidence; Falcone admission quotas still
apply. Reviewers must confirm these bounds before merge. This schema neither
implements scaling nor changes the controller's exclusive replica ownership.
The 1–3600 second duration bounds limit configuration to positive, at-most-one-hour
Lease timings; defaults come from `ctr-controller-config`. Ports use the TCP
port range. The controller configuration still requires duration > renew deadline
> retry period and distinct metrics/probe ports; cross-field comparisons are
consumer checks, not asserted by this schema.

`prerequisites.gatewayRoute.name`, `storage.name`, `broker.name`,
`networkPolicyEnforcement.name` and `existingSCC.name`, plus
`prerequisites.rbac.controllerRoleBinding`, are optional name-only strings
(lowercase DNS subdomains, 1–253 characters). Field names and types match
[`install-prerequisites.md`](../../operations/install-prerequisites.md) and
[`controller-config.md`](../controller-config.md). These references grant no
creation authority. In particular, `existingSCC` names an operator-provided SCC;
it never creates or changes one. Absent prerequisites remain the existing
preflight's responsibility. No setting enables cluster resources, privilege,
capabilities, host access, a RuntimeClass or SCC creation at any nesting level.

## OCI off by default

An absent `oci`, an empty `oci` object or `oci.enabled: false` requires no OCI
field and permits only an absent or empty `watchedOCINamespaces`. Consumers must
render no OCI resources in those cases. This repository publishes the contract;
Helm rendering and runtime enablement are separate changes.

Explicit `oci.enabled: true` requires a non-empty, unique `watchedOCINamespaces`
list and `platformRegistryPullSecretName`. These are the explicit binding and
pull-Secret fields in `ctr-controller-config` and ADR 0010 D3/D6. An optional
`oci.intakeMirrorRepository` follows the Docker Hub rule too: D6 makes the mirror
operator input, and the 2026-11-07 registry decision restricts that input here.
No OCI images or extra privileges are enabled by supplying these fields. Real
OCI authorization, isolation qualification and prerequisite checks still apply.

## Validation and versioning

`worker/tests/serverless_chart_values.rs` compiles the schema, accepts all valid
fixtures, and rejects every invalid fixture at its intended instance path and
schema keyword. It fails closed on missing/empty fixture sets, malformed JSON,
non-local references and schema compilation errors. Mutation witnesses prove
that removing the digest requirement, registry pattern or object closure cannot
silently weaken the gate. Go's public `ParseValues` accepts every valid fixture.
Fixtures contain synthetic identifiers, LF line endings and one final newline.

The inventory pins the SHA256 of the schema's exact bytes. Consumers pin v1.
A breaking change (including renamed fields or changed accepted ranges) requires
v2 and updated consumer conformance tests; do not silently redefine v1. Additive
inventory registration uses v1.2 and preserves the frozen v1 section hashes.
