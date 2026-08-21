# dynamic-config on Kubernetes

Annotate a pod; an agent appears in it that renders configuration from a
remote store to a file the application watches. The agent-injector
shape, for configuration.

```yaml
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "consul"
    dynamic-config.rs/endpoint: "http://consul:8500"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
```

The application needs no store client, no store credential, and no code
change: it reads a file, and if it reads it with any `dynamic-config`
binding it also reloads on every re-render — the agent writes
atomically, exactly the whole-file event a watcher wants.

**When not to use this.** The engine runs in-process everywhere; a Rust,
Python or Node service that can hold a store credential should usually
use its own binding's remote support and skip the sidecar entirely. This
integration exists for the pods that want files rendered *for* them:
Java services reading `.properties`, anything that must not carry store
credentials in-process, and fleets standardising one injection pattern.

How a document REACHES a workload — a live file, real environment
variables, or a native Kubernetes Secret — is its own decision, with a
map and two honest comparison tables (Vault Agent Injector, External
Secrets Operator) on [The Three Deliveries](injection-shapes.md).

## The three pieces, staged

| piece | ships in | today |
|---|---|---|
| agent | 0.2.0 | all nine stores — the blocking six since 0.1.0, etcd/nats/s3 on the async path since 0.1.1 — and **watched** rather than polled since 0.2.0 |
| webhook | 0.2.0 | golden-tested; the annotation contract is v1; three TLS modes incl. selfRotate; admission and rotation are metrics since 0.2.0 |
| operator | 0.2.0 | Render → ConfigMap reconciler shipped, Class watch wired, e2e-gated, **leader-elected** since 0.2.0 |
