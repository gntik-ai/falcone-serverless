# ADR 0002: Repository ownership

## Status

Accepted — operator, 2026-10-22. Records the operator decision in `AGENTS.md:8-16`.

## Context

Design §3 (`docs/falcone-serverless-design.md:252-263`) separates public APIs, admission, workers, controllers and packaging, but does not assign repositories. `docs/README.md:46` leaves ownership assignment conditional. The later operator decision in `AGENTS.md:8-16` settles that condition. Relevant stories are MIG-01 (#48), RUN-01 (#18), DEP-03 (#23), INV-02 (#25) and OPS-01 (#45).

## Decision

Record the repository boundaries from `AGENTS.md:8-16`:

| Repository | Responsibility |
| --- | --- |
| `gntik-ai/falcone-serverless` | Rust execution worker, trusted worker/compiler components and Go namespaced pool controller |
| `gntik-ai/falcone` | Public Functions API, quotas, authorization/admission/dispatch, console, SDK and the `functions.invoke` Temporal activity |
| `gntik-ai/falcone-charts` | Helm chart deploying worker pools and the controller |

A cross-repository story is split into one issue per repository, each with one accountable owner. The conditional destination guidance at `docs/README.md:46` is superseded for implementation ownership by this recorded decision; this change does not edit that file or transfer existing issues.

For design §5 item 7 (`docs/falcone-serverless-design.md:694`), this settles repository destinations only. The platform operator remains accountable for assigning maintainers, on-call coverage, security approvers, the SLA budget and Project destination; those operational assignments are not established by this ADR.

## Alternatives

- Keep all implementation in this repository: conflicts with Falcone ownership of product policy and the operator's explicit split.
- Put workers and controller in Falcone or charts: conflicts with the assigned source ownership and mixes runtime implementation with product APIs or packaging.
- Transfer existing issues automatically: publication and issue management are outside this documentation change.

## Consequences

Runtime code must not compensate silently for missing Falcone or chart capabilities. Maintainers coordinate versioned contracts across repositories. Existing backlog placement is historical evidence, not permission to implement another repository's responsibilities here. Named operational assignments remain review work for the platform operator.

## Amended by ADR 0010

2026-10-27. [ADR 0010](0010-oci-execution-mode-and-base-catalog.md) adds components to the boundaries above:

- `gntik-ai/falcone-serverless` also owns the trusted OCI components: `oci-shim`, `egress-gateway`, `image-intake` and `image-assembler`.
- `gntik-ai/falcone` owns the OCI authorization, the base catalog API and schema, and their console pages.
- `gntik-ai/falcone-charts` owns the OCI values, the OCI pod-template defaults and the OCI-namespace NetworkPolicy projections, and renders the intake and assembler as restricted pods.

Line citations above to the design and `AGENTS.md` refer to their versions before ADR 0010 (commit `d459340`); the ADR 0010 revision shifts them.
