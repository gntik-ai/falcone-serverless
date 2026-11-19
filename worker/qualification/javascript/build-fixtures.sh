#!/usr/bin/env bash
# Operator-run only, inside the project's offline, digest-pinned trusted compiler image.
# This script neither fetches nor builds a guest toolchain and never enables a profile.
set -euo pipefail
[[ ${FS_TRUSTED_COMPILER_IMAGE:-} == 1 ]] || { echo 'requires the trusted compiler image' >&2; exit 1; }
: "${FS_TOOLCHAIN_CACHE:?offline toolchain cache required}"
: "${FS_LEGACY_COMPONENTS:?new output directory required}"
: "${TMPDIR:?run scratch directory required}"
root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$TMPDIR/legacy-cargo}"
mode=${1:-legacy}
[[ "$mode" == legacy || "$mode" == --profile ]] || { echo 'expected legacy or --profile' >&2; exit 1; }
if [[ "$mode" == --profile ]]; then
    # The reference currently fails this source-only gate. No package installation,
    # bundling shim or grammar relaxation is permitted to make it build.
    cargo test --offline --locked -p falcone-serverless-worker --test profile_javascript -- --ignored --exact reference_is_build_eligible --nocapture
fi

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
[[ ! -e "$FS_LEGACY_COMPONENTS" ]] || { echo 'output directory already exists' >&2; exit 1; }
stage=$(mktemp -d "$TMPDIR/legacy-js.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/components"

if [[ "$mode" == --profile ]]; then
    "$node" "$jco" componentize worker/qualification/javascript/reference/actions/openapi-spec-serve.mjs --wit worker/wit/host.wit --world-name function --disable all -o "$stage/reference.raw.wasm"
    "$virt" --deny-all "$stage/reference.raw.wasm" -o "$stage/components/reference.wasm"
    FS_JS_PROFILE_COMPONENT="$stage/components/reference.wasm" cargo test --offline --locked -p falcone-serverless-worker --test profile_javascript -- --ignored --exact built_reference_imports --nocapture
    mkdir -p "$FS_LEGACY_COMPONENTS/bin"
    cp "$stage/components/reference.wasm" "$FS_LEGACY_COMPONENTS/"
    cargo test --offline --locked -p falcone-serverless-worker --test cluster_profile_javascript --no-run --message-format=json > "$stage/build.jsonl"
    python3 - "$stage/build.jsonl" "$FS_LEGACY_COMPONENTS/bin/profile-javascript" <<'PY'
import json, pathlib, shutil, sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
tests = [a['executable'] for a in artifacts if a.get('target', {}).get('name') == 'cluster_profile_javascript' and a.get('executable')]
if len(tests) != 1:
    raise SystemExit('missing unique cluster_profile_javascript executable')
shutil.copy2(tests[0], sys.argv[2])
PY
    cp "$CARGO_TARGET_DIR/debug/artifact-compile-child" "$FS_LEGACY_COMPONENTS/bin/"
    (cd "$FS_LEGACY_COMPONENTS" && sha256sum reference.wasm bin/* > SHA256SUMS)
    exit 0
fi

# Static checks precede componentization. Every source is synthetic; none is tenant input.
cargo test --offline --locked -p falcone-serverless-worker --test compat_static
for name in success throw reject logs identity denied guessed; do
    cat worker/src/adapters/javascript/wrapper.mjs "worker/tests/fixtures/legacy/$name.js" > "$stage/$name.mjs"
    "$node" "$jco" componentize "$stage/$name.mjs" --wit worker/wit/host.wit --world-name function --disable all -o "$stage/$name.raw.wasm"
    "$virt" --deny-all "$stage/$name.raw.wasm" -o "$stage/components/$name.wasm"
done

# The worker validator rejects all remaining non-Falcone imports (including WASI), before
# compiling or instantiating a component. Only a fully passing set is copied to the output.
FS_LEGACY_COMPONENTS="$stage/components" cargo test --offline --locked -p falcone-serverless-worker --test legacy_runtime -- --ignored --exact built_legacy_components --nocapture
mkdir -p "$FS_LEGACY_COMPONENTS/bin"
cp "$stage/components/"*.wasm "$FS_LEGACY_COMPONENTS/"
cargo test --offline --locked -p falcone-serverless-worker --test legacy_runtime --no-run --message-format=json > "$stage/build.jsonl"
python3 - "$stage/build.jsonl" "$FS_LEGACY_COMPONENTS/bin/legacy-runtime" <<'PY'
import json, pathlib, shutil, sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
tests = [a['executable'] for a in artifacts if a.get('target', {}).get('name') == 'legacy_runtime' and a.get('executable')]
if len(tests) != 1:
    raise SystemExit('missing unique legacy_runtime test executable')
shutil.copy2(tests[0], sys.argv[2])
PY
cp "$CARGO_TARGET_DIR/debug/artifact-compile-child" "$FS_LEGACY_COMPONENTS/bin/"
(cd "$FS_LEGACY_COMPONENTS" && sha256sum ./*.wasm bin/* > SHA256SUMS)
