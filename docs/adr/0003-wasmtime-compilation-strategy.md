# ADR 0003: Wasmtime compilation strategy

## Status

Proposed — 2026-10-22. Recommendation for later review; no engine or version is selected here.

## Context

Design §3 calls for a pinned engine and patch policy (`docs/falcone-serverless-design.md:256-265`), validated inputs and trusted caches (`docs/falcone-serverless-design.md:281-285`). WAS-01 (#51) requires actual compile/load and executable-memory evidence (`docs/backlog.md:279`); WAS-02 (#52) covers safe compilation and host boundaries. RUN-01 (#18) will implement the worker.

There is no Wasmtime dependency in `worker/Cargo.toml`; `worker/src/main.rs:3-4` is a placeholder. Cranelift, Winch, Pulley, executable-memory requirements and an exact Wasmtime version pin are **unverified** in this repository. Their availability, interfaces and suitability must not be inferred from the design's research snapshot.

## Decision

Recommend evaluating Cranelift as the initial trusted compilation candidate during WAS-01 (#51). Compare Winch for startup/throughput trade-offs only if the chosen engine release supports the required component/WASI profiles. Evaluate Pulley as a potential interpreter alternative when executable memory is unavailable; whether it avoids the relevant policy restriction and supports the required profiles is unverified. It is not a promised fallback. If no candidate passes existing restricted-pod policy, report infeasibility and keep the affected profile disabled; do not escalate permissions or execute tenant native code.

Recommend that the runtime maintainer select an exact Wasmtime version only after qualification, record it with WASI/WIT, target and engine configuration, and keep the eventual manifest and lockfile synchronized. No exact version is asserted by this proposal. Compile only validated components through bounded trusted build components; never deserialize a tenant-supplied precompiled artifact. Any trusted native cache must have verified provenance and keys covering artifact digest, engine version, target and configuration; invalidate it on relevant changes.

Recommend assigning engine advisory/CVE monitoring and triage to the runtime maintainer, with the security approver assessing exposure. An affected profile should be blocked when its risk cannot be mitigated under existing rules; upgrades require a reviewed exact pin, cache invalidation and rerun qualification before re-enablement. An urgent patch is not permission to bypass safety gates. Any sandbox-boundary change still requires `AGENTS.md:24` review.

## Alternatives

- Select Winch immediately: unsupported by repository evidence and may trade throughput for startup differently across profiles.
- Promise Pulley as a universal policy workaround: its compatibility and executable-memory behavior have not been tested here.
- Accept tenant-native caches or use an unpinned engine: violates provenance or reproducibility constraints.

## Consequences

Acceptance would establish a qualification and patch process, not certify any engine backend. The runtime maintainer owns the comparison and exact-pin evidence; the platform operator supplies policy prerequisites. WAS-01 remains unrun, and engine support, executable memory and latency claims remain unverified.
