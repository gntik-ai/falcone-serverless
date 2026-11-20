# Go Wasm profile qualification

Profile `go-wasi-v1`, matrix revision `2`, WAS-03#ac1 / FR-WAS-05.
**Status: disabled, unverified.** No component build or cluster evidence is recorded.
Qualification requires the trusted build plus the restricted cluster run and its artifact ID
and SHA256 on story #53. Static success alone never qualifies or enables this profile.

The manifest is `worker/qualification/profiles/go/manifest.toml`; the archive pins are in
`worker/qualification/go/versions.toml`. The machine-readable contract below is checked against
the manifest and both matrix copies by `profile_go.rs`; changing a pin, module binding,
interface, example or diagnostic on only one side fails the drift check.

## Toolchain and packages

TinyGo **0.44.0** uses Go **1.26.11**, target **wasip2**, WASI **0.2.0**, and Wasmtime **49.0.5**.
Archive URLs and SHA256 are copied exactly from the operator's 2026-11-19 decision below.
No separately versioned third-party Go module is catalogued. Every listed standard-library
package is bound to `go@1.26.11`; indirect stdlib implementation dependencies share that pin.
The fixture module `falcone.profile` and reviewed `bindings` package are local platform source,
never installed dependencies. TinyGo reflection (`reflect`), unsafe tenant APIs, goroutines,
filesystem/network/environment APIs and unlisted stdlib packages are unsupported. Reflection
limits and `//go:linkname` report `WASM-UNSUPPORTED-API`. There is no native fallback.

## Imports and allocator

The ceiling is the eight function-world imports in `worker/wit/host.wit`, plus exactly
`wasi:random/random@0.2.0`. The handler is an export, not an import. Capability grants still
apply to Falcone operations. Randomness is provided by the operating system CSPRNG through
already-locked `ring::rand::SystemRandom`, never a seeded PRNG. Only `get-random-u64` and
`get-random-bytes` link; byte lengths above **65536** trap before allocation. Each call charges
fuel (one plus returned bytes) and the host-call budget, retains any narrower configured
host output bound, and checks deadline/cancellation
before and after the OS call. No other WASI interface links, including random/insecure,
random/insecure-seed, cli, filesystem, sockets, clocks and io.

The fixture bindings use TinyGo's runtime `cabi_realloc` as the sole allocator export.
`Region` accepts input allocated by that allocator through a bounded linear-memory view;
it does not require pointers to come from a separate arena. Go-owned output and result slots
stay rooted for the invocation. The build uses `tinygo build -target=wasip2` with the Falcone
world, trap-only panics and no guest scheduler; no adapter or wasi-virt is used. Runtime checks require exactly one core cabi_realloc
export and one linear memory, then validate, compile and execute with Host's `memories(1)`.
The memory limit is unchanged. Tenant precompiled artifacts remain `precompiled_rejected`.

## Examples and diagnostics

`json-transform` sorts uppercased item names and computes total 17; `text-regex` extracts
orders 12 and 345; `hash-sha256` uses the independent FIPS SHA-256 vector for `abc`. Inputs
and expected JSON are literal manifest entries. Negative sources cover all six diagnostics
below and are checked statically before any component can be built. They are never built or
executed successfully. Directives are checked on every comment token, including inside grouped
imports, between imports and at file level. Diagnostics name the construct and file/line/column,
never source values or tenant data.

## Operator rerun and evidence

Default `cargo test --workspace --locked` needs no toolchain cache and builds no guest.
Only the trusted compiler image, with networking disabled and pinned archives and extracted
tools provisioned under FS_TOOLCHAIN_CACHE, runs `worker/qualification/go/build-profile.sh`.
Set `FS_TRUSTED_COMPILER_IMAGE=1`, `FS_TOOLCHAIN_CACHE`, a new `FS_PROFILE_COMPONENTS` output
directory and TMPDIR. Hash or version mismatches fail closed; nothing is fetched or installed.
The script runs static checks, builds only examples, verifies each component's imports, memory
and realloc export, executes each expected output, then packages components, evidence sources
and the runtime test binary with SHA256SUMS. The script never enables the profile.

