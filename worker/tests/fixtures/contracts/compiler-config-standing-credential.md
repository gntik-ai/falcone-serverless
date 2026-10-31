# Compiler runtime configuration

Contract: ctr-compiler-runtime-config
Version: 1

This contract provides `ctr-compiler-runtime-config` for the compiler service,
trusted build runner and chart. It defines configuration names and boundaries;
it implements no parser, launcher or chart wiring. Values are deployment inputs,
not embedded credentials. Paths below are configuration keys, with no key material,
secret-store path or Secret value. `ctr-source-build-api` defines build behavior.

## Service keys

| Key | Meaning |
| --- | --- |
| `listen_port` | HTTP-over-TLS source build API port inside the cluster. |
| `metrics_port` | Metrics listener port; never expose credentials or source contents as labels. |
| `probe_port` | Startup/readiness listener port; fail closed when sandbox enforcement is unavailable. |
| `compile_limits` | Service policy ceiling for aggregate CPU seconds, memory bytes, processes, output bytes and wall-clock duration. |
| `toolchain_dir` | Read-only approved trusted toolchains, unavailable for tenant modification. |
| `catalogue_dir` | Read-only approved catalogue whose version and digest are pinned per build. |
| `cache_output` | Service-owned cache location for trusted compiler outputs, isolated from tenant workspaces; never accept tenant precompiled caches. |
| `storage_endpoint` | Host storage API endpoint (STO-01); never a direct object-store credential. |
| `tls_cert_path` | Read-only service TLS certificate mount path supplied by chart wiring. |
| `jwks_url` | Verification endpoint containing public keys only; never a source of a standing credential. |

## Authentication

`falcone_service_token` = <REDACTED>

The caller presents the Falcone build orchestration service identity token
(`ctr-platform-service-identity`) over HTTP over TLS inside the cluster. Verify
the caller before accepting work. The service consumes each request's scoped
build credential in its host binding layer (`ctr-build-credential`, owned by
Falcone), never in the runner, configuration, outputs or logs. There is no standing
Falcone credential key. Service authentication is separate from artifact signing;
attestation keys are never used as caller authentication credentials.

## Artifact signing key

`signing_key_path` is a read-only mount path for an Ed25519 key from an existing
Secret supplied through External Secrets. Only the service container mounts it,
and only to sign build attestations. The operator owns generation, storage and
rotation outside this contract. The public signing key identifier accompanies
the attestation so retained artifacts do not require re-signing on rotation.
No key material, actual mount path or Secret value appears here.

## Build-runner container

Use the same trusted image as the service, with the launcher entrypoint. There
is no shared PID namespace with the service and no Secret, key or identity mount.
The runner handles one build at a time with its own CPU and memory limits; the
aggregate process-tree budget includes all descendants. Throughput scales with
compiler replicas. No pod-per-build Job or extra controller permission is required.

| Key | Meaning |
| --- | --- |
| `workspace_root` | Dedicated writable workspace volume; fresh isolated directory per build, no cross-tenant state. |
| `landlock_min_abi` | Minimum Landlock ABI required for the restrictions; insufficient Landlock or seccomp makes readiness fail closed. |
| `build_cpu_seconds` | Aggregate CPU-time ceiling for the whole build process tree. |
| `build_max_processes` | Aggregate process-count ceiling, including every descendant. |
| `build_output_bytes` | Aggregate output-byte ceiling. |
| `build_wall_clock` | Wall-clock duration ceiling for the entire build. |
| `control_socket` | Launcher-provided unnamed Unix socketpair descriptor for control, never a named filesystem or network socket. |

CPU and memory container limits and these build budgets are bounded by service
policy. The launcher applies Landlock and seccomp before executing trusted
toolchains, denies message-queue IPC and cross-process memory access, and allows
no sockets other than unnamed Unix socketpairs. Terminate and reap all descendants
before returning a result or collecting files. Staging and collection follow the
descriptor-relative, no-follow, beneath-workspace single-link regular-file rules
of the source build contract, only while the build process tree is empty.

Both containers retain restricted pod posture: arbitrary non-root UID,
read-only root filesystem except dedicated workspace/cache volumes, no privilege
escalation, all capabilities dropped, seccomp RuntimeDefault and no host access.
No automatic service-account token is mounted. The runner receives neither
per-build credentials nor the service's identity; storage flows through the host
binding layer only. Landlock availability on the target kernel is unverified;
fail-closed readiness must not be weakened to accommodate it.

## Design basis

[Design:121-122](../falcone-serverless-design.md#dep-build-revisions-and-deployment)
(FR-DEP-01/02) and design:483-487 (DEP-01) require bounded traceable builds.
[ADR 0010](../adr/0010-oci-execution-mode-and-base-catalog.md) fixes catalog base
pinning and separates the OCI assembler. [AGENTS.md:24-26](../../AGENTS.md) fix
trusted artifact and host-binding boundaries; AGENTS.md:34 requires a second
reviewer and operator approval for R3, and AGENTS.md:52 defines the existing
External Secrets supply. Secret/container/TLS chart wiring belongs to
gntik-ai/falcone-charts; key lifecycle and secret-store paths remain out of scope.
Rollout is merge-only with no runtime consumer.
