# Go Wasm profile qualification

Profile `go-wasi-v1` is **disabled**, **not-qualified**, and **UNVERIFIED**.
Static checks and a flag probe never qualify it. The operator builds the fixtures
inside the trusted compiler image, then runs the restricted cluster suite and
records an artifact ID and SHA256 on story #53 before any enablement. This is
merge-only and requires an independent R3 reviewer and operator approval.

TinyGo **0.44.0**, Go **1.26.11**, and wasm-tools **1.258.3** are the operator's
pins. `worker/src/qualification/go_versions.toml` is the single build source;
static tests compare it to the manifest and the matrix. The standard library is
bound to Go 1.26.11, including TinyGo's supported subset. There are no third-party
modules in the catalogue. The local trusted bindings belong to the fixture module.

Pure JSON, regular expression and SHA-256 worked examples have input and expected
output files. They export `falcone:host/handler@0.2.0`. Binding input and output use
TinyGo's runtime allocator; bindings export no `cabi_realloc`. The trusted build
counts exactly one such core export and exactly one linear memory per component.
No wasi-virt, preview1 adapter, runtime compilation or tenant precompiled input is used.

The manifest's `wit.interfaces` are the Falcone capability ceiling; verified grants
still apply. Host WASI is separate from matrix `allowedInterfaces`, which stays
Falcone-only. The only WASI instances are `wasi:random/random@0.2.0`,
`wasi:cli/stdout@0.2.0`, `wasi:io/streams@0.2.0`, and `wasi:io/error@0.2.0`.
Randomness comes from the operating system CSPRNG through pinned ring SystemRandom;
`get-random-bytes` traps above 65,536 bytes. Stdout is output-only, capped at 65,536
bytes per invocation. Additional bytes are silently dropped and set a truncation
flag; `check-write` reports no more than remaining capacity. The worker has no
existing stdout byte record: bytes are discarded, with only the accepted byte count
and truncation flag recorded on the invocation's Host (`stdout_summary`). There is
no file, network path or new logging pipeline. Hosts never share this metadata.
The memory count remains `memories(1)`; fuel, host-call and time limits still apply.

The only stdout functions are `get-stdout`, output-stream `write`, `check-write`,
`blocking-write-and-flush`, `flush`, `blocking-flush` and resource drop. The io error
resource is a type only; the host never creates one. Other functions fail linkage.
Stdin, stderr, exit, environment, clocks, polling, filesystem, sockets and input
streams are unavailable. Fixtures that import any of them fail closed.

The static `compat_go::check_tree` seam checks all source files without executing
any guest. It rejects cgo/import C, process APIs (os/exec and syscall), go:generate
anywhere, GPU packages, uncatalogued modules, go:linkname and packages outside the
stdlib subset. Reflection supports only TypeOf, ValueOf and DeepEqual as package
entry points; dynamic construction (including MakeFunc) is unsupported, including
aliased and dot imports. It is a qualification diagnostic, not a Go parser or sandbox.
Each diagnostic names a fixed construct plus file and line; no tenant source is echoed.
No negative fixture is built or executed.

Rerun static checks: `cargo test --workspace --locked`.
Operator build: set `FS_TRUSTED_COMPILER_IMAGE=1`, `FS_TOOLCHAIN_CACHE` to the offline
cache, `FS_PROFILE_COMPONENTS` to a new output directory and `TMPDIR` to scratch,
then run `bash worker/qualification/go/build-profile.sh`. The trusted image installs
the verified archives at `tinygo/`, `go/`, and `bin/wasm-tools` in that cache. It
supplies their extracted binaries; this script neither downloads nor installs tools.
The script checks the archive digests and installed versions, probes its exact TinyGo
options in a scratch module (`module probe`, `go 1.26`), and builds every fixture.
The cached wasm-tools is first on PATH for TinyGo componentization. It validates
and lists imports and writes SHA256SUMS with a relocatable runtime test binary.
Run `cargo test --locked -p falcone-serverless-worker --test profile_go_runtime -- --ignored --nocapture`
for the trusted-image tests. Without FS_TOOLCHAIN_CACHE these report SKIPPED.
Run `cargo test --locked -p falcone-serverless-worker --test cluster_profile_go -- --ignored --nocapture`
with FS_TEST_CLUSTER, FS_TOOLCHAIN_CACHE and FS_TEST_WAS02_IMAGE (a digest-pinned
qualification image). Put the bundle at `/opt/falcone-toolchain-cache/profile-go`
and its `bin/profile-go-runtime` and `bin/artifact-compile-child` in `/usr/local/bin`
in that image. The runtime uses the bundled evidence and needs no checkout.
Without the cluster/cache gates this reports SKIPPED. The shared fs-test harness
creates and deletes only its dedicated namespace by default; shared mode uses
run-labelled objects only. Pods run non-root with RuntimeDefault seccomp and a
read-only root filesystem. Cleanup must pass. No credentials are printed.