Local trusted execution is opt-in:
`cargo test -p falcone-serverless-worker --locked --test profile_go_runtime -- --ignored --nocapture`.
The suite reports UNVERIFIED when FS_TOOLCHAIN_CACHE is absent. Package the generated bundle
in a digest-pinned trusted qualification image with `bin/*` under `/usr/local/bin` and the
remaining bundle under `/opt/falcone-toolchain-cache/profile`.

For cluster qualification set `FS_TEST_CLUSTER=1`, `FS_TEST_WAS02_IMAGE` to that image digest,
FS_TOOLCHAIN_CACHE, and the existing test kubeconfig; run
`cargo test -p falcone-serverless-worker --locked --test cluster_profile_go -- --include-ignored --nocapture`.
The shared harness defaults to creating/deleting its own fs-test-* namespace and fails before
mutation if creation is denied, naming FS_TEST_NAMESPACE_MODE. In shared mode it uses only new
run-named, run-labelled objects and removes only objects owned by that run. Pods use an arbitrary
non-root UID, RuntimeDefault seccomp, read-only root filesystem, dropped capabilities and no
service-account token. No credentials or pod logs are printed. The pod also runs the narrow
random host regression cases and rejects all negative sources before executing any positive.

Record the bundle SHA256SUMS and cluster outcome as an artifact ID plus SHA256 on #53.
**Evidence: UNVERIFIED; artifact ID and SHA256 pending.** No success or evidence reference is
invented here. R3 still requires an independent second reviewer and operator approval before
merge. Rollback is a merge-only revert; no runtime state or profile enablement changes.

## Checked contract

```json
{
  "runtime": {
    "wasmtime": "49.0.5",
    "wasi": "0.2.0",
    "target": "wasip2"
  },
  "toolchains": {
    "tinygo": {
      "name": "TinyGo",
      "version": "0.44.0",
      "source": "https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz",
      "archive": "tinygo0.44.0.linux-amd64.tar.gz",
      "sha256": "ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9"
    },
    "go": {
      "name": "Go toolchain",
      "version": "1.26.11",
      "source": "https://go.dev/dl/go1.26.11.linux-amd64.tar.gz",
      "archive": "go1.26.11.linux-amd64.tar.gz",
      "sha256": "9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec"
    }
  },
  "wit": {
    "package": "falcone:host@0.2.0",
    "world": "function",
    "interfaces": [
      "falcone:host/storage@0.2.0",
      "falcone:host/http@0.2.0",
      "falcone:host/events@0.2.0",
      "falcone:host/workflows@0.2.0",
      "falcone:host/logging@0.2.0",
      "falcone:host/cancellation@0.2.0",
      "falcone:host/progress@0.2.0",
      "falcone:host/invocation-context@0.2.0",
      "wasi:random/random@0.2.0"
    ]
  },
  "stdlib": {
    "encoding/json": "go@1.26.11",
    "encoding/hex": "go@1.26.11",
    "crypto/sha256": "go@1.26.11",
    "crypto/rand": "go@1.26.11",
    "regexp": "go@1.26.11",
    "sort": "go@1.26.11",
    "strings": "go@1.26.11"
  },
  "catalogue": {
    "revision": "unqualified",
    "modules": []
  },
  "diagnostics": {
    "WASM-NATIVE-LIBRARY": "cgo/native code is unavailable.",
    "WASM-PROCESS-SPAWN": "Processes and guest threads are unavailable.",
    "WASM-INSTALL-HOOK": "Build/install directives are never executed.",
    "WASM-GPU": "GPU/device access is unavailable.",
    "WASM-PACKAGE-NOT-IN-CATALOGUE": "The module or version is outside the pinned catalogue.",
    "WASM-UNSUPPORTED-API": "Unlisted stdlib, reflection, unsafe, linkname and undeclared host imports are unavailable."
  },
  "id": "go-wasi-v1",
  "status": "disabled",
  "testedExamples": [
    "json-transform",
    "text-regex",
    "hash-sha256"
  ]
}
```
