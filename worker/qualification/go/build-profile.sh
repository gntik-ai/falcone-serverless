#!/usr/bin/env bash
# Operator-run only in the offline, trusted compiler image. Never enables a profile.
set -euo pipefail
[[ ${FS_TRUSTED_COMPILER_IMAGE:-} == 1 ]] || { echo 'requires the trusted compiler image' >&2; exit 1; }
: "${FS_TOOLCHAIN_CACHE:?offline toolchain cache required}"
: "${FS_PROFILE_COMPONENTS:?new output directory required}"
: "${TMPDIR:?run scratch directory required}"
root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root"
pins=worker/src/qualification/go_versions.toml
profile="$root/worker/qualification/profiles/go"
[[ ! -e "$FS_PROFILE_COMPONENTS" ]] || { echo 'output directory already exists' >&2; exit 1; }
stage=$(mktemp -d "$TMPDIR/profile-go.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
python3 - "$FS_TOOLCHAIN_CACHE" "$pins" "$stage/tools" <<'PY'
import hashlib, pathlib, shutil, sys, tarfile, tomllib
cache = pathlib.Path(sys.argv[1])
pins = tomllib.loads(pathlib.Path(sys.argv[2]).read_text())
tools = pathlib.Path(sys.argv[3])
for key in ('tinygo', 'go', 'wasm_tools'):
    pin = pins[key]
    with (cache / pin['archive']).open('rb') as archive:
        digest = hashlib.file_digest(archive, 'sha256').hexdigest()
        if digest != pin['sha256']:
            raise SystemExit('toolchain archive sha256 mismatch: ' + key)
        # Extract the very same verified file, never a separately installed tree.
        archive.seek(0)
        with tarfile.open(fileobj=archive, mode='r:gz') as contents:
            contents.extractall(tools / key, filter='data')
componentizers = [p for p in (tools / 'wasm_tools').rglob('wasm-tools') if p.is_file()]
if len(componentizers) != 1:
    raise SystemExit('missing unique wasm-tools executable in verified archive')
(tools / 'bin').mkdir()
shutil.copy2(componentizers[0], tools / 'bin/wasm-tools')
PY
# Every executed tool comes from a verified archive in this invocation's scratch.
# PATH puts that componentizer ahead of any host wasm-tools used by TinyGo.
export GOROOT="$stage/tools/go/go"
export TINYGOROOT="$stage/tools/tinygo/tinygo"
export PATH="$stage/tools/bin:$GOROOT/bin:$TINYGOROOT/bin:$PATH"
export GOTOOLCHAIN=local GOPROXY=off GOSUMDB=off
[[ $("$stage/tools/bin/wasm-tools" --version) == 'wasm-tools 1.258.3' ]] || { echo 'wasm-tools version mismatch' >&2; exit 1; }
[[ $("$GOROOT/bin/go" version) == 'go version go1.26.11 linux/amd64' ]] || { echo 'Go version mismatch' >&2; exit 1; }
[[ $("$TINYGOROOT/bin/tinygo" version) == 'tinygo version 0.44.0 '* ]] || { echo 'TinyGo version mismatch' >&2; exit 1; }
export GOCACHE="$stage/go-cache" GOPATH="$stage/gopath"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$TMPDIR/profile-cargo}"
tinygo="$TINYGOROOT/bin/tinygo"
wasm_tools="$stage/tools/bin/wasm-tools"
cp -r "$profile/wit" "$stage/wit"
mkdir "$stage/wit/deps/falcone-host"
cp "$root/worker/wit/host.wit" "$stage/wit/deps/falcone-host/host.wit"
options=(-target=wasip2 -buildmode=c-shared -wit-package "$stage/wit" -wit-world function)

# A missing package is deliberately the operand, after option parsing. The loader
# must run inside a module, or the probe says nothing about these options (#433).
mkdir "$stage/probe"
printf 'module probe\n\ngo 1.26\n' > "$stage/probe/go.mod"
if (cd "$stage/probe" && "$tinygo" build "${options[@]}" -o probe.wasm ./missing-probe-package) > "$stage/probe.log" 2>&1; then
    echo 'option probe unexpectedly built a missing package' >&2; exit 1
fi
python3 - "$stage/probe.log" <<'PY'
import pathlib, sys
text = pathlib.Path(sys.argv[1]).read_text()
if any(message in text for message in ('unknown flag', 'unknown argument', 'flag provided but not defined', 'go.mod file not found')) or 'missing-probe-package' not in text:
    raise SystemExit('TinyGo option probe failed before reaching the missing-package operand')
PY
[[ ${FS_OPTION_PROBE_ONLY:-} != 1 ]] || exit 0

# No guest is executed by the static profile test. Negative sources are never built.
cargo test --offline --locked -p falcone-serverless-worker --test profile_go --test language_profile_matrix
mkdir "$stage/components"
cp -r "$profile" "$stage/components/evidence"
for name in json-transform text-regex hash-sha256; do
    (cd "$profile" && "$tinygo" build "${options[@]}" -o "$stage/components/$name.wasm" "./workloads/$name")
    "$wasm_tools" validate "$stage/components/$name.wasm"
    "$wasm_tools" component wit "$stage/components/$name.wasm" > "$stage/components/$name.wit"
done
# Validates the actual imports, memory count, single realloc and expected outputs.
# It prints each component's imports. A mismatch stops publication of the bundle.
FS_PROFILE_COMPONENTS="$stage/components" cargo test --offline --locked -p falcone-serverless-worker --test profile_go_runtime -- --ignored --exact built_profile_components --nocapture
cargo test --offline --locked -p falcone-serverless-worker --test profile_go_runtime --no-run --message-format=json > "$stage/build.jsonl"
mkdir "$stage/components/bin"
python3 - "$stage/build.jsonl" "$stage/components/bin/profile-go-runtime" <<'PY'
import json, pathlib, shutil, sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
tests = [a['executable'] for a in artifacts if a.get('target', {}).get('name') == 'profile_go_runtime' and a.get('executable')]
if len(tests) != 1:
    raise SystemExit('missing unique profile_go_runtime executable')
shutil.copy2(tests[0], sys.argv[2])
PY
cp "$CARGO_TARGET_DIR/debug/artifact-compile-child" "$stage/components/bin/"
(cd "$stage/components" && find . -type f -print0 | sort -z | xargs -0 sha256sum > "$stage/SHA256SUMS")
cp "$stage/SHA256SUMS" "$stage/components/SHA256SUMS"
mkdir -p "$(dirname "$FS_PROFILE_COMPONENTS")"
mv "$stage/components" "$FS_PROFILE_COMPONENTS"
