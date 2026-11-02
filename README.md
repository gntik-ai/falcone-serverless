# falcone-serverless

Falcone-oriented serverless execution for Kubernetes and OpenShift.

## Status

Design and implementation backlog. Option A selects Wasmtime/WASI execution inside restricted pods. Runtime implementation, cluster qualification and performance validation are pending.

## Layout

- `worker/`: Rust execution worker (Wasmtime/WASI host), a placeholder until RUN-01.
- `controller/`: Go namespaced pool controller, a placeholder until DEP-03.
- `docs/`: design, requirements and backlog.
- [AGENTS.md](AGENTS.md): code ownership across repositories, hard rules, build/test and cluster-testing rules.

## Documentation

- [Complete design and requirements](docs/falcone-serverless-design.md)
- [Backlog: 16 epics and 58 stories](docs/backlog.md)
- [Machine-readable requirements and issue payloads](docs/backlog.json)
- [Authoring sources and import instructions](docs/README.md)
- [GitHub issues](https://github.com/gntik-ai/falcone-serverless/issues)

The design preserves Falcone-specific contracts and integrates with Keycloak, SeaweedFS and Temporal. Untrusted code runs through qualified Wasm profiles; long-running work uses checkpoint/resume orchestration.

## Releasing images

The official images are published to Docker Hub by `.github/workflows/release-images.yml`:

- `docker.io/gntik/falcone-serverless-worker`
- `docker.io/gntik/falcone-serverless-controller`

To release:

1. Push a strict `vX.Y.Z` tag (for example `git tag v0.1.0 && git push origin v0.1.0`), or publish a GitHub Release for such a tag.
2. Each image gets the tags `X.Y.Z` and `sha-<commit>`. A tag push also moves `:latest`.
3. A version or `sha-` tag that already exists is never overwritten. An existing pair that resolves to one digest is verified and kept; any other combination fails the run.
4. Read the digests from `release-images.json`, which lists each image's reference and `sha256` digest:
   - in the run summary;
   - as the run artifact `release-images`;
   - for a published Release, as a Release asset.

   The Falcone chart pins the images by digest from this file.

A manual run (Actions → release-images → Run workflow) takes a version and builds without pushing while `dry_run` is checked.

The run needs:

- the repository secrets `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN`;
- both Docker Hub repositories created as public repositories beforehand. The workflow never creates them or changes their visibility.

## License

MIT, the same as Falcone. See [LICENSE](LICENSE).
