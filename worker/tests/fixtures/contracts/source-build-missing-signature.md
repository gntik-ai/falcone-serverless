# Source build API

Contract: ctr-source-build-api
Version: 1

This contract provides `ctr-source-build-api` to Falcone build orchestration.
It defines the source-to-Wasm compiler boundary; it implements no build service,
launcher, artifact validator or OCI image assembler. The caller authenticates
with the Falcone build orchestration service identity token
(`ctr-platform-service-identity`). Transport is HTTP over TLS inside the cluster.
Credentials and key material are never examples or values in this contract.

## Request fields

All fields are required. References are immutable and tenant-scoped; the service
checks their authorization before accepting work. No request grants access to
another tenant's artifacts or credentials.

| Field | Meaning |
| --- | --- |
| `source_ref` | Digest-pinned source object reference, fetched through the host storage API. |
| `language_profile` | Approved profile from `ctr-language-profile-matrix`; must equal the selected base's profile. |
| `catalogue_version` | Immutable dependency catalogue version, resolved to a pinned digest. |
| `base_ref` | Object with `id`, `version` and `digest`: catalog id@version plus its immutable digest, as in ADR 0010. Only a catalog Wasm base with the matching profile is accepted here. |
| `output_prefix` | Authorized tenant-scoped output object prefix; never a local filesystem path. |
| `compile_limits` | Aggregate CPU seconds, memory bytes, process count, output bytes and wall-clock duration bounds; cannot exceed service policy. |
| `build_credential` | Per-build authorization consumed only by the service host binding layer; format belongs to `ctr-build-credential` (Falcone). |

Falcone owns catalog visibility and lifecycle eligibility under ADR 0010 D8;
this API does not implement the base catalog API or authorize OCI execution.

## Response fields

A successful result contains every field below. Artifact validation and compile
rules belong to `ctr-artifact-validation-compile`; this document does not replace
WAS-02 or the WAS-03 profile matrix.

| Field | Meaning |
| --- | --- |
| `wasm_digest` | Digest of the validated Wasm component, never a tenant-supplied native compiled cache. |
| `base_digest` | Echo of the exact catalog base digest used for composition. |
| `sbom_ref` | Immutable SBOM object reference. |
| `provenance_ref` | Immutable provenance reference recording inputs, catalogue, toolchain and base digests. |
| `attestation_ref` | Immutable build attestation reference covering those digests and the result. |
| `REMOVED` | Reference to the Ed25519 signature of the attestation. |
| `signing_key_id` | Public identifier of the signing key, permitting rotation without re-signing retained artifacts. |
| `imports` | Declared, validated component imports. |
| `features` | Declared, validated Wasm features. |
| `diagnostics` | Bounded build and validation diagnostics, with credentials redacted. |

## Outcomes

- Success is returned only after validation and all isolation rules hold.
- `limit_exceeded` rejects the build and names the exceeded limit in diagnostics;
  no successful artifact result is published.
- `base_mismatch` rejects a missing catalog match, a profile mismatch or any
  composition whose base digest differs from the request; no substitute base,
  floating alias or automatic rebase is allowed.
- Authentication, authorization, invalid input and unsupported profiles fail
  closed with bounded diagnostics; they never select a native fallback.

## Build isolation rules

- `SB-R1`: Use only approved trusted toolchains and a digest-pinned dependency
  catalogue and catalog base. No dependency fetch outside the approved inputs.
- `SB-R2`: Never execute a tenant hook, Dockerfile, build script or native binary.
  Reject tenant-supplied precompiled Wasmtime artifacts; never deserialize them.
  Trusted toolchains compile the validated component only, with no native fallback.
- `SB-R3`: Enforce one aggregate process-tree budget for CPU, memory, processes,
  output bytes and wall-clock time, including every descendant. On any limit or
  cancellation, terminate and reap the entire tree. All descendants must end
  before any result (including an error) is returned.
- `SB-R4`: Allocate a fresh per-build workspace isolated from every other build
  and tenant. Do not reuse tenant state or expose another tenant's artifacts.
- `SB-R5`: The per-build credential remains in the service host binding layer.
  It never reaches toolchains, environment, config, workspace, output or logs.
- `SB-R6`: Read and write artifact storage only through the host storage API
  (`ctr-host-storage-api`, STO-01), authorized for this build; no direct object
  store credential or endpoint access from the build runner.
- `SB-R7`: Toolchains have no sockets except unnamed Unix socketpairs provided
  by the trusted launcher. Deny network sockets, named Unix sockets, message-queue
  IPC and cross-process memory access, including access to the service process.
- `SB-R8`: Landlock plus seccomp enforce the workspace, filesystem, socket, IPC
  and process-access restrictions. Readiness fails closed if either is missing
  or insufficient for the configured minimum ABI and required restrictions.
  Restricted pods, arbitrary non-root UID, read-only root filesystem, no host
  access and seccomp RuntimeDefault remain mandatory; no privileged workaround.
- `SB-R9`: Stage inputs and collect outputs only with descriptor-relative,
  no-follow, beneath-workspace opens of single-link regular files. Reject links,
  special files and escapes. Both operations require an empty build process tree:
  stage before starting it, and collect only after all descendants are reaped.
- `SB-R10`: Compose onto exactly the `base_ref` digest, and require
  `language_profile` to equal that base's profile. Verify the actual base and echo
  it as `base_digest`; otherwise fail with `base_mismatch`. No tag resolution or
  substitution is permitted during composition. OCI assembly is a separate
  trusted component and runs no tenant hooks or RUN steps (ADR 0010).

## Design basis

Snapshot sources: [FR-DEP-01/02, design:121-122](../falcone-serverless-design.md#dep-build-revisions-and-deployment)
and DEP-01 acceptance at design:483-487; [ADR 0010](../adr/0010-oci-execution-mode-and-base-catalog.md),
especially catalog and revision base at lines 30-34. [AGENTS.md:24-26](../../AGENTS.md)
fix the artifact and host-capability boundaries; AGENTS.md:34 requires a second
reviewer and operator approval for R3; AGENTS.md:52 defines the existing External
Secrets supply boundary. No credential, key material, secret-store path or
Secret value is specified here. Rollout is merge-only; there is no runtime consumer.
