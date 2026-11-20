# Go Wasm profile qualification

Profile `go-wasi-v1`, matrix revision `2`
([matrix](../../contracts/language-profile-matrix.md), WAS-03, FR-WAS-05).

**Status: `disabled`, not qualified.** Nothing in this document enables the profile. It
becomes qualified only when every static and cluster check passes; any failure records it as
`blocked`. Built components and their evidence do not exist yet: every runtime result below
is `UNVERIFIED` until an operator runs the trusted-image build and the cluster suite and
records the artifact ID and sha256 on the story issue (gntik-ai/falcone-serverless#53). The
matrix entry keeps status `disabled` and empty evidence until then.

The manifest is [worker/qualification/profiles/go/manifest.toml](../../../worker/qualification/profiles/go/manifest.toml).
A static test keeps this page, the manifest and both matrix copies consistent.

## Versions

| Component | Version | sha256 of the archive |
| --- | --- | --- |
| TinyGo | `0.44.0` | `ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9` |
| Go toolchain (TinyGo 0.44.0 accepts Go 1.25 through 1.27) | `1.26.11` | `9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec` |
| Target | `wasip2` (TinyGo emits a component directly; no adapter) | |
| Wasmtime | `49.0.5` | |
| WASI | `0.2.0`, only `wasi:random/random@0.2.0` | |
| WIT package | `falcone:host@0.2.0`, world `function` | |
| Go modules | none: the catalogue is unqualified and empty | |

The pins are one file, [worker/src/qualification/go_versions.toml](../../../worker/src/qualification/go_versions.toml).
It lives under `worker/src` because the worker image build copies only `Cargo.toml`,
`Cargo.lock`, `compiler/` and `worker/src` into its context, and non-test worker code may
embed only files under `worker/src`. `worker/qualification/go/build-profile.sh` reads that
file, and a test fails when the manifest or a script carries a differing copy. The build runs
only inside the trusted compiler image, with no network, and a sha256 or version mismatch
fails it closed. Fixtures are committed as sources; prebuilt `.wasm` components come only from
the trusted image and are referenced by sha256.

## Interfaces

The final component may import only these interfaces (a ceiling, not a grant; verified
invocation grants still apply):

- `falcone:host/storage@0.2.0`
- `falcone:host/http@0.2.0`
- `falcone:host/events@0.2.0`
- `falcone:host/workflows@0.2.0`
- `falcone:host/logging@0.2.0`
- `falcone:host/cancellation@0.2.0`
- `falcone:host/progress@0.2.0`
- `falcone:host/invocation-context@0.2.0`
- `wasi:random/random@0.2.0`

`wasi:random/random@0.2.0` is the one WASI interface the host provides (operator decision,
2026-11-20): the pinned TinyGo `wasip2` runtime imports `get-random-u64` from reactor init, and
Go `crypto/rand` reads `get-random-bytes`. The host links exactly those two functions, backed by
the operating system CSPRNG and never by a seeded or deterministic source. `get-random-bytes`
traps for a length above 65536 bytes, every call counts toward the invocation's time, fuel and
host-call limits, and a component that needs any other function of the interface fails to
instantiate. `wasi:random/insecure@0.2.0`, `wasi:random/insecure-seed@0.2.0` and every other
`wasi:*` import (`cli`, `filesystem`, `sockets`, `clocks`, `io`) are rejected with
`undeclared_import`. The host links no other WASI interface. The import gate still requires
each import to be an interface instance.

Every component has exactly one `cabi_realloc` export (TinyGo's own; the bindings define none),
exactly one linear memory (no WASI virtualiser is composed in, so the store-wide `memories(1)`
limit holds unchanged) and imports only what is listed above. A precompiled Wasmtime artifact is
never deserialized: validation answers `precompiled_rejected` (tested for the Go path), and no
fixture is or contains one.

## Standard library

Tenant sources may import only these standard-library packages, each bound to Go `1.26.11`
(TinyGo compiles the standard library that ships with that exact release):

- `bufio`
- `bytes`
- `crypto/hmac`
- `crypto/rand`
- `crypto/sha1`
- `crypto/sha256`
- `crypto/sha512`
- `encoding/base64`
- `encoding/binary`
- `encoding/hex`
- `encoding/json`
- `errors`
- `fmt`
- `io`
- `maps`
- `math`
- `math/bits`
- `reflect`
- `regexp`
- `slices`
- `sort`
- `strconv`
- `strings`
- `unicode`
- `unicode/utf8`

`os/exec` is `WASM-PROCESS-SPAWN`, `C`, `plugin` and `runtime/cgo` are `WASM-NATIVE-LIBRARY`,
and every other standard-library package (`os`, `net`, `net/http`, `syscall`, `unsafe`, `embed`,
`runtime`, `time`) is `WASM-UNSUPPORTED-API`.

## TinyGo limits

These are profile limits, each with the stable diagnostic `WASM-UNSUPPORTED-API`:

- Reflection that constructs types or functions at run time: `reflect.MakeFunc`,
  `reflect.FuncOf`, `reflect.StructOf`, `reflect.ChanOf`, `reflect.MakeChan` and
  `reflect.Select`. Other `reflect` use stays allowed, subject to what TinyGo supports.
- Standard-library packages outside the list above.
- `//go:linkname` anywhere in a source file, `//go:wasmexport`, and `//go:wasmimport` of an
  interface outside the allow-list.

## Tested examples

| Example | What it shows | Imports |
| --- | --- | --- |
| `hash-sha256` | SHA-256 of the request bytes with `crypto/sha256`. | none |
| `byte-stats` | Byte, word and line counts with `strings`, `strconv` and `sort`. | none |
| `random-bytes` | `crypto/rand` through the host's `wasi:random`: two draws differ and are not constant. | `wasi:random` (runtime) |
| `events-handle` | A real `handler.handle` that publishes its request as one event. | events |

The recorded inputs and expected outputs are under `worker/qualification/profiles/go/examples/`.
The handler bindings (`bindings/`) are trusted, hand-written canonical-ABI code with no module
dependency; a tenant tree may not contain files under `bindings/`.

## Diagnostics

Each diagnostic names the construct and its source location (path, line, byte column), never
tenant data. No diagnostic implies a native fallback. OCI is only ever named as an explicit,
separately authorized alternative.

| Code | Go trigger |
| --- | --- |
| `WASM-NATIVE-LIBRARY` | `import "C"` (cgo), a `#cgo` line, `go:cgo_*` directives, `plugin`, `runtime/cgo`, or a native source or binary file (`.c`, `.h`, `.s`, `.so`). |
| `WASM-GPU` | An import path with a GPU, CUDA, OpenCL, Vulkan, WebGPU or NVML segment. |
| `WASM-PROCESS-SPAWN` | `import "os/exec"`. |
| `WASM-INSTALL-HOOK` | `//go:generate` anywhere in a file (file level, between imports or inside a grouped `import (...)`), a `tool` directive in `go.mod`, or a `Makefile` or `Dockerfile`. |
| `WASM-PACKAGE-NOT-IN-CATALOGUE` | A third-party import path, or a `require` or `replace` in `go.mod`. |
| `WASM-UNSUPPORTED-API` | A standard-library package outside the allow-list, a TinyGo reflection limit, `go:linkname`, an escaped import path, or a `toolchain` directive. |

Every comment token in a file runs the directive checks (`go:generate`, `go:linkname`, cgo),
separately from import traversal, so a directive cannot hide in an import group.

## Rerunning qualification

| Check | Command | Runs |
| --- | --- | --- |
| Pins, interfaces, matrix, docs, negative fixtures | `cargo test --workspace --locked` (`profile_go`, `language_profile_matrix`) | Static; runs in CI. |
| Host `wasi:random`, validator allow-list | `cargo test --workspace --locked` (`capability_invocations`, `artifact_validation`) | In-process; runs in CI. |
| Image build context | `cargo test --workspace --locked` (`build_context`) | Builds the worker with only the Containerfile inputs. |
| Build the components | `worker/qualification/go/build-profile.sh` | Operator-run in the trusted compiler image: needs `FS_TRUSTED_COMPILER_IMAGE=1`, `FS_TOOLCHAIN_CACHE` and a new `FS_PROFILE_COMPONENTS`. |
| Execute the components | `cargo test -p falcone-serverless-worker --test profile_go_runtime -- --ignored` | Opt-in with `FS_TOOLCHAIN_CACHE`. |
| Cluster run | `FS_TEST_CLUSTER=1 cargo test -p falcone-serverless-worker --test cluster_profile_go -- --ignored` | Creates and deletes its own `fs-test-*` namespace under the restricted posture; skips with an explicit reason when the gate is unset. |

Without `FS_TOOLCHAIN_CACHE` and `FS_TEST_CLUSTER` the gated checks skip with a reason and the
profile stays unverified. Record the artifact ID and sha256 of the cluster run on the story
issue before any enablement. Enablement is R3: a second reviewer and explicit operator approval.

TinyGo's `wasip2` target needs `wasm-tools` on its `PATH`. The trusted image provides it and the
build script records its version in the evidence. It is not in the operator's pin table, so it is
not pinned here.
