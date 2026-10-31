# Language profile matrix

Contract: ctr-language-profile-matrix

The [matrix](language-profile-matrix.json) and sibling
[JSON Schema](language-profile-matrix.schema.json) publish the Wasm language
qualification boundary required by WAS-03 and FR-WAS-05. They define data only;
there is no runtime loader or enablement gate in this change. See
[design §6, Language enablement](../falcone-serverless-design.md#language-enablement)
and the [AGENTS.md hard rules](../../AGENTS.md#hard-rules-design-6-do-not-relax-without-an-accepted-adr).

## Versioning and compatibility

`matrixVersion` identifies an immutable published matrix revision. It is a
non-empty string; the initial revision is `1`. Publish a new revision whenever
profile pins, capability ceilings, diagnostic meanings, evidence or lifecycle
policy change. Consumers must pin a revision and explicitly revalidate before
adopting another. Never silently reinterpret an existing revision or profile id.
Breaking contract changes need a new schema identifier and coordinated consumer
migration. Diagnostic codes are stable after publication.

Enabled profiles require exact versions in three-part version notation, with
optional prerelease/build identifiers, for the runtime, WASI, toolchain and
package catalogue. Ranges and floating aliases are rejected. Disabled profiles
may use the literal `unqualified` for choices not yet made. The seeds' Wasmtime
version is `49.0.4`, cross-checked against the workspace's exact dependency pin;
this pin alone makes no language compatibility claim. A package catalogue
revision must identify an immutable list of supported packages and exact package
versions; unsupported packages are rejected, never installed through a hook.

## Fields

Every profile field is required even while disabled. Empty lists and an empty
evidence object make the absence of qualification explicit.

| Field | Meaning |
| --- | --- |
| `matrixVersion` | Immutable revision of this published matrix. |
| `profiles` | Object keyed by the required languages `javascript`, `python` and `go`. |
| `id` | Stable, unique language-profile identifier, independent of catalogue entries. |
| `status` | `disabled` or `enabled`; evidence records use `pending`, `failed` or `passed`. |
| `wasmtimeVersion` | Exact trusted engine version. |
| `wasiVersion` | Exact WASI interface release qualified for this profile. |
| `toolchainVersion` | Exact release of the chosen trusted language toolchain. Its identity is fixed by the profile id and qualification evidence. |
| `packageCatalogueVersion` | Exact immutable package/version catalogue revision; no base-catalog dependency is introduced here. |
| `allowedInterfaces` | Explicit, unique list of fully qualified WIT interface identifiers. |
| `testedExamples` | Unique, non-empty example identifiers for representative qualified workloads; at least one is required for enablement. |
| `diagnostics` | Unique stable codes covering every unsupported feature below. All four are mandatory per profile. |
| `eolPolicy` | Published support end and migration/retirement policy; an unqualified seed has no support window. |
| `evidence` | Object containing qualification records, empty for these seeds. |
| `compatibility` | Evidence of expected results and real dependency behavior for representative workloads. |
| `adversarial` | Host-interface fuzz/adversarial evidence under the declared capability boundary. |
| `leakage` | Fresh-instance, cross-invocation and cross-tenant leakage evidence. |
| `crashRecovery` | Worker-crash recovery exercise with the expected invocation/checkpoint outcome. |
| `artifactId` | Non-empty immutable evidence artifact reference, never credentials or a secret value. |
| `digest` | Evidence artifact digest: `sha256:` followed by exactly 64 lowercase hexadecimal characters. |

The `allowedInterfaces` list caps grantable capabilities; membership never grants
authority by itself. Each entry must resolve at its exact package version in
[worker/wit/host.wit](../../worker/wit/host.wit), parsed with wit-parser. For
example, `falcone:host/storage@0.1.0` names an interface, not a grant. Wildcards,
unknown interfaces and version substitutions are rejected. Empty seed lists
grant no capabilities. Future consumers must still intersect this ceiling with
verified invocation grants and enforce scope, limits and cancellation.

## Diagnostics

| Code | Trigger and outcome |
| --- | --- |
| `WASM-NATIVE-LIBRARY` | A dependency requires a native library, extension, binary or tenant-supplied precompiled artifact. Reject qualification; port to supported Wasm packages. |
| `WASM-GPU` | A workload requires GPU/device access outside the declared WIT host. Reject qualification. |
| `WASM-PROCESS-SPAWN` | A workload or package requires subprocess creation or an OS shell. Reject qualification. |
| `WASM-INSTALL-HOOK` | A dependency requires an install hook, build script, Dockerfile or tenant-controlled installer execution. Reject qualification. |

No diagnostic implies a native fallback. Diagnostics may name OCI as an explicit
alternative, but must never select it or switch modes. OCI requires separate
authorization and an explicitly deployed revision under
[ADR 0010](../adr/0010-oci-execution-mode-and-base-catalog.md). Unsupported Wasm
workloads remain blocked; tenant native code is never loaded or executed by the
Wasm worker. Guests reach the platform only through the versioned WIT capability
host, with allow-listed HTTP and secrets held in the host binding layer.

## Enablement evidence

Enablement is R3: a second reviewer plus explicit operator approval are required.
A hello-world demo alone never qualifies a profile for untrusted production use.
JavaScript qualification covers the legacy handler/envelope and real dependencies;
Python covers representative analysis dependencies; Go covers its chosen
Wasm/WASI toolchain and required host interfaces, as specified by design §6.

An enabled profile must have non-empty tested examples and all four evidence
kinds. Every record must have a `passed` result, an artifact reference and a valid
digest. A missing record, `pending` or `failed` result, empty reference or invalid
digest blocks enablement. Evidence must cover the exact published versions,
examples and capability ceiling. Changing those inputs requires fresh evidence
and review before publishing an enabled revision. Lifecycle policy must publish
the support end and a migration window before retirement; a security issue may
require immediate disablement and requalification.

The schema validates evidence references and result shape; it cannot authenticate
artifacts or replace reviewers. Artifact verification, the runtime enablement
gate and real qualification belong to later WAS-03 tasks. Fixtures marked
`fixture-only` are synthetic test data, never qualification evidence or approval.
JSON Schema uses draft 2020-12 and local references only; tests perform no network,
cluster, credential or secret access. Markdown checks prove token coverage;
human review must assess the policy and prose.

## Seeded profiles

| Language | Stable id | Initial state |
| --- | --- | --- |
| JavaScript | `javascript-wasi-v1` | Disabled; no evidence or tested examples. |
| Python | `python-wasi-v1` | Disabled; no evidence or tested examples. |
| Go | `go-wasi-v1` | Disabled; no evidence or tested examples. |

Every committed seed is disabled, with an empty evidence object and empty
capability ceiling. WASI, language toolchain and package catalogue are
unqualified. No profile is enabled and no real evidence is produced by this
contract change. Publication is merge-only; rollback is a revert until a later
consumer pins a matrix revision.
