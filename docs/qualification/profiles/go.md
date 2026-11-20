# Go Wasm profile

`go-wasi-v1` is `disabled` and `not-qualified`. Build and cluster results are
UNVERIFIED: no operator evidence has been recorded. This is a source qualification
profile, with no production Go adapter or enablement change. Matrix revision 2
records the compiler as toolchainVersion 0.44.0; the companion Go toolchain is pinned
below. Reverting this source change is the rollback; no runtime state is created.

The operator selected these pins on 2026-11-19. Record them without substitution.

| Component | Version | sha256 | Source |
| --- | --- | --- | --- |
| TinyGo | `0.44.0` | `ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9` | https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz |
| Go | `1.26.11` | `9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec` | https://go.dev/dl/go1.26.11.linux-amd64.tar.gz |

| Setting | Pin |
| --- | --- |
| target | `wasip2` |
| wasi | `none` |
| wasmtime | `49.0.5` |

The WIT package is `falcone:host@0.2.0`, world `function`. The final component
exports `falcone:host/handler@0.2.0`, whose handle function accepts JSON bytes and
returns JSON bytes or a fixed handler error. The interface allow-list is:

- `falcone:host/storage@0.2.0`
- `falcone:host/http@0.2.0`
- `falcone:host/events@0.2.0`
- `falcone:host/workflows@0.2.0`
- `falcone:host/logging@0.2.0`
- `falcone:host/cancellation@0.2.0`
- `falcone:host/progress@0.2.0`
- `falcone:host/invocation-context@0.2.0`

No WASI interface is allowed. The trusted build uses the existing pinned wasi-virt
revision from `worker/qualification/javascript/versions.toml` in deny-all mode.
For componentization only, `prepare-build-wit.py` stages a build-only world that
includes the unchanged `falcone:host/function@0.2.0` world and the authenticated
TinyGo bundle's `wasi:cli/imports` world (including its I/O, clocks and random
dependencies). It copies the bundled WIT versions without selecting new pins and
fails closed if that runtime world is missing or ambiguous. This does not change
`worker/wit/host.wit` or the final component's allowed imports.
Every final component still passes the unchanged worker import gate. Any leftover
WASI import rejects qualification; there is no alternate execution path.

Third-party catalogue: empty (revision `unqualified`). There are no separately
versioned modules and nothing is downloaded or installed. Only these standard
library packages are eligible, bound to both pinned toolchains:

| Package | Toolchain |
| --- | --- |
| `crypto/sha256` | Go 1.26.11 / TinyGo 0.44.0 |
| `encoding/hex` | Go 1.26.11 / TinyGo 0.44.0 |
| `strconv` | Go 1.26.11 / TinyGo 0.44.0 |
| `strings` | Go 1.26.11 / TinyGo 0.44.0 |

`falcone-profile/bindings` is local trusted fixture glue, not a catalogue package.
TinyGo supports only parts of the standard library; this profile grants no support
for packages beyond the table. In particular, `reflect`, `unsafe`, `net/http`,
`os`, `syscall`, and dynamic loading in tenant source are rejected. The fixed
bindings alone use guest-memory pointers for the canonical ABI. They read buffers
allocated by the TinyGo runtime, keep input/output rooted through canonical
post-return, and never declare a second allocator or require arena membership.
Only the trusted-build test and cluster run can establish allocator correctness.

| Limit | Behavior |
| --- | --- |
| reflection | General reflection and dynamic code loading are unsupported. |
| concurrency | Goroutines, process spawning and threads are unsupported. |
| ambient_io | No ambient filesystem, environment or network access; use the capability host only. |
| allocator | TinyGo owns cabi_realloc; bindings accept runtime buffers and retain them through post-return. |

The compatibility checker scans all supplied files, including build-tagged files.
It checks every line or block comment before traversing imports. This includes
comments inside grouped imports, between imports and at file level. Strings are
opaque. Build tags, unknown compiler directives and export directives are refused;
line and block-form location directives are also refused. Ordinary prose such as
`algo: x` is not a compiler directive.
`go:generate` is an install hook. `go:linkname` and `go:wasmimport` both map to
`WASM-UNSUPPORTED-API` (reviewer decision recorded here). Diagnostics name a fixed
construct, file, physical line and column, never source payload or tenant values.
The static checker is eligibility screening, not a substitute for the Go compiler
or for component validation.

| Code | Diagnostic |
| --- | --- |
| `WASM-NATIVE-LIBRARY` | cgo, import C, native libraries and plugins are rejected. |
| `WASM-GPU` | GPU access is outside the capability host. |
| `WASM-PROCESS-SPAWN` | Process spawning, os/exec and goroutines are unsupported. |
| `WASM-INSTALL-HOOK` | go:generate and install/build hooks are never run. |
| `WASM-PACKAGE-NOT-IN-CATALOGUE` | Third-party modules are outside the empty pinned catalogue. |
| `WASM-UNSUPPORTED-API` | Off-allow-list standard packages, reflection, build tags, go:linkname and go:wasmimport are unsupported. |

