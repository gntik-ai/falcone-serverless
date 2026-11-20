# Go Wasm profile

UNVERIFIED: source review does not qualify this profile. It remains disabled and not-qualified until the operator records successful restricted-cluster evidence with an artifact ID and sha256 on #53. No runtime adapter or admission path uses the static checker.

## profile

| Field | Value |
| --- | --- |
| id | `"go-wasi-v1"` |
| matrix_revision | `"2"` |
| mode | `"wasm"` |
| qualification | `"not-qualified"` |
| status | `"disabled"` |

## toolchain

| Field | Value |
| --- | --- |
| go | `"1.26.11"` |
| go_sha256 | `"9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec"` |
| target | `"wasip2"` |
| tinygo | `"0.44.0"` |
| tinygo_sha256 | `"ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9"` |
| wasi | `"none"` |
| wasi_virt_rev | `"bca61c27499d2c290b5431cfcff013737b3ba01d"` |
| wasmtime | `"49.0.5"` |

## wit

| Field | Value |
| --- | --- |
| interfaces | `["falcone:host/storage@0.2.0","falcone:host/http@0.2.0","falcone:host/events@0.2.0","falcone:host/workflows@0.2.0","falcone:host/logging@0.2.0","falcone:host/cancellation@0.2.0","falcone:host/progress@0.2.0","falcone:host/invocation-context@0.2.0"]` |
| package | `"falcone:host@0.2.0"` |
| world | `"function"` |

## catalogue

| Field | Value |
| --- | --- |
| packages | `[]` |
| revision | `"unqualified"` |

## stdlib

| Field | Value |
| --- | --- |
| go | `"1.26.11"` |
| packages | `["crypto/sha256","encoding/hex","encoding/json","unsafe"]` |
| tinygo | `"0.44.0"` |

## limits

| Field | Value |
| --- | --- |
| abi_arena_bytes | `8388608` |
| host_call_timeout_ms | `30000` |
| max_guest_to_host_bytes | `1048576` |
| max_host_to_guest_bytes | `1048576` |
| unsupported | `["cgo","processes and goroutines","install hooks","GPU packages","third-party modules","direct reflection","ambient filesystem, network and environment","dynamic loading"]` |

## diagnostics

| Field | Value |
| --- | --- |
| WASM-GPU | `{"message":"GPU packages are outside the capability host."}` |
| WASM-INSTALL-HOOK | `{"message":"Generation, install and build hooks are never run."}` |
| WASM-NATIVE-LIBRARY | `{"message":"cgo and native libraries are unavailable in Wasm mode."}` |
| WASM-PACKAGE-NOT-IN-CATALOGUE | `{"message":"No third-party module is in the empty unqualified catalogue."}` |
| WASM-PROCESS-SPAWN | `{"message":"Processes and goroutines are unavailable in this profile."}` |
| WASM-UNSUPPORTED-API | `{"message":"The API, directive or interface is outside the supported subset."}` |

## Operator archive pins

### tinygo

- archive: `tinygo0.44.0.linux-amd64.tar.gz`
- name: `TinyGo`
- sha256: `ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9`
- source: `https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz`
- version: `0.44.0`

### go

- archive: `go1.26.11.linux-amd64.tar.gz`
- name: `Go`
- sha256: `9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec`
- source: `https://go.dev/dl/go1.26.11.linux-amd64.tar.gz`
- version: `1.26.11`

### wasi_virt

- name: `wasi-virt`
- rev: `bca61c27499d2c290b5431cfcff013737b3ba01d`
- source: `https://github.com/bytecodealliance/WASI-Virt`
- version: `0.2.0`

## Examples

- `json-transform`: `examples/json-transform/input.json`, expected `examples/json-transform/expected.json`; imports `[]`.
- `hash-sha256`: `examples/hash-sha256/input.json`, expected `examples/hash-sha256/expected.json`; imports `[]`.
- `storage-events`: `examples/storage-events/input.json`, expected `examples/storage-events/expected.json`; imports `["falcone:host/storage@0.2.0","falcone:host/events@0.2.0"]`.

