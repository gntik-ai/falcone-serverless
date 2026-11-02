# ADR 0012: Compiler descriptor hygiene and confinement handshake

## Status

Accepted decision — 2026-11-01 operator amendment to #235. R3: implementation
still requires independent second review and explicit operator approval before
merge. This records the authorized boundary; it does not certify enforcement or
approve a rollout. Merge only; no runtime consumer or deployment in this change.

## Context

The launcher may inherit non-CLOEXEC descriptors from CI or its supervisor.
Rejecting those in the parent prevents legitimate startup. Passing them to a
compiler leaks authority. The launcher also needs proof of child confinement
before reporting Started: executing the trusted trampoline is not that proof.

## Decision

Use option (a), one audited compiler module, `compiler/src/isolate/fd_hygiene.rs`.
Change the workspace lint from forbid to deny solely to permit this module's
exception. Every crate continues to deny this construct elsewhere, enforced by a
workspace source scan. No Wasmtime, package version or worker sandbox change.
The compiler adds the already locked libc 0.2.191 as a direct dependency.

The exact boundary exposes safe functions to the parent and disposable trusted
trampoline. It may install Command's pre_exec hook, change the status descriptor's
FD_CLOEXEC flag, adopt the validated live status descriptor exactly once as an
OwnedFd, and close descriptors above stderr with close_range, excluding the status
writer. On ENOSYS it closes the descriptor list collected from /proc/self/fd in
the parent before fork. Every block documents its ownership or syscall invariant.
There is no allocation, locking or logging in pre_exec: only fcntl and error
reporting. Cleanup runs immediately after the trusted trampoline exec and before
probes or confinement, when Command's internal exec-error pipe and its other
CLOEXEC descriptors have closed. Closing that internal pipe in pre_exec would
break Rust's own spawn failure reporting. The fallback inventory is transferred
as trusted internal arguments; no scan or allocation is performed after fork
before the trampoline exec.

The status pipe is separate from stdio, CLOEXEC in the parent and toolchain exec.
Only its writer crosses the initial trusted trampoline exec; pre_exec temporarily
clears that flag and the trampoline immediately restores it before cleanup.
The child reports confinement ready or failure; exec failure has a distinct
message. EOF after ready detects toolchain exec out of band. Missing readiness
or silent death is Confinement; ready without exec is Spawn. Both phases share a
two-second setup deadline; failure kills and reaps the child. Only a surviving
unexpected descriptor after cleanup is Descriptors. The control listener remains
unconfined, and every uncertain setup fails closed and drops readiness.

Option (b), a new helper dependency, hides the same descriptor ownership and
post-fork operations in another crate without reducing the audited surface.
Option (c), retaining inherited descriptors, drops the guarantee that the
executed toolchain sees only fds 0–2, and risks inherited service authority.
Neither satisfies this ChangeSet's guarantees.

Seccomp denies connect and bind, destination-bearing sendto, cross-process
prlimit64, and metadata mutation not mediated by Landlock, including fchmodat2.
sendmsg and sendmmsg are denied unconditionally: no current profile uses
SCM_RIGHTS, whose descriptor passing requires a separately reviewed issue.
Null-destination sendto and write preserve unnamed socketpair communication.
Configured forbidden paths augment the fixed credential-presence probes; they
are never opened or read, and cannot overlap any Landlock allow-list entry.

## Consequences and qualification

Rust, Go, JavaScript and Python profiles remain subject to separate cluster
qualification. Cargo, Go and npm dependency extraction can require chmod,
utimensat or xattrs; none of these profiles is claimed qualified against this
filter. No denial is relaxed for compatibility. The chart argument wiring belongs
to gntik-ai/falcone-charts and follows this merge. Contract Version 3 requires
forbidden_paths while preserving fixed-only behavior when the argument is absent.

Tests cover the actual compiled filter, installed syscalls, readiness, bounded
handshake outcomes, inherited descriptors, and both close_range and ENOSYS
cleanup with only stdio after exec. Socket tests require an environment that
allows Unix sockets; CI must pass before merge.
Rollback reverts this whole ChangeSet, including restoring contract Version 2;
there is no partial handshake or unconfined fallback.
