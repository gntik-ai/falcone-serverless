#!/usr/bin/env bash
# Decide how release-images.yml publishes one image (#250), from the registry state of its
# two immutable references, docker.io/<ns>/<image>:<version> and :sha-<commit>.
#
#   release-image-refs.sh <repo> <version> <short-sha> <push>
#
# Prints key=value lines (repo, version_ref, sha_ref, mode, existing_digest) for
# $GITHUB_OUTPUT. mode is:
#   build     both references are absent (or this run does not push): build and push both;
#   retag     only the sha reference exists: point the version at its digest, no rebuild;
#   existing  both exist with one digest: keep them, rebuild nothing.
# Any other state fails: a version that exists without this commit's sha tag, two different
# digests, a registry error other than buildx's "ERROR: <ref>: not found" for that very
# reference, or a malformed digest. A failed lookup is
# never read as "absent", so a transient registry error cannot lead to an overwrite.
# IMAGETOOLS overrides the inspector command (tests); it defaults to docker buildx imagetools.
set -euo pipefail

repo="$1"
version="$2"
short_sha="$3"
push="$4"
version_ref="${repo}:${version}"
sha_ref="${repo}:sha-${short_sha}"
read -r -a imagetools <<< "${IMAGETOOLS:-docker buildx imagetools}"

lookup() {
  local out err status digest
  err="$(mktemp)"
  status=0
  out="$("${imagetools[@]}" inspect --format '{{json .Manifest}}' "$1" 2> "$err")" || status=$?
  if [ "$status" -eq 0 ]; then
    rm -f "$err"
    digest="$(printf '%s' "$out" | jq -r '.digest // empty' 2> /dev/null || true)"
    if ! [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
      echo "::error::the registry returned a malformed digest for $1: '$digest'" >&2
      return 1
    fi
    echo "$digest"
  elif grep -qxF "ERROR: $1: not found" "$err"; then
    # Only buildx's own missing-manifest line for exactly this reference means "absent";
    # any other "not found" (an auth token endpoint, a missing repository) is an error.
    rm -f "$err"
    echo ABSENT
  else
    cat "$err" >&2
    rm -f "$err"
    echo "::error::the registry lookup of $1 failed (exit $status); refusing to guess that it is absent" >&2
    return 1
  fi
}

mode=build
existing=""
if [ "$push" = true ]; then
  version_digest="$(lookup "$version_ref")"
  sha_digest="$(lookup "$sha_ref")"
  if [ "$version_digest" = ABSENT ] && [ "$sha_digest" = ABSENT ]; then
    mode=build
  elif [ "$version_digest" = ABSENT ]; then
    mode=retag
    existing="$sha_digest"
    echo "$sha_ref exists ($sha_digest): $version_ref will point at it, no rebuild" >&2
  elif [ "$version_digest" = "$sha_digest" ]; then
    mode=existing
    existing="$version_digest"
    echo "$version_ref and $sha_ref already resolve to $version_digest: kept, not rebuilt" >&2
  else
    echo "::error::$version_ref ($version_digest) and $sha_ref ($sha_digest) do not resolve to one digest; refusing to overwrite an immutable tag" >&2
    exit 1
  fi
fi

echo "repo=$repo"
echo "version_ref=$version_ref"
echo "sha_ref=$sha_ref"
echo "mode=$mode"
echo "existing_digest=$existing"
