# ADR 0014: Measurement-only counting allocator for capability qualification

## Status

Accepted boundary — operator decision of 2026-11-05 for the capability gate
(#109), option A. R3: independent second review and explicit operator approval
remain required before merge. Merge only; no runtime consumer, image
publication or deployment.

## Context

The #109 operator decision requires host allocation to be measured with a
counting allocator or equivalent, and asserted to stay below a small fixed bound
(256 KiB) for the oversized UTF-8 and list cases. Resident-memory measurements
(VmHWM) do not establish that bound: compilation and warm-up set an earlier
peak, and allocator reuse hides new allocations. After a freed 16 MiB warm-up,
the previous VmHWM formula reported 0 bytes for a 512 KiB allocation. Rust
installs a counting allocator only through `unsafe impl GlobalAlloc`. No crate in
Cargo.lock provides a safe counting allocator, and the worker sandbox reads only
locked crates.

## Decision

Amend ADRs 0012 and 0013 without changing their boundaries. Permit exactly one
third audited Rust module, `worker/src/qualification/counting_alloc.rs`, to use
unsafe code solely to implement `GlobalAlloc` for a measurement-only counting
allocator. The workspace deny lint remains intact everywhere else. Source scans
allow these exact paths only, never sibling paths.

The allocator is authorized only while these invariants hold:

1. Layout passthrough: `alloc`, `alloc_zeroed`, `dealloc` and `realloc` forward
   their pointer, layout and size unchanged to `System` and return its result.
   The allocator never reads or writes the memory it hands out.
2. The only state is two counters, live and peak bytes, updated with relaxed
   atomics. Nothing inside the allocator allocates, locks or logs.
3. Accounting is exact: a successful allocation adds its layout size, `dealloc`
   subtracts it, and a successful `realloc` adds the new size before subtracting
   the old one. That briefly counts both blocks, as a copying reallocation does.
   A failed allocation or `realloc` changes nothing.
4. The peak is reset only by the probes, which run in fresh child processes.

Link scope: the module is compiled only into the `capability_invocations`
integration test and the `compile-cache-qualification` binary, which declare it
with `mod`. The worker library, the worker runtime and the
`artifact-compile-child`, `qualification_runner` and OCI binaries do not include
it. `compile-cache-qualification` ships only in the qualification image
(`worker/qualification/Containerfile`). It needs the allocator because its
`--capability-gates` mode re-executes itself to run the allocation probes in the
cluster; its compile-cache mode then also runs under the passthrough allocator.

## Consequences and evidence

Tests pin the boundary. `compiler/tests/isolation/boundary.rs` lists exactly
three audited paths and rejects siblings. The test
`allocation_instrumentation_reports_an_allocation_above_the_bound` proves
sensitivity at the bound: after a freed 16 MiB warm-up, a 512 KiB allocation
must read above 256 KiB. It reads 0 under the previous VmHWM formula. The
allocation probes then measure peak heap above the live heap at the start of
each oversized call, and the eager negative controls must still exceed 32 MiB.

The counters measure heap requests through the Rust global allocator only.
Guest linear memory, thread stacks and other direct mappings are not counted;
they are bounded separately by the store limiter and the helper-thread ceiling.
No package enters Cargo.lock, and no capability, tenant execution or runtime
wiring changes. Rollback reverts this change, restoring the two-exception
boundary of ADR 0013 and the probes' previous measurement.
