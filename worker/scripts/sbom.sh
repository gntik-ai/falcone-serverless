#!/bin/sh
# Offline SPDX-2.3 inventory of Cargo.lock, not base-image OS packages.
set -eu
LC_ALL=C
export LC_ALL

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    printf '%s\n' 'usage: sbom.sh <outdir> [lockfile]' >&2
    exit 1
fi

invalid_commit() {
    printf '%s\n' 'SOURCE_COMMIT must be exactly 40 lowercase hexadecimal characters' >&2
    exit 1
}
source_commit=${SOURCE_COMMIT-}
case $source_commit in
    *[!0123456789abcdef]*) invalid_commit ;;
esac
[ "${#source_commit}" -eq 40 ] || invalid_commit

case $0 in
    */*) script_parent=${0%/*} ;;
    *) script_parent=. ;;
esac
script_dir=$(CDPATH= cd -P "$script_parent" && pwd)
workspace=$script_dir/../..
lockfile=${2:-$workspace/Cargo.lock}
outdir=$1
case $outdir in
    /*|./*|../*) ;;
    *) outdir=./$outdir ;;
esac

rust_toolchain=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)"[[:space:]]*$/\1/p' "$workspace/rust-toolchain.toml")
[ -n "$rust_toolchain" ]
mkdir -p "$outdir"
staging=$outdir/.sbom.$$
mkdir "$staging"
trap 'rm -f "$staging/sbom.spdx" "$staging/provenance.txt"; rmdir "$staging"' 0
trap 'exit 1' 1 2 3 15

awk -v commit="$source_commit" '
function emit() {
    if (!package) return
    if (name == "" || version == "") {
        bad = 1
        exit 1
    }
    print ""
    print "PackageName: " name
    print "SPDXID: SPDXRef-Package-" package
    print "PackageVersion: " version
    print "PackageDownloadLocation: NOASSERTION"
    print "FilesAnalyzed: false"
    print "PackageLicenseConcluded: NOASSERTION"
    print "PackageLicenseDeclared: NOASSERTION"
    print "PackageCopyrightText: NOASSERTION"
    if (length(checksum) == 64 && checksum ~ /^[[:xdigit:]]+$/)
        print "PackageChecksum: SHA256: " checksum
    print "Relationship: SPDXRef-DOCUMENT DESCRIBES SPDXRef-Package-" package
}
BEGIN {
    print "SPDXVersion: SPDX-2.3"
    print "DataLicense: CC0-1.0"
    print "SPDXID: SPDXRef-DOCUMENT"
    print "DocumentName: falcone-serverless-worker"
    print "DocumentNamespace: https://falcone.invalid/spdx/falcone-serverless-worker-" commit
    print "Creator: Tool: falcone-serverless-worker-sbom"
    # Fixed epoch required by SPDX CreationInfo; not a build timestamp.
    print "Created: 1970-01-01T00:00:00Z"
}
/^\[\[package\]\][[:space:]]*$/ {
    emit()
    package++
    name = version = checksum = ""
    next
}
/^(name|version|checksum)[[:space:]]*=/ {
    key = $1
    sub(/[[:space:]]*=.*/, "", key)
    value = $0
    sub(/^[^=]*=[[:space:]]*"/, "", value)
    sub(/"[[:space:]]*$/, "", value)
    if (key == "name") name = value
    if (key == "version") version = value
    if (key == "checksum") checksum = value
}
END {
    if (bad || !package) exit 1
    emit()
}
' "$lockfile" > "$staging/sbom.spdx"

{
    printf 'source_commit=%s\n' "$source_commit"
    printf 'rust_toolchain=%s\n' "$rust_toolchain"
    awk '
    toupper($1) == "FROM" {
        image = tolower($2)
        if (!(image in aliases)) print "base_image=" $2
        if (toupper($3) == "AS") aliases[tolower($4)] = 1
    }
    ' "$workspace/worker/Containerfile"
} > "$staging/provenance.txt"

mv "$staging/provenance.txt" "$outdir/provenance.txt"
mv "$staging/sbom.spdx" "$outdir/sbom.spdx"
