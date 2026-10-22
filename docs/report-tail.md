## 5. Remaining questions and ownership

The user has selected Option A. Motivation, Falcone-only interface scope, untrusted code, deployed-function/message scale and quota ownership are settled. They must not be reopened as generic architecture questions.

Implementation discovery still needs:

1. Exact OpenShift patch/version and existing SCC, namespace permissions, allowed image registries and network restrictions.
2. Representative existing JavaScript functions plus the real Python/Go analysis dependencies, input sizes, thread/process needs and expected memory.
3. WebSocket connection count, average/maximum payload, burst duration, fan-out, ordering/durability guarantees and fraction of messages that invoke code.
4. Current SeaweedFS notification configuration and its loss/replay behavior; preserve it until a compatible change is explicitly approved.
5. Long-job duration distribution, checkpoint interval, acceptable lost progress and whether application authors can implement checkpoints.
6. Whether any Model Context Protocol endpoints are actually deployed; WebSockets alone do not establish MCP compatibility.
7. Named maintainers/on-call/security approvers, actual SLA budget and repository/Project destination.

These are qualification inputs, not reasons to stop design work or permission to assume successful tests.

### Responsibility model

| Responsibility | Typical accountable owner | Work |
| --- | --- | --- |
| Product scope | Falcone product owner / technical lead | Supported languages/packages, quotas, compatibility promises and prioritization |
| Runtime | Runtime/platform engineering | Wasmtime integration, host interfaces, execution lifecycle, patches and compatibility |
| Deployment | DevOps/platform engineering | Namespaced chart, image delivery, CI and rollout/recovery procedures |
| Production incidents | SRE/on-call operations, with runtime developers as escalation | Alerts, capacity, failures, restoration and incident review |
| Corporate infrastructure | Existing OpenShift/platform administrators | Approved cluster/SCC/network/storage prerequisites; no new sandbox-runtime installation assumed |
| Security qualification | Application/platform security and independent testers | Threat model, sandbox/host-boundary review, adversarial tests and release evidence |
| Risk acceptance | Deploying organization's designated security/risk owner | Accept residual deployment risks; engineering tests do not substitute for organizational approval |

Small teams may combine roles, but each responsibility needs a named owner. Open-source maintainers own software/security advisories; self-hosting organizations normally own their deployment and on-call duty unless a support agreement says otherwise.

## 6. Implementation trade-offs and sequence

### Selected and deferred paths

| Choice | Decision | Consequence |
| --- | --- | --- |
| Wasmtime/WASI in restricted pods | Selected baseline | No cluster sandbox runtime required; language/package compatibility and host security become Falcone responsibilities |
| Arbitrary tenant containers/native binaries | Excluded from Option A | Cannot claim transparent compatibility with every Linux workload |
| New operator/CRDs/RuntimeClass | Excluded as mandatory dependencies | PostgreSQL authority and namespaced standard-resource controller |
| Existing Knative | Optional coexistence during migration | No new installation required; no full retirement until all real consumers are resolved |
| External execution pool | Not selected | Not an automatic fallback for unsupported functions |
| Interactive + analysis pools | Selected logical separation | Independent reservations/scaling with quotas governed by Falcone |
| Shared dirty guest instances | Excluded | Fresh execution state; cache only trusted code artifacts |
| gRPC/NATS/AMQP additions | Deferred pending measured need | Reuse current WebSockets/Kafka; avoid adding operators/services to solve an unproven gap |
| KEDA | Optional future alternative only | Not required for namespace-only installation; one replica owner remains mandatory |
| Multi-region | Deferred | First prove single-region execution, recovery and corporate-policy compatibility |

### Language enablement

1. Qualify the legacy JavaScript handler/envelope and its real dependencies.
2. Qualify Go with the chosen Wasm/WASI toolchain and required host interfaces.
3. Qualify Python using representative analysis dependencies; maintain an exact package/version catalogue and reject unsupported packages.

This is a testing order, not a claim that one language is universally faster or safer. No profile is enabled for untrusted production use based solely on a hello-world demo.

### Milestones

| Milestone | Deliverable | Exit gate |
| --- | --- | --- |
| M0 — feasibility | Accepted ADR, contract inventory, minimum restricted-OpenShift Wasmtime fixture | MIG-01 and WAS-01; exact policy/version evidence, no privilege escalation |
| M1 — secure execution | Host capability contract, qualified profiles, fresh-instance runner, Falcone auth/quotas, durable invocations and namespaced packaging | WAS-02/03 plus native slice; “native” means Falcone-owned backend, not tenant native machine code |
| M2 — platform behavior | Wasm artifact pipeline, routes/rollbacks, pool activation, events/DLQ, Temporal, checkpoint-aware jobs and HA | Recovery, isolation, package compatibility and mixed-load evidence |
| M3 — migration | WebSocket load qualification, hours-long analysis tests, legacy migration, restore/upgrade and retirement | All mandatory compatible-workload contracts pass; unresolved workloads block retirement |

### First executable qualification specification

- Deploy only approved standard namespaced resources with the available identity; record server-side admission and actual startup.
- Run a valid module and a deliberately rejected native/invalid artifact.
- Execute an infinite-loop guest, guest-memory exhaustion case and blocked host call; prove bounded independent cancellation.
- Attempt cross-tenant object/handle access and forbidden filesystem/network operations; prove denial and scoped audit.
- Run a real legacy JavaScript function and real analysis dependencies; publish supported and unsupported behaviors.
- Kill a checkpoint-aware analysis execution and resume from a committed SeaweedFS checkpoint through Temporal.