## Rejections and limits

All examples use go.mod with no require lines. Every third-party module is outside the empty catalogue, including an exactly versioned module. The standard library is bound to both recorded toolchains; only the listed direct imports are supported. Direct reflect, net, os, plugin, syscall and other unlisted standard-library imports are WASM-UNSUPPORTED-API. GPU path segments gpu, cuda, opencl and vulkan (case insensitive) map to WASM-GPU; these markers define the checker policy, not a qualified GPU package list. The synthetic example.invalid/gpu fixture never resolves a module. Processes and goroutines are WASM-PROCESS-SPAWN; go:generate is WASM-INSTALL-HOOK. Unknown compiler directives and malformed source are refused. Diagnostics identify the source file and line with a fixed construct label and never include source contents, commands, parameters or dependency values. Diagnostics cannot change execution mode.

Bindings are project-authored Go source using wasmimport/wasmexport and the standard library. unsafe is confined by convention to this reviewed ABI glue, not a promise that arbitrary unsafe Go is supported. Canonical allocations use a bounded per-invocation arena, retained until post-return; storage handles are dropped after reads. TinyGo JSON and cryptography support, ABI exports, allocation callbacks and WASI removal still require the pinned build and cluster replay. The static checker does not prove Go type correctness or runtime safety. The worker import gate remains mandatory. No fixture is a native binary or serialized/precompiled artifact; negatives are source-only and are never built or instantiated.

## Rerun qualification

Default: `cargo test --workspace --locked` requires no guest toolchain cache.

Operator build: in the trusted compiler image with networking disabled, set FS_TRUSTED_COMPILER_IMAGE=1, FS_TOOLCHAIN_CACHE to the read-only offline cache, FS_GO_COMPONENTS to a new output directory, and TMPDIR to scratch, then run `bash worker/qualification/go/build-fixtures.sh`. The cache contains the exact archives above, installed go/ and tinygo/ trees, bin/wasi-virt, and wasi-virt.rev. Archive hashes and the revision receipt are checked before use; no download, install or toolchain build occurs. Only the three positive examples are built with the wasip2 target and function world, then wasi-virt deny-all runs. Every component must pass validate.rs, with exactly its declared imports and the handler export, before any component executes. Any mismatch stops publication. ABI compatibility and wasi-virt effectiveness are UNVERIFIED until this succeeds.

Package the built Go components under /opt/falcone-toolchain-cache/go-profile, its bin/profile-go-runtime as /usr/local/bin/profile-go-runtime and bin/artifact-compile-child beside it in the operator's digest-pinned qualification image. No guest toolchain executes in the cluster pod.

Operator cluster: set FS_TEST_CLUSTER=1, FS_TOOLCHAIN_CACHE, KUBECONFIG and digest-pinned FS_TEST_WAS02_IMAGE, then run `cargo test --locked -p falcone-serverless-worker --test cluster_profile_go -- --ignored --exact cluster_profile_go --nocapture`. Missing FS_TEST_CLUSTER or FS_TOOLCHAIN_CACHE prints an explicit UNVERIFIED reason without a client call. Default FS_TEST_NAMESPACE_MODE=create creates and deletes its own fs-test-* namespace and observes deletion. If creation is denied it fails before mutation and names the switch. FS_TEST_NAMESPACE_MODE=shared uses FS_TEST_NAMESPACE and touches only new run-labelled objects; it never creates or deletes a namespace. Pods use an arbitrary non-root UID, RuntimeDefault seccomp, no privilege escalation, dropped capabilities and a read-only root filesystem.

The runner validates prebuilt components, compares the two pure outputs and replays the storage-events cases (stored payload and denied read), checking exact keys, destinations and payload bytes. It never prints credentials. Archive pinning or matching local expectations alone supplies no cluster evidence. Publish the component bundle and cluster report by artifact ID plus sha256 on #53; until then manifest qualification stays not-qualified, matrix status stays disabled and evidence stays empty. Independent R3 second review and operator approval are required before merge.
