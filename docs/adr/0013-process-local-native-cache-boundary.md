# ADR 0013: Process-local trusted native cache load boundary

## Status

Accepted boundary — operator decisions of 2026-11-04 for WAS-02.t6 (#135).
R3: independent second review and explicit operator approval remain required
before merge. Merge only; no runtime consumer, image publication or deployment.

## Decision

Amend ADR 0012 without changing its descriptor boundary. Permit exactly one
second audited Rust module, `worker/src/cache/load.rs`, to use unsafe code solely
for the single in-memory Wasmtime component deserialize call. The workspace
deny lint and the method deny list remain intact everywhere else. Source scans
allow these exact paths only, never sibling paths. The method lint exception
applies only to that call. File-based native load APIs remain prohibited.

This exception is authorized only when all three conditions hold:

1. The bytes were produced by this process owner's trusted compiler after raw
   component validation and successful bounded child completion.
2. The MAC verifies under the owner's randomly generated HMAC-SHA256 key, held
   only in memory and never persisted. Restarting the owner invalidates entries.
3. Provenance (engine version, configuration hash and module hash) matches the
   validated request, together with the complete cache key and native SHA-256.

Any failed check is a miss, never a native load. Tenant-supplied precompiled or
native artifacts are rejected before compile or load. The loader reads a bounded
regular file with O_NOFOLLOW into memory, verifies it, and passes that same
buffer to the engine. Fresh output passes the same gate. The store publishes a
single complete entry by rename only after verification and active-budget checks.
The caller supplies a pod-local directory; persistent/shared caches, signatures,
OpenBao key lifecycle and reference-aware cleanup require separate decisions.

## Consequences and evidence

Contract ctr-artifact-validation-compile Version 3 records the outcome mapping,
64 MiB output bound and qualification budgets (120 s CPU, 4 GiB address space,
300 s deadline for controls). ring 0.17.14 gains a production dependency edge;
no new package enters Cargo.lock. The trusted child and loader use the identical
pinned engine configuration, with fuel and epoch interruption enabled.

Tests observe public compile-or-load outcomes and test-only source/order witnesses:
every rejected entry triggers one compile and loads only fresh output; a hit
verifies the MAC and loads only the checked file buffer. They cover altered,
truncated, incomplete, foreign, tenant and symlinked entries, resource limits,
child failures, cancellation and deadlines. Cluster evidence requires the operator
to build and digest-pin the qualification image and attach evidence to #52;
real OpenShift qualification is separate.

The MAC proves origin, not compiler correctness. A compiler exploit can produce
validly authenticated native code; this change adds no child confinement.
RLIMIT_AS is not RSS, and a cgroup OOM SIGKILL can retain the existing CPU
classification. No guest/host capability, tenant execution or runtime wiring is
changed. Rollback reverts this change, restoring Version 2 and ADR 0012's sole
exception; process-owned trust does not survive restart.
