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

## License

MIT, the same as Falcone. See [LICENSE](LICENSE).
