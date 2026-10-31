#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
workspace=$(CDPATH= cd -P "$script_dir/../../.." && pwd)
temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/runtime-metrics-selftest.XXXXXXXX")
trap 'rm -rf "$temp_dir"' 0
trap 'exit 1' HUP INT TERM

mkdir "$temp_dir/workspace"
# Include hidden entries without copying the real Git metadata or build output.
for entry in "$workspace"/* "$workspace"/.[!.]* "$workspace"/..?*; do
    if [ ! -e "$entry" ] && [ ! -L "$entry" ]; then
        continue
    fi
    case ${entry##*/} in
        .git|target) continue ;;
    esac
    cp -R "$entry" "$temp_dir/workspace/"
done
export CARGO_TARGET_DIR="$temp_dir/target"
export CARGO_NET_OFFLINE=true

copy="$temp_dir/workspace"
inventory="$copy/docs/contracts/runtime-metrics/check-test-inventory.sh"
source="$copy/worker/src/runtime_metrics.rs"
cp "$source" "$temp_dir/original.rs"

# A passing baseline rules out missing toolchains or dependencies as the cause.
sh "$inventory" "$copy"

expect_rejection() {
    if sh "$inventory" "$copy" >"$temp_dir/inventory.log" 2>&1; then
        cat "$temp_dir/inventory.log" >&2
        printf '%s\n' "inventory accepted $1 test" >&2
        exit 1
    fi
    cat "$temp_dir/inventory.log"
    # Require the missing-pass diagnostic, rather than an unrelated cargo failure.
    grep -Fxq 'expected exactly one passing test: runtime_metrics::tests::committed_catalogue_validates (found 0)' "$temp_dir/inventory.log"
}

sed 's/fn committed_catalogue_validates()/fn renamed_committed_catalogue_validates()/' \
    "$temp_dir/original.rs" >"$source"
if cmp -s "$temp_dir/original.rs" "$source"; then
    printf '%s\n' 'rename mutation did not change the copied source' >&2
    exit 1
fi
expect_rejection renamed

sed '/^    fn committed_catalogue_validates()/i\
    #[ignore]
' "$temp_dir/original.rs" >"$source"
if cmp -s "$temp_dir/original.rs" "$source"; then
    printf '%s\n' 'ignore mutation did not change the copied source' >&2
    exit 1
fi
expect_rejection ignored
printf '%s\n' 'inventory rejected renamed and ignored tests'
