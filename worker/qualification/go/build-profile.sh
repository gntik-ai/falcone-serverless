#!/usr/bin/env bash
# Operator-run only, inside the project's offline, digest-pinned trusted compiler image.
# Builds the Go profile's positive components (json-transform, text-scan, hash-sha256 and the
# events-handle example) with TinyGo's wasip2 target. This script neither fetches nor builds a
# guest toolchain and never enables a profile.
#
# There is no WASI virtualization step and no second component is composed: a composed adapter
# brings its own linear memory and Host::new allows exactly one. WASI is kept out through the
# build configuration (the falcone:host world, -scheduler=none, -panic=trap, -gc=leaking), and
# the worker validator plus the runtime test below reject any import or memory that remains.
set -euo pipefail
[[ ${FS_TRUSTED_COMPILER_IMAGE:-} == 1 ]] || { echo 'requires the trusted compiler image' >&2; exit 1; }
: "${FS_TOOLCHAIN_CACHE:?offline toolchain cache required}"
: "${FS_PROFILE_COMPONENTS:?new output directory required}"
: "${TMPDIR:?run scratch directory required}"
root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$TMPDIR/profile-go-cargo}"
profile=worker/qualification/profiles/go

# Verify the operator's exact archives and the installed tool versions before executing the
# preinstalled tools. No download, no extraction, no module fetch.
python3 - "$FS_TOOLCHAIN_CACHE" worker/qualification/go/versions.toml <<'PY'
import hashlib, pathlib, subprocess, sys, tomllib
cache = pathlib.Path(sys.argv[1])
pins = tomllib.loads(pathlib.Path(sys.argv[2]).read_text())
for key in ('tinygo', 'go'):
    pin = pins[key]
    digest = hashlib.file_digest((cache / pin['archive']).open('rb'), 'sha256').hexdigest()
    if digest != pin['sha256']:
        raise SystemExit('toolchain archive sha256 mismatch: ' + key)
def version(*command):
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout
if 'tinygo version ' + pins['tinygo']['version'] + ' ' not in version(str(cache / 'tinygo/bin/tinygo'), 'version'):
    raise SystemExit('installed TinyGo version mismatch')
if 'go version go' + pins['go']['version'] + ' ' not in version(str(cache / 'go/bin/go'), 'version'):
    raise SystemExit('installed Go version mismatch')
PY
tinygo="$FS_TOOLCHAIN_CACHE/tinygo/bin/tinygo"
export GOROOT="$FS_TOOLCHAIN_CACHE/go"
export PATH="$FS_TOOLCHAIN_CACHE/tinygo/bin:$GOROOT/bin:$FS_TOOLCHAIN_CACHE/bin:$PATH"
export GOTOOLCHAIN=local GOPROXY=off GOFLAGS=-mod=readonly GOWORK=off CGO_ENABLED=0
[[ ! -e "$FS_PROFILE_COMPONENTS" ]] || { echo 'output directory already exists' >&2; exit 1; }
stage=$(mktemp -d "$TMPDIR/profile-go.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
export GOCACHE="$stage/gocache"
mkdir "$stage/components" "$stage/components/evidence"

# The pod has no checkout: the static evidence (EVIDENCE_PATHS in tests/support/profile_go.rs)
# is packaged with the components and read from there at run time.
for path in manifest.toml negative; do
    cp -r "$profile/$path" "$stage/components/evidence/$path"
done
{ "$tinygo" version; go version; wasm-tools --version; } > "$stage/components/evidence/toolchain.txt"

# Static checks precede componentization: the matrix, the manifest, the bindings and the
# negative fixtures. A failure stops the build before any component exists.
cargo test --offline --locked -p falcone-serverless-worker --test language_profile_matrix --test profile_go

build() {
    (cd "$profile" && "$tinygo" build -target=wasip2 -wit-package "$root/worker/wit" -wit-world function \
        -gc=leaking -scheduler=none -panic=trap -no-debug -o "$stage/components/$1.wasm" "$2")
}
build json-transform ./workloads/json-transform
build text-scan ./workloads/text-scan
build hash-sha256 ./workloads/hash-sha256
build events-handle ./examples/events-handle

# The worker validator rejects every remaining non-Falcone import (WASI included) before any
# compile or instantiation, and the runtime test asserts one cabi_realloc and one linear
# memory per component, then runs each under the unchanged Host::new limits. Only a fully
# passing set is copied to the output.
FS_PROFILE_COMPONENTS="$stage/components" cargo test --offline --locked -p falcone-serverless-worker --test profile_go_runtime -- --ignored --exact built_profile_components --nocapture
mkdir -p "$FS_PROFILE_COMPONENTS/bin"
cp "$stage/components/"*.wasm "$FS_PROFILE_COMPONENTS/"
cp -r "$stage/components/evidence" "$FS_PROFILE_COMPONENTS/evidence"
cargo test --offline --locked -p falcone-serverless-worker --test profile_go_runtime --no-run --message-format=json > "$stage/build.jsonl"
python3 - "$stage/build.jsonl" "$FS_PROFILE_COMPONENTS/bin/profile-go-runtime" <<'PY'
import json, pathlib, shutil, sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
tests = [a['executable'] for a in artifacts if a.get('target', {}).get('name') == 'profile_go_runtime' and a.get('executable')]
if len(tests) != 1:
    raise SystemExit('missing unique profile_go_runtime test executable')
shutil.copy2(tests[0], sys.argv[2])
PY
cp "$CARGO_TARGET_DIR/debug/artifact-compile-child" "$FS_PROFILE_COMPONENTS/bin/"
(cd "$FS_PROFILE_COMPONENTS" && sha256sum ./*.wasm bin/* $(find evidence -type f | sort) > SHA256SUMS)
