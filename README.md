# falcone-serverless

Falcone-oriented serverless execution for Kubernetes and OpenShift.

## Status

Design and implementation backlog. Option A selects Wasmtime/WASI execution inside restricted pods. Runtime implementation, cluster qualification and performance validation are pending.

## Documentation

- [Complete design and requirements](docs/falcone-serverless-design.md)
- [Backlog: 14 epics and 42 stories](docs/backlog.md)
- [Machine-readable requirements and issue payloads](docs/backlog.json)
- [Authoring sources and import instructions](docs/README.md)
- [GitHub issues](https://github.com/gntik-ai/falcone-serverless/issues)

The design preserves Falcone-specific contracts and integrates with Keycloak, SeaweedFS and Temporal. Untrusted code runs through qualified Wasm profiles; long-running work uses checkpoint/resume orchestration.