Deliverables from this spike are a reproducible fixture, pinned engine/toolchain manifests, compatibility results, resource measurements and a go/no-go report. This package specifies that work; it does not claim the fixture or tests have run.

### Backlog and import notes

The revised package contains **14 epics, 42 stories and 84 requirements**. Existing IDs are retained; WAS (qualification/capabilities) and JOB (long-running recovery) are added. Every requirement maps to a story and the dependency graph is checked for cycles.

The importer remains preview-first and preserves existing issue bodies. If an older package was imported elsewhere, importing this revision does not automatically rewrite those existing issues; review and update their changed scope separately. No GitHub issues or deployments have been changed by producing this revision.

### Definition of done

- Linked requirements and explicit positive/negative acceptance criteria pass on supported profiles.
- Integration evidence uses actual relevant services and actual corporate admission policies.
- No native fallback, implicit privilege escalation or cross-tenant capability leakage is accepted.
- Unsupported dependencies, timeout uncertainty and checkpoint limitations are visible to users.
- Engine, toolchain, package, chart and data-format versions are pinned and recoverable.
- Production qualification, SLO claims and Knative retirement require evidence, not design approval alone.


## Sources and evidence

Primary sources retrieved on 21 October 2026. Live documentation can change; pin versions before implementation. Repository links below pin the Falcone evidence snapshot. Recommendations, effort sizes and SLO targets are architectural judgments.

- **S1:** [Falcone README](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/README.md).
- **S2:** [Falcone Function executor](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/apps/control-plane/function-executor.mjs).
- **S3:** [Falcone Temporal functions.invoke Activity](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/apps/workflow-worker/src/activities/functions-invoke.mjs).
- **S4:** [Managed-Knative proposed integration](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/docs/installation/managed-knative-proposed.md).
- **S5:** [Existing managed-Knative design](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/openspec/changes/add-managed-knative-serving/design.md).
- **S6:** [Knative Serving architecture](https://knative.dev/docs/serving/architecture/).
- **S7:** [Kourier](https://github.com/knative-extensions/net-kourier).
- **S8:** [Knative event delivery failures](https://knative.dev/docs/eventing/event-delivery/).
- **S9:** [Falcone runtime-status and cleanup contract](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/docs/reference/architecture/knative-runtime-status.md).
- **S10:** [Fission architecture](https://fission.io/docs/architecture/).
- **S11:** [Fission executor](https://fission.io/docs/architecture/executor/).
- **S12:** [OpenWhisk architecture](https://github.com/apache/openwhisk/blob/master/docs/about.md).
- **S13:** [OpenWhisk reference and namespaces](https://github.com/apache/openwhisk/blob/master/docs/reference.md).
- **S14:** [OpenWhisk documentation](https://openwhisk.apache.org/documentation.html).
- **S15:** [Nuclio architecture](https://docs.nuclio.io/en/stable/concepts/architecture.html).
- **S16:** [Nuclio repository](https://github.com/nuclio/nuclio).
- **S17:** [Fn repository](https://github.com/fnproject/fn).
- **S18:** [Fn agent API](https://pkg.go.dev/github.com/fnproject/fn@v0.3.770/api/agent).
- **S19:** [OpenFaaS edition/pricing terms](https://www.openfaas.com/pricing/).
- **S20:** [OpenFaaS scale-to-zero](https://docs.openfaas.com/openfaas-pro/scale-to-zero/).
- **S21:** [Kubeless maintenance notice](https://github.com/vmware-archive/kubeless-website).
- **S22:** [Temporal Activity execution](https://docs.temporal.io/activity-execution) and [Activity failure detection](https://docs.temporal.io/encyclopedia/detecting-activity-failures).
- **S23:** [KEDA concepts](https://keda.sh/docs/2.22/concepts/) and [HTTP add-on scaling](https://keda.sh/http-add-on/0.16/concepts/scaling/).
- **S24:** [SeaweedFS S3 compatibility](https://github.com/seaweedfs/seaweedfs/wiki/Amazon-S3-API) and [filer notifications](https://github.com/seaweedfs/seaweedfs/wiki/Filer-Notification-Webhook). Deployed-version verification is mandatory; S3 compatibility does not prove AWS event-notification parity.
- **S25:** [GitHub sub-issues REST API](https://docs.github.com/en/rest/issues/sub-issues) and [GitHub CLI project item-add](https://cli.github.com/manual/gh_project_item-add).

- **S26:** [Wasmtime security](https://docs.wasmtime.dev/security.html).
- **S27:** [WASI capability security](https://wasi.dev/security).
- **S28:** [Wasmtime configuration and host-call interruption limits](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html).
- **S29:** [Python WebAssembly availability](https://docs.python.org/3/library/intro.html#webassembly-platforms).
- **S30:** [Kubernetes release lines](https://kubernetes.io/releases/), [OpenShift 4.20 release notes](https://docs.redhat.com/en/documentation/openshift_container_platform/4.20/html/release_notes/ocp-4-20-release-notes), and [SeaweedFS releases](https://github.com/seaweedfs/seaweedfs/releases). Version observations were checked earlier in this conversation on 21 October 2026; pin and requalify at implementation.