| Negative source tree | Expected code | Location |
| --- | --- | --- |
| `negative/native-library` | `WASM-NATIVE-LIBRARY` | `main.go:3` |
| `negative/gpu` | `WASM-GPU` | `main.go:3` |
| `negative/process-spawn` | `WASM-PROCESS-SPAWN` | `main.go:3` |
| `negative/install-hook` | `WASM-INSTALL-HOOK` | `main.go:3` |
| `negative/package-not-in-catalogue` | `WASM-PACKAGE-NOT-IN-CATALOGUE` | `main.go:3` |
| `negative/unsupported-api` | `WASM-UNSUPPORTED-API` | `main.go:3` |

The six negatives are never built or executed. The default static suite checks
exact diagnostics and source locations. The restricted-pod suite embeds the same
sources and rechecks their rejection before running any positive component.

Three source examples have checked-in input and expected JSON: `echo-json` copies
an object, `uppercase` converts a JSON string using the pinned string subset, and
`hash-sha256` hashes the JSON input bytes (the empty object `{}` in its case).
Each example is under `worker/qualification/profiles/go/examples/<name>/` with
`main.go`, `input.json` and `expected.json`. They import no host interface; the
manifest lists their imports explicitly. These expected outputs are fixture
expectations, not evidence of an operator run. No prebuilt Wasm is committed.

## Rerun

Default checks need no guest toolchain cache and build or execute no Go guest:

```sh
cargo test --locked -p falcone-serverless-worker --test profile_go --test language_profile_matrix --test cluster_profile_go
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Operator-only, inside the project's trusted compiler image with networking disabled:
set `FS_TRUSTED_COMPILER_IMAGE=1`, `FS_TOOLCHAIN_CACHE` to the offline cache,
`FS_PROFILE_COMPONENTS` to a new output directory, and `TMPDIR` to disk scratch.
The cache holds both pinned archives and their matching extracted `tinygo/` and
`go/` trees, plus `bin/wasi-virt` and `wasi-virt.rev` from the existing trusted
compiler image. The script checks archive SHA256 and every installed file before
using any tool. No download, installation, extraction or toolchain build occurs.
The image also supplies the repository's Rust toolchain and offline crate cache.

```sh
worker/qualification/go/build-profile.sh
cargo test --locked -p falcone-serverless-worker --test profile_go -- --ignored --exact trusted_components_have_one_allocator_and_only_declared_imports --nocapture
```

The script builds only the three positive sources using `tinygo build
-target=wasip2` and the build-only `function` world, then denies WASI. It retains
TinyGo's pre-composition output under `raw/<name>.wasm`. The trusted-build check
validates that output and requires exactly one core module exporting
`falcone:host/handler@0.2.0#handle`, with exactly one function export named
`cabi_realloc` in that same module. An adapter allocator cannot satisfy that check.
The final composed component separately passes full validation of all nested
modules and the unchanged import gate; wasi-virt modules may own their own
allocators. Both raw and final outputs are covered by `SHA256SUMS` and packaged
alongside the runtime test binary and compiler child. It never executes a guest.
A SHA or import mismatch fails closed. Without FS_TOOLCHAIN_CACHE the ignored export check prints UNVERIFIED
with the reason and contributes no evidence.

An operator packages `bin/profile-go-runtime` and `bin/artifact-compile-child`
under `/usr/local/bin/` and the components under
`/opt/falcone-toolchain-cache/go-profile/` in a digest-pinned qualification image.
Set `FS_TEST_CLUSTER=1`, `FS_TOOLCHAIN_CACHE`, `KUBECONFIG` (paths only), and
`FS_TEST_WAS02_IMAGE=<repository>@sha256:<digest>` before running:

```sh
cargo test --locked -p falcone-serverless-worker --test cluster_profile_go -- --ignored --nocapture
```

The existing cluster harness defaults to creating and deleting its own `fs-test-*`
namespace. If creation is denied it fails before object mutation and names
`FS_TEST_NAMESPACE_MODE`. For an operator-selected shared namespace, set
`FS_TEST_NAMESPACE_MODE=shared` and `FS_TEST_NAMESPACE`; the harness never creates
or deletes that namespace and only touches objects labelled with its own run id.
Pods use an arbitrary non-root UID, RuntimeDefault seccomp, no privilege escalation,
a read-only root filesystem and no service-account token. No secrets are read or
printed. Only the prebuilt positive components execute, through raw validation and
trusted compilation. Tenant-supplied native/precompiled Wasmtime input is rejected
before any native load. Cache/cluster absence is UNVERIFIED, never qualification.

Attach the successful build/export and cluster evidence to story #53 as artifact
IDs plus SHA256 (the output bundle includes SHA256SUMS). Keep the manifest
not-qualified and matrix disabled with empty evidence until those operator runs
and the subsequent enablement decision. R3 requires independent second review and
explicit operator approval before merge. Fuzz, leakage, crash qualification,
catalogue registration and the production execute adapter remain separate work.
