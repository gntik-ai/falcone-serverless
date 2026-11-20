#!/usr/bin/env bash
# Operator-run only, inside the project's offline, digest-pinned trusted compiler image.
# Builds the JavaScript profile's positive components (json-transform, text-regex, hash-sha256
# and the events-handle example). This script neither fetches nor builds a guest toolchain and
# never enables a profile. The legacy reference function is never built: it is a recorded
# BLOCKED migration result.
set -euo pipefail
[[ ${FS_TRUSTED_COMPILER_IMAGE:-} == 1 ]] || { echo 'requires the trusted compiler image' >&2; exit 1; }
: "${FS_TOOLCHAIN_CACHE:?offline toolchain cache required}"
: "${FS_PROFILE_COMPONENTS:?new output directory required}"
: "${TMPDIR:?run scratch directory required}"
root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$TMPDIR/profile-cargo}"
profile=worker/qualification/profiles/javascript

# Verify the operator's exact archives, installed package versions and trusted-image git
# receipt before executing the preinstalled tools. No npm, git, cargo install or extraction.
python3 - "$FS_TOOLCHAIN_CACHE" worker/qualification/javascript/versions.toml <<'PY'
import hashlib, json, pathlib, sys, tomllib
cache = pathlib.Path(sys.argv[1])
pins = tomllib.loads(pathlib.Path(sys.argv[2]).read_text())
for key in ('jco', 'componentize_js', 'node'):
    pin = pins[key]
    digest = hashlib.file_digest((cache / pin['archive']).open('rb'), 'sha256').hexdigest()
    if digest != pin['sha256']:
        raise SystemExit('toolchain archive sha256 mismatch: ' + key)
for key in ('jco', 'componentize_js'):
    pin = pins[key]
    package = json.loads((cache / 'node_modules' / pin['name'] / 'package.json').read_text())
    if package['name'] != pin['name'] or package['version'] != pin['version']:
        raise SystemExit('preinstalled tool version mismatch: ' + key)
if (cache / 'wasi-virt.rev').read_text().strip() != pins['wasi_virt']['rev']:
    raise SystemExit('wasi-virt trusted-image revision mismatch')
PY
node="$FS_TOOLCHAIN_CACHE/bin/node"
jco="$FS_TOOLCHAIN_CACHE/node_modules/@bytecodealliance/jco/lib/jco.js"
virt="$FS_TOOLCHAIN_CACHE/bin/wasi-virt"
[[ $("$node" --version) == v26.13.0 ]] || { echo 'Node version mismatch' >&2; exit 1; }
[[ ! -e "$FS_PROFILE_COMPONENTS" ]] || { echo 'output directory already exists' >&2; exit 1; }
stage=$(mktemp -d "$TMPDIR/profile-js.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/components"

# Static checks precede componentization: the matrix, the manifest, the negative fixtures and
# the reference tree's BLOCKED result. A failure stops the build before any component exists.
cargo test --offline --locked -p falcone-serverless-worker --test language_profile_matrix --test compat_tree --test profile_javascript

# Every handler already exports handler.handle; there is no wrapper and nothing is injected.
for name in json-transform text-regex hash-sha256; do
    "$node" "$jco" componentize "$profile/workloads/$name/handler.mjs" --wit worker/wit/host.wit --world-name function --disable all -o "$stage/$name.raw.wasm"
    "$virt" --deny-all "$stage/$name.raw.wasm" -o "$stage/components/$name.wasm"
done
"$node" "$jco" componentize "$profile/examples/events-handle/handler.mjs" --wit worker/wit/host.wit --world-name function --disable all -o "$stage/events-handle.raw.wasm"
"$virt" --deny-all "$stage/events-handle.raw.wasm" -o "$stage/components/events-handle.wasm"

# The worker validator rejects every remaining non-Falcone import (WASI included) before any
# compile or instantiation. Only a fully passing set is copied to the output.
FS_PROFILE_COMPONENTS="$stage/components" cargo test --offline --locked -p falcone-serverless-worker --test profile_javascript_runtime -- --ignored --exact built_profile_components --nocapture
mkdir -p "$FS_PROFILE_COMPONENTS/bin"
cp "$stage/components/"*.wasm "$FS_PROFILE_COMPONENTS/"
cargo test --offline --locked -p falcone-serverless-worker --test profile_javascript_runtime --no-run --message-format=json > "$stage/build.jsonl"
python3 - "$stage/build.jsonl" "$FS_PROFILE_COMPONENTS/bin/profile-javascript-runtime" <<'PY'
import json, pathlib, shutil, sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
tests = [a['executable'] for a in artifacts if a.get('target', {}).get('name') == 'profile_javascript_runtime' and a.get('executable')]
if len(tests) != 1:
    raise SystemExit('missing unique profile_javascript_runtime test executable')
shutil.copy2(tests[0], sys.argv[2])
PY
cp "$CARGO_TARGET_DIR/debug/artifact-compile-child" "$FS_PROFILE_COMPONENTS/bin/"
(cd "$FS_PROFILE_COMPONENTS" && sha256sum ./*.wasm bin/* > SHA256SUMS)
