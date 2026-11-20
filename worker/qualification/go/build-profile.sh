#!/usr/bin/env bash
# Operator-run only, inside the project's offline, digest-pinned trusted compiler image.
# Builds the Go profile's positive components (hash-sha256, byte-stats, random-bytes and the
# events-handle example) with the pinned TinyGo wasip2 target. This script neither fetches nor
# builds a guest toolchain and never enables a profile. No WASI virtualiser is composed in: the
# Go components keep exactly one linear memory and import only what the worker import gate allows.
# Every version and sha256 is read from worker/src/qualification/go_versions.toml.
set -euo pipefail
[[ ${FS_TRUSTED_COMPILER_IMAGE:-} == 1 ]] || { echo 'requires the trusted compiler image' >&2; exit 1; }
: "${FS_TOOLCHAIN_CACHE:?offline toolchain cache required}"
: "${FS_PROFILE_COMPONENTS:?new output directory required}"
: "${TMPDIR:?run scratch directory required}"
root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$TMPDIR/profile-cargo}"
pins=worker/src/qualification/go_versions.toml
profile=worker/qualification/profiles/go
tinygo="$FS_TOOLCHAIN_CACHE/tinygo/bin/tinygo"
go="$FS_TOOLCHAIN_CACHE/go/bin/go"
wasm_tools="$FS_TOOLCHAIN_CACHE/bin/wasm-tools"

# Verify the operator's exact archives and the installed tool versions before running them.
# A sha256 or version mismatch fails closed. No download, install or extraction happens here.
python3 - "$FS_TOOLCHAIN_CACHE" "$pins" "$tinygo" "$go" <<'PY'
import hashlib, pathlib, subprocess, sys, tomllib
cache = pathlib.Path(sys.argv[1])
pins = tomllib.loads(pathlib.Path(sys.argv[2]).read_text())
for key in ('tinygo', 'go'):
    pin = pins[key]
    digest = hashlib.file_digest((cache / pin['archive']).open('rb'), 'sha256').hexdigest()
    if digest != pin['sha256']:
        raise SystemExit('toolchain archive sha256 mismatch: ' + key)
tinygo = subprocess.run([sys.argv[3], 'version'], capture_output=True, text=True, check=True).stdout
if not tinygo.startswith('tinygo version ' + pins['tinygo']['version'] + ' '):
    raise SystemExit('TinyGo version mismatch')
if 'go version go' + pins['go']['version'] + ' ' not in tinygo:
    raise SystemExit('TinyGo was not built with the pinned Go release')
go = subprocess.run([sys.argv[4], 'version'], capture_output=True, text=True, check=True).stdout
if not go.startswith('go version go' + pins['go']['version'] + ' '):
    raise SystemExit('Go version mismatch')
PY
[[ ! -e "$FS_PROFILE_COMPONENTS" ]] || { echo 'output directory already exists' >&2; exit 1; }
stage=$(mktemp -d "$TMPDIR/profile-go.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/components" "$stage/components/evidence"

# The pod has no checkout: the static evidence is packaged with the components.
for path in manifest.toml negative; do
    cp -r "$profile/$path" "$stage/components/evidence/$path"
done
"$tinygo" version > "$stage/components/evidence/tinygo.version"
"$wasm_tools" --version > "$stage/components/evidence/wasm-tools.version"

# Static checks precede the build: pins, interfaces, negative fixtures. A failure stops here.
cargo test --offline --locked -p falcone-serverless-worker --test profile_go

# TinyGo's wasip2 target composes the component itself from the build-only world, which adds
# wasi:random/random to the function world. The bindings carry no cabi_realloc: TinyGo's runtime
# exports the one cabi_realloc the component has.
mkdir -p "$stage/wit/deps/host" "$stage/wit/deps/random" "$stage/module"
cp worker/wit/host.wit "$stage/wit/deps/host/host.wit"
cp worker/qualification/go/wit/random.wit "$stage/wit/deps/random/random.wit"
cp worker/qualification/go/wit/world.wit "$stage/wit/world.wit"
cp -r "$profile/go.mod" "$profile/bindings" "$profile/examples" "$stage/module/"
export GOTOOLCHAIN=local GOPROXY=off GOFLAGS=-mod=readonly GOCACHE="$stage/gocache" GOPATH="$stage/gopath"
export PATH="$FS_TOOLCHAIN_CACHE/bin:$PATH"
for name in hash-sha256 byte-stats random-bytes events-handle; do
    (cd "$stage/module" && "$tinygo" build -target=wasip2 -no-debug -wit-package "$stage/wit" -wit-world falcone:go-build/function@0.2.0 -o "$stage/components/$name.wasm" "./examples/$name")
done

# The worker validator rejects every remaining non-allowed import before any compile or
# instantiation, and the runner checks one cabi_realloc and one memory per component. Only a
# fully passing set is copied to the output.
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
