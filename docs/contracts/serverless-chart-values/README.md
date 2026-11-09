# Serverless chart values

Contract ID: `ctr-serverless-chart-values`

Version: `v1`

Schema: [chart-values.v1.schema.json](chart-values.v1.schema.json)

This is a JSON Schema 2020-12 contract for raw install values. It adds no Helm
rendering or runtime behaviour. All objects reject unknown fields. The complete
field-name table is in [install prerequisites](../../operations/install-prerequisites.md).
The Go preflight continues to read its existing subset; this contract also carries
[ctr-controller-config](../controller-config.md) settings.

A values object requires `images`, `watchedPoolNamespaces`,
`controllerNamespace`, `prerequisites`, `namespaceBindingsPath`,
`falconeDesiredStateEndpoint` and `falconeStatusEndpoint`. Other fields are
optional unless OCI is enabled.

`images.controller`, `images.worker` and `images.compiler` each require
`repository` and `digest`. `digest` is exactly `sha256:` followed by 64 lowercase
hex digits. Repository strings have no tag or digest suffix. Docker Hub is the
only allowed registry: `docker.io/team/image`, `team/image`, `library/image` and
bare `image` are accepted. Consumers resolve registry-less names to `docker.io`
and bare names to `docker.io/library/`. The schema validates those original
strings itself; the tests never normalise input. The first registry-less segment
must have no dot or colon and cannot be `localhost`. Other registries, ports,
URLs and Docker Hub lookalikes (including `index.docker.io`) are rejected. The
remaining path segments use lowercase Docker repository components.

`namespaceMode` is absent or `preprovisioned`. Namespace lists are nonempty,
unique DNS-1123 labels (at most 63 characters). The controller creates no
namespaces or cluster resources. There are no pod-security overrides, admission
creation settings, host settings or credential-value fields. Secret settings
hold existing Secret names only.

OCI is disabled when `oci` is absent, `{}` or `{"enabled": false}`. Disabled OCI
allows neither a shim image nor the OCI-only controller settings
`watchedOCINamespaces` and `platformRegistryPullSecretName`. Enabled OCI requires
`oci.enabled: true`, `oci.shimImage` with the same Docker Hub digest policy,
`watchedOCINamespaces`, `platformRegistryPullSecretName`, and
`prerequisites.networkPolicyEnforcement.name`. The prerequisite is required for
Wasm installs as well, matching preflight. A declaration is not proof of
NetworkPolicy enforcement, OCI authorization or runtime qualification. ADR 0010
D6's intake-mirror location is operator input, outside this image-values contract;
this schema does not choose or configure it.

## Numeric bounds

All values below are integers; fractional values and strings are rejected.
Bounds are inclusive and are install-contract ceilings, not tenant quotas or
capacity guarantees. Optional settings are omitted to use the consumer's defaults.

| Field | Minimum | Maximum |
| --- | --- | --- |
| `replicas.controller` | 1 | 400 |
| `replicas.worker` | 0 | 400 |
| `replicas.compiler` | 1 | 400 |
| `poolLimits.maxPools` | 1 | 10000 |
| `poolLimits.maxReplicasPerPool` | 1 | 400 |
| `leaseDurationSeconds` | 1 | 3600 |
| `renewDeadlineSeconds` | 1 | 3600 |
| `retryPeriodSeconds` | 1 | 3600 |
| `metricsPort` | 1 | 65535 |
| `probePort` | 1 | 65535 |

The replica ceiling uses the design's initial roughly 400 simultaneous execution
slots; the pool ceiling uses its roughly 10000 deployed definitions. Operators
must size lower limits to actual capacity. Reviewers confirm these initial v1
bounds. Lease timing order and distinct metrics/probe ports remain consumer
requirements from ctr-controller-config; this schema does not establish runtime
readiness or compare sibling values.

## Versioning and verification

Consumers pin `v1` and the schema SHA256 registered in
[Serverless-owned contracts](../falcone-contract-inventory.md#serverless-owned-contracts).
The SHA256 below is over the exact schema bytes, including the final newline.
Additive fields require a reviewed v1.x contract update, refreshed fixtures and
hash; removals, renames and relaxed safety rules require a new major version and
an accepted ADR where applicable. The frozen MIG-01 surfaces are unchanged.
R3 requires a second reviewer and operator approval before merge.

Run `cargo test --locked -p falcone-serverless-worker --test serverless_chart_values_contract`
and `cargo test --locked -p falcone-serverless-worker --test contract_inventory`.
Fixtures must be present, nonempty and valid JSON. Validation reports the fixture,
instance path and schema rule. Local references are checked before compilation.
Mutation witnesses prove the fixtures detect missing digest requirements, open
objects and registry-policy weakening. Harness negatives exercise missing/empty
fixture directories, malformed JSON, invalid schemas and non-local references.

Schema SHA256: `sha256:b666128d7f20719abe56dae12b27a730bd8f42ade4bd703000f3b0cb0129f4a3`