## Manifest (checked verbatim for drift)

<!-- manifest:start -->
```toml
# Static eligibility only; operator-run build and cluster evidence remain UNVERIFIED.
modules = [] # No third-party modules are qualified.
[profile]
id = "go-wasi-v1"
status = "disabled"
qualification = "not-qualified"
mode = "wasm"
matrix_revision = "2"
[toolchain]
target = "wasip2"
wasmtime = "49.0.5"
wasi = "0.2.0"
[stdlib]
toolchain = "1.26.11"
packages = ["encoding/json", "encoding/hex", "crypto/sha256", "regexp", "reflect"]
[wit]
package = "falcone:host@0.2.0"
world = "function"
interfaces = ["falcone:host/storage@0.2.0", "falcone:host/http@0.2.0", "falcone:host/events@0.2.0", "falcone:host/workflows@0.2.0", "falcone:host/logging@0.2.0", "falcone:host/cancellation@0.2.0", "falcone:host/progress@0.2.0", "falcone:host/invocation-context@0.2.0"]
[host_wasi]
interfaces = ["wasi:random/random@0.2.0", "wasi:cli/stdout@0.2.0", "wasi:io/streams@0.2.0", "wasi:io/error@0.2.0"]

[pins.tinygo]
name = "TinyGo"
version = "0.44.0"
source = "https://github.com/tinygo-org/tinygo/releases/download/v0.44.0/tinygo0.44.0.linux-amd64.tar.gz"
archive = "tinygo0.44.0.linux-amd64.tar.gz"
sha256 = "ed0f45543a49ff2a99120a5a1b8814a0750e1fda1a66f8e2a0a172576134aaf9"

[pins.go]
name = "Go"
version = "1.26.11"
source = "https://go.dev/dl/go1.26.11.linux-amd64.tar.gz"
archive = "go1.26.11.linux-amd64.tar.gz"
sha256 = "9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec"

[pins.wasm_tools]
name = "wasm-tools"
version = "1.258.3"
source = "https://github.com/bytecodealliance/wasm-tools/releases/download/v1.258.3/wasm-tools-1.258.3-x86_64-linux.tar.gz"
archive = "wasm-tools-1.258.3-x86_64-linux.tar.gz"
sha256 = "99b486a9ddc19af0260cd5f98f3b709a60817a3acd20efc9c582d719caebbf6d"

[[workloads]]
name = "json-transform"
source = "workloads/json-transform/main.go"
input = "workloads/json-transform/input.json"
expected = "workloads/json-transform/expected.json"
imports = []

[[workloads]]
name = "text-regex"
source = "workloads/text-regex/main.go"
input = "workloads/text-regex/input.json"
expected = "workloads/text-regex/expected.json"
imports = []

[[workloads]]
name = "hash-sha256"
source = "workloads/hash-sha256/main.go"
input = "workloads/hash-sha256/input.json"
expected = "workloads/hash-sha256/expected.json"
imports = []

[[negative]]
file = "negative/native-library/main.go"
code = "WASM-NATIVE-LIBRARY"
line = 3

[diagnostics."WASM-NATIVE-LIBRARY"]
message = "native-library is outside the Go Wasm profile."

[[negative]]
file = "negative/process-spawn/main.go"
code = "WASM-PROCESS-SPAWN"
line = 3

[diagnostics."WASM-PROCESS-SPAWN"]
message = "process-spawn is outside the Go Wasm profile."

[[negative]]
file = "negative/install-hook/main.go"
code = "WASM-INSTALL-HOOK"
line = 3

[diagnostics."WASM-INSTALL-HOOK"]
message = "install-hook is outside the Go Wasm profile."

[[negative]]
file = "negative/gpu/main.go"
code = "WASM-GPU"
line = 3

[diagnostics."WASM-GPU"]
message = "gpu is outside the Go Wasm profile."

[[negative]]
file = "negative/package-not-in-catalogue/main.go"
code = "WASM-PACKAGE-NOT-IN-CATALOGUE"
line = 3

[diagnostics."WASM-PACKAGE-NOT-IN-CATALOGUE"]
message = "package-not-in-catalogue is outside the Go Wasm profile."

[[negative]]
file = "negative/unsupported-api/main.go"
code = "WASM-UNSUPPORTED-API"
line = 5

[diagnostics."WASM-UNSUPPORTED-API"]
message = "unsupported-api is outside the Go Wasm profile."
```
<!-- manifest:end -->
