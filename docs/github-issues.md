# Published GitHub backlog

56 issues: 14 epics and 42 stories. Parent/child and dependency links are recorded in issue bodies. Native sub-issue relationships and Project board membership are not configured.

## EP-API — API and developer experience

Epic: [#1](https://github.com/gntik-ai/falcone-serverless/issues/1)

- [API-01 — Preserve the Functions API and add CLI operations](https://github.com/gntik-ai/falcone-serverless/issues/15)
- [API-02 — Deliver language starters and declarative manifests](https://github.com/gntik-ai/falcone-serverless/issues/16)
- [API-03 — Integrate runtime status, console and SDK schemas](https://github.com/gntik-ai/falcone-serverless/issues/17)

## EP-RUN — Runtime and execution

Epic: [#2](https://github.com/gntik-ai/falcone-serverless/issues/2)

- [RUN-01 — Implement Wasmtime execution and qualified source adapters](https://github.com/gntik-ai/falcone-serverless/issues/18)
- [RUN-02 — Enforce guest and host limits with safe draining](https://github.com/gntik-ai/falcone-serverless/issues/19)
- [RUN-03 — Enforce fresh instances and runtime support policy](https://github.com/gntik-ai/falcone-serverless/issues/20)

## EP-DEP — Build, revisions and deployment

Epic: [#3](https://github.com/gntik-ai/falcone-serverless/issues/3)

- [DEP-01 — Build and verify Wasm function artifacts](https://github.com/gntik-ai/falcone-serverless/issues/21)
- [DEP-02 — Implement revisions, weighted rollout and rollback](https://github.com/gntik-ai/falcone-serverless/issues/22)
- [DEP-03 — Reconcile worker pools and collect artifacts safely](https://github.com/gntik-ai/falcone-serverless/issues/23)

## EP-INV — Invocation, routing and autoscaling

Epic: [#4](https://github.com/gntik-ai/falcone-serverless/issues/4)

- [INV-01 — Implement authenticated sync and durable async invocation](https://github.com/gntik-ai/falcone-serverless/issues/24)
- [INV-02 — Implement bounded admission and worker-pool activation](https://github.com/gntik-ai/falcone-serverless/issues/25)
- [INV-03 — Preserve WebSocket behavior and benchmark message routing](https://github.com/gntik-ai/falcone-serverless/issues/26)

## EP-EVT — Events and triggers

Epic: [#5](https://github.com/gntik-ai/falcone-serverless/issues/5)

- [EVT-01 — Implement CloudEvents adapters and trigger management](https://github.com/gntik-ai/falcone-serverless/issues/27)
- [EVT-02 — Implement retry, deduplication, DLQ and replay](https://github.com/gntik-ai/falcone-serverless/issues/28)
- [EVT-03 — Integrate schedules and ordered dispatch](https://github.com/gntik-ai/falcone-serverless/issues/29)

## EP-WF — Temporal integration

Epic: [#6](https://github.com/gntik-ai/falcone-serverless/issues/6)

- [WF-01 — Preserve functions.invoke and retry identity](https://github.com/gntik-ai/falcone-serverless/issues/30)
- [WF-02 — Coordinate revision pins, retries and cancellation](https://github.com/gntik-ai/falcone-serverless/issues/31)
- [WF-03 — Expose scoped workflow start and signal operations](https://github.com/gntik-ai/falcone-serverless/issues/32)

## EP-SEC — Identity and security

Epic: [#7](https://github.com/gntik-ai/falcone-serverless/issues/7)

- [SEC-01 — Implement Keycloak verification and Falcone authorization](https://github.com/gntik-ai/falcone-serverless/issues/33)
- [SEC-02 — Qualify restricted pods and the application sandbox](https://github.com/gntik-ai/falcone-serverless/issues/34)
- [SEC-03 — Secure host capabilities, egress and audit](https://github.com/gntik-ai/falcone-serverless/issues/35)

## EP-TEN — Tenancy, plans and quotas

Epic: [#8](https://github.com/gntik-ai/falcone-serverless/issues/8)

- [TEN-01 — Implement tenant scoping and namespace bindings](https://github.com/gntik-ai/falcone-serverless/issues/36)
- [TEN-02 — Enforce Falcone-owned quotas and workload fairness](https://github.com/gntik-ai/falcone-serverless/issues/37)
- [TEN-03 — Implement suspension and durable scoped cleanup](https://github.com/gntik-ai/falcone-serverless/issues/38)

## EP-STO — SeaweedFS, secrets and metadata

Epic: [#9](https://github.com/gntik-ai/falcone-serverless/issues/9)

- [STO-01 — Implement Wasm artifact, checkpoint and scoped S3 storage](https://github.com/gntik-ai/falcone-serverless/issues/39)
- [STO-02 — Bridge storage events and rotate OpenBao secrets](https://github.com/gntik-ai/falcone-serverless/issues/40)
- [STO-03 — Implement durable invocation metadata and outbox recovery](https://github.com/gntik-ai/falcone-serverless/issues/41)

## EP-OBS — Observability, audit and metering

Epic: [#10](https://github.com/gntik-ai/falcone-serverless/issues/10)

- [OBS-01 — Add tenant-scoped logs and distributed tracing](https://github.com/gntik-ai/falcone-serverless/issues/42)
- [OBS-02 — Deliver metrics, dashboards and operational alerts](https://github.com/gntik-ai/falcone-serverless/issues/43)
- [OBS-03 — Add auditable metering and retention controls](https://github.com/gntik-ai/falcone-serverless/issues/44)

## EP-OPS — Operations, CI/CD and resilience

Epic: [#11](https://github.com/gntik-ai/falcone-serverless/issues/11)

- [OPS-01 — Package namespace-only installation and resource ownership](https://github.com/gntik-ai/falcone-serverless/issues/45)
- [OPS-02 — Implement HA and the integration CI gate](https://github.com/gntik-ai/falcone-serverless/issues/46)
- [OPS-03 — Prove restore, upgrades and safe uninstall](https://github.com/gntik-ai/falcone-serverless/issues/47)

## EP-MIG — Compatibility and Knative retirement

Epic: [#12](https://github.com/gntik-ai/falcone-serverless/issues/12)

- [MIG-01 — Freeze Falcone contracts and accept Option A architecture](https://github.com/gntik-ai/falcone-serverless/issues/48)
- [MIG-02 — Implement backend selection and staged tenant migration](https://github.com/gntik-ai/falcone-serverless/issues/49)
- [MIG-03 — Certify Wasm compatibility and retire owned Knative resources](https://github.com/gntik-ai/falcone-serverless/issues/50)

## EP-WAS — Wasm qualification and capability contract

Epic: [#13](https://github.com/gntik-ai/falcone-serverless/issues/13)

- [WAS-01 — Prove restricted OpenShift execution feasibility](https://github.com/gntik-ai/falcone-serverless/issues/51)
- [WAS-02 — Specify host capabilities and safe artifact loading](https://github.com/gntik-ai/falcone-serverless/issues/52)
- [WAS-03 — Qualify language profiles and legacy compatibility](https://github.com/gntik-ai/falcone-serverless/issues/53)

## EP-JOB — Long-running analysis and recovery

Epic: [#14](https://github.com/gntik-ai/falcone-serverless/issues/14)

- [JOB-01 — Implement asynchronous analysis jobs and checkpoint contracts](https://github.com/gntik-ai/falcone-serverless/issues/54)
- [JOB-02 — Recover and cancel analysis jobs through Temporal](https://github.com/gntik-ai/falcone-serverless/issues/55)
- [JOB-03 — Qualify mixed interactive and long-running load](https://github.com/gntik-ai/falcone-serverless/issues/56)

