# Trusted worker and controller image inputs

## Digest pinning

`worker/Containerfile` builds the trusted worker from these two external inputs:

- Builder: `docker.io/library/rust:1.99.1-slim-trixie@sha256:b8e062b1c3a80183df83d9ba187cadd2b5ab84bc78d453dbfeb2fccd5f982247`
- Runtime: `gcr.io/distroless/cc-debian13:nonroot@sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e`

The builder tag follows the channel in `rust-toolchain.toml`. The image supplies
that toolchain directly; the Containerfile does not copy the toolchain file and
therefore does not request its rustfmt and clippy components during the build.
Build from the repository root, using `worker/Containerfile` as the build file.
The build runs `cargo build --release --locked -p falcone-serverless-worker`;
only its binary is copied from the build stage into the nonroot runtime image.
There are no ADD instructions or copies from external images.

The digests are the operator-supplied manifest index values from the 2026-10-31
amendment. Offline tests check their format and the expected repositories and
tags; they cannot authenticate registry contents. Before merge, an independent
reviewer must resolve both tags against their registries and confirm the exact
digests above. This R3 change also requires operator approval covering the digest
amendment. No image build, publication or deployment is added by this task, and
the existing CI does not build this Containerfile or the controller one. Image
build validation remains an operator gate.

## Controller image

`controller/Containerfile` builds the trusted pool controller from these two
external inputs:

- Builder: `docker.io/library/golang:1.26.10-trixie@sha256:a050dcceee6eb9bf110151662dfac7662f3bec40927c7dee48731e3d92083d42`
- Runtime: `gcr.io/distroless/static-debian13:nonroot@sha256:e2e927ec666bae08560abb3c55d0659eceabb657f56b6782ab500a9fc7f555e3`

Both are OCI image indexes supplied by the operator in the 2026-11-01 amendment.
The builder tag's major.minor equals the `go` directive in `controller/go.mod`.
Build from the repository root, using `controller/Containerfile` as the build file:
`docker build -f controller/Containerfile .`. The build stage copies only
`controller/go.mod` and `controller/main.go` (no test files; there is no `go.sum`),
sets `GOTOOLCHAIN=local` so no toolchain is downloaded, and runs
`CGO_ENABLED=0 go build -trimpath -ldflags '-s -w'`. Only the static binary is copied
into the runtime image. The Containerfile has no ADD instruction, no RUN after the
runtime FROM and no copy from an external image; the runtime image has no shell or
package manager. The image runs as the numeric user `65532:65532` and its ENTRYPOINT
is the exec-form binary `/usr/local/bin/falcone-serverless-controller`.

The binary ignores arguments, so running the image with no arguments or with
`--help` prints `falcone-serverless-controller 0.1.0` and exits 0. Real flag
parsing and `--help` output belong to DEP-03 (#23).

The root `.dockerignore` excludes `docs/`, `target/` and `.git` from every build
context. No path that `worker/Containerfile` or `controller/Containerfile` copies
from the context is excluded, and every `docs/` include in the worker is test-only.
BuildKit prefers a `<Containerfile>.dockerignore` placed next to a Containerfile, so
adding one would bypass the root rules; do not add one without updating them.

### Image age exception

Base images are normally at least two weeks old. The builder index was pushed
2026-10-28, under 14 days before this change, and the operator accepted it for the
Go 1.26.10 security fixes to `crypto/x509` in the Go standard library compiled into
the binary; Go 1.26.9, pushed 2026-10-10, lacks those fixes. The runtime index was
uploaded 2026-09-13, and its immutable tag is
`nonroot-02a141105c8d1b1987df7993c1a635ec72ac9c65`.

The digests are operator-supplied. Offline tests (`controller/image_test.go`) check
the reference format, byte-equality with the values above, the user, the exec-form
entrypoint and the `.dockerignore` rules; they cannot authenticate registry contents.
Before merge, an independent reviewer must resolve both tags to these digests. The
image build, `docker inspect` of `Config.User`, and a run of the image are validated by
the operator's CI job, which is added in a separate change.

## SBOM

Run `SOURCE_COMMIT=<40 lowercase hex characters> sh worker/scripts/sbom.sh <outdir>
[lockfile]` to generate `sbom.spdx` and `provenance.txt` without network access.
The optional lockfile defaults to the workspace `Cargo.lock`; this default and
the toolchain and Containerfile inputs resolve relative to the script, so the
command works from any working directory. The output directory and an explicit
relative lockfile resolve from the caller's working directory.

The SPDX-2.3 tag-value SBOM lists every Cargo.lock package, including workspace
packages, with its name and version. A package checksum is emitted only when
the lockfile supplies a SHA256 checksum. This is a lockfile inventory, including
development dependencies; it does not claim that every listed crate is linked
into the release binary. Download locations and licenses are `NOASSERTION`.
It does not cover OS packages in either base image. Function artifacts and
other trusted platform images are outside this task.

The document namespace is derived from SOURCE_COMMIT. SPDX CreationInfo requires
a Created field: its fixed value `1970-01-01T00:00:00Z` is a format constant,
not the build time. Neither output contains wall-clock, random or host-specific
values. Repeated runs with the same inputs produce identical bytes. The script
uses POSIX sh and awk/sed for parsing, with standard file utilities for staging
and publishing output. SOURCE_COMMIT is validated before any writes. Output is
written to temporary files, cleaned up on failure and moved into place only
after generation succeeds.

## Provenance

`provenance.txt` records `source_commit` from SOURCE_COMMIT, `rust_toolchain`
read from `rust-toolchain.toml`, and one `base_image` for each external FROM
reference parsed from `worker/Containerfile`. References to earlier stage aliases
are internal and are omitted. Base image references are never duplicated as
constants in the generator.

This deterministic statement records caller-declared source identity and local
build inputs. It is not an authenticated attestation or proof of an image build,
and does not bind an output image digest. Callers must supply the actual source
commit and preserve the statement alongside the SBOM in their image build
process. This task adds no CI job that consumes these files.

## Signing

This task neither signs nor verifies worker images or statements. Image signing,
signature verification and authenticated attestations required by FR-DEP-02 need
separate implementation and review. These files alone do not complete FR-DEP-02.
