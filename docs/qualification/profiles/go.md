# Go (TinyGo wasip2) Wasm profile qualification

Profile `go-wasi-v1`, matrix revision `2`
([matrix](../../contracts/language-profile-matrix.md), WAS-03, FR-WAS-05).

**Status: `disabled`, not qualified.** Nothing in this document enables the profile. It becomes
qualified only when every static and cluster check passes; any failure records it as `blocked`.
Built components and their evidence do not exist yet: every runtime result below is
`UNVERIFIED` until an operator runs the trusted-image build and the cluster suite and records
the artifact ID and sha256 on the story issue (gntik-ai/falcone-serverless#53). The sources in
this change were never compiled by TinyGo in the maker sandbox, which has no toolchain.

The manifest is [worker/qualification/profiles/go/manifest.toml](../../../worker/qualification/profiles/go/manifest.toml).
A static test keeps this page, the manifest, `versions.toml` and both matrix copies consistent.

## Versions

| Component | Version | sha256 |
| --- | --- | --- |
| TinyGo, target `wasip2` | `0.44.0` | `ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9` |
| Go toolchain | `1.26.11` | `9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec` |
| Wasmtime | `49.0.5` | |
| WASI | `none` | |
| WIT package | `falcone:host@0.2.0`, world `function` | |
| Package catalogue | unqualified and empty: no third-party module is accepted | |

Sources: `https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz`
and `https://go.dev/dl/go1.26.11.linux-amd64.tar.gz`. The pins are copied exactly from
`worker/qualification/go/versions.toml` (operator decision 2026-11-19). TinyGo 0.44.0 accepts Go
1.25 through 1.27. Fixtures are built only inside the trusted compiler image with no network,
and a sha256 mismatch fails the build closed. TinyGo's wasip2 target runs `wasm-tools` from the
trusted image; its version is recorded in the evidence (`toolchain.txt`) but is not pinned by the
operator decision, which is an open question for the operator.

Standard-library packages are bound to the pinned Go toolchain (each manifest entry carries
`go = "1.26.11"`): `bytes`, `crypto/sha256`, `encoding/hex`, `encoding/json`, `errors`, `math`,
`sort`, `strconv`, `strings` and `unicode/utf8`. Every other package is a diagnostic.

## Build

```
tinygo build -target=wasip2 -wit-package worker/wit -wit-world function -gc=leaking -scheduler=none -panic=trap -no-debug
```

`worker/qualification/go/build-profile.sh` runs the static checks first, builds the four
components, then runs the runtime checks and only then copies them out with a `SHA256SUMS`.

- **No wasi-virt, no composition.** `wasi-virt` is not used and not pinned. Composing a WASI-Virt
  adapter adds a second linear memory, which the store-wide `memories(1)` limit of `Host::new`
  rejects, and the revision `bca61c27499d2c290b5431cfcff013737b3ba01d` has no `--deny-all`
  option. WASI is kept out through the build configuration instead, and the worker validator
  (`validate.rs`) stays the import gate: any WASI import a build leaves behind is rejected.
  If TinyGo 0.44.0 cannot produce a WASI-free single-memory component with this configuration,
  the profile stays blocked and the operator must decide on a runtime-level shim.
- **One `cabi_realloc`.** The TinyGo wasip2 runtime exports the only `cabi_realloc`. The
  bindings declare none and keep no private arena; they lift whatever the runtime allocator
  returned. `-gc=leaking` means nothing the host wrote into is reclaimed or moved during an
  invocation. The runtime test lists every core module's exports and requires exactly one.
- **One memory.** The runtime test requires exactly one defined linear memory per component and
  runs each under the unchanged `Host::new` limits, including `memories(1)`.

## Interfaces

The final component may import only these `falcone:host` interfaces (a ceiling, not a grant;
verified invocation grants still apply):

- `falcone:host/storage@0.2.0`
- `falcone:host/events@0.2.0`
- `falcone:host/logging@0.2.0`
- `falcone:host/cancellation@0.2.0`
- `falcone:host/invocation-context@0.2.0`

`http`, `workflows` and `progress` have no Go binding in this revision and are not allowed. No
WASI interface is allowed. A precompiled Wasmtime artifact is never deserialized, and no
fixture is or contains one (tested).

## Tested examples

| Example | What it shows | Imports |
| --- | --- | --- |
| `json-transform` | A pure JSON transformation with `encoding/json`. | none |
| `text-scan` | Scanning text for order numbers and a date with `strings` only. | none |
| `hash-sha256` | SHA-256 with `crypto/sha256`; the expected output is the FIPS 180-2 vector for `abc`. | none |
| `events-handle` | A real `handler.handle` that reads and updates a rate-limit bucket in storage and publishes an access event. | storage, events |

Inputs and expected outputs are under `workloads/` and `examples/`. `events-handle` runs with
narrowly scoped grants in both the credential and the projection (one topic and one storage key,
no wildcard); its recorded dependency state is replayed before each invocation and the
comparison is exact: storage keys, checkpoint flags, event destinations and payload bytes, in
order, plus the response. Each recorded output is only compared once a built component runs;
until then it is `UNVERIFIED`.

## Supported and unsupported behaviour

Supported for qualification: pure computation and the standard-library packages above, plus the
host interfaces above through the trusted bindings. Anything else is rejected before any build
or instantiation, without executing a guest.

| Code | Meaning |
| --- | --- |
| `WASM-NATIVE-LIBRARY` | cgo (`import "C"`), `plugin`, `runtime/cgo`, `go:cgo` directives, native source files. |
| `WASM-GPU` | A GPU toolkit package. |
| `WASM-PROCESS-SPAWN` | `os/exec`. |
| `WASM-INSTALL-HOOK` | `go:generate`, wherever it appears (see below). |
| `WASM-PACKAGE-NOT-IN-CATALOGUE` | A module import or `go.mod` requirement outside the pinned catalogue. |
| `WASM-UNSUPPORTED-API` | `reflect`, `unsafe`, `go:linkname`, guest `go:wasmimport` and `go:wasmexport`, `//export`, `go:embed`, unknown compiler directives and any standard-library package outside the list. |

TinyGo's limits are profile limits: its `reflect` is partial, so `reflect` and `unsafe` are not
accepted from guests (`encoding/json` is, through TinyGo's own reflect subset), and goroutines
need the scheduler that the build turns off. Every diagnostic names the construct and its file,
line and column, never tenant data. A message may name OCI mode as an alternative, but no
diagnostic selects or changes the execution mode; the mode stays `wasm`.

Every comment token anywhere in a file goes through the directive checks, independent of import
parsing: inside `import (...)`, between imports, at file level and inside a function. `go
generate` also scans raw lines, so a column-one `//go:generate` inside a block comment or raw
string is reported too. A comment that is not a directive (`// go:generate`, prose) is not.

Negative fixtures, each with its documented diagnostic:

| Fixture | Code | Construct |
| --- | --- | --- |
| `negative/native-library` | `WASM-NATIVE-LIBRARY` | `cgo` |
| `negative/gpu` | `WASM-GPU` | `GPU package` |
| `negative/process-spawn` | `WASM-PROCESS-SPAWN` | `os/exec` |
| `negative/install-hook` | `WASM-INSTALL-HOOK` | `go:generate` |
| `negative/package-not-in-catalogue` | `WASM-PACKAGE-NOT-IN-CATALOGUE` | `module requirement`, `package import` |
| `negative/unsupported-reflect` | `WASM-UNSUPPORTED-API` | `reflect` |
| `negative/unsupported-unsafe` | `WASM-UNSUPPORTED-API` | `unsafe` |
| `negative/unsupported-linkname` | `WASM-UNSUPPORTED-API` | `unsafe`, `go:linkname` |
| `negative/unsupported-wasmimport` | `WASM-UNSUPPORTED-API` | `go:wasmimport` |
| `negative/unsupported-stdlib` | `WASM-UNSUPPORTED-API` | `standard library package` |

No negative fixture is ever built or run; the cluster run asserts that no component exists for
one.

## Rerunning qualification

1. `cargo test --workspace --locked` runs the static suite (`profile_go`), with no toolchain.
2. In the trusted compiler image, with the offline toolchain cache: `FS_TRUSTED_COMPILER_IMAGE=1
   FS_TOOLCHAIN_CACHE=... FS_PROFILE_COMPONENTS=<new directory> worker/qualification/go/build-profile.sh`.
3. `profile_go_runtime -- --ignored` (also run by the script) validates, inspects, instantiates
   and runs every component; `cluster_profile_go -- --ignored` repeats the run in a restricted
   pod in a self-created and self-deleted `fs-test-*` namespace, gated by `FS_TEST_CLUSTER`. When
   it is skipped it prints why and the profile stays `UNVERIFIED`.
4. Record the evidence artifact ID and sha256 on #53. The matrix entry stays `disabled` until
   that evidence exists.
