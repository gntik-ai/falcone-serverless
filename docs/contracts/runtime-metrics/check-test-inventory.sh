#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
workspace=${1:-"$script_dir/../../.."}
cd "$workspace"

for name in \
    runtime_metrics::tests::committed_catalogue_validates \
    runtime_metrics::tests::runtime_mode_values_are_wasm_and_oci \
    runtime_metrics::tests::oci_phases_are_the_four_cold_start_phases \
    runtime_metrics::tests::series_inventory_is_exact \
    runtime_metrics::tests::series_budget_is_bounded_by_8 \
    runtime_metrics::tests::validator_rejects_single_invariant_mutations \
    runtime_metrics::tests::constants_match_catalogue \
    tests::version_is_the_crate_version
do
    if output=$(cargo test --workspace --locked -- --exact "$name" 2>&1); then
        count=$(printf '%s\n' "$output" | grep -Fxc "test $name ... ok" || true)
        if [ "$count" != 1 ]; then
            printf '%s\n' "$output" >&2
            printf '%s\n' "expected exactly one passing test: $name (found $count)" >&2
            exit 1
        fi
    else
        printf '%s\n' "$output" >&2
        printf '%s\n' "cargo failed for $name" >&2
        exit 1
    fi
    printf '%s\n' "test $name ... ok"
done
