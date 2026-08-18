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

## The three pieces, staged

| piece | ships in | today |
|---|---|---|
| agent | 0.1.0 | built and tested — consul, vault, config-server, firestore, git, redis; etcd, nats and s3 join in 0.2.0 |
| webhook | 0.2.0 | golden-tested; the annotation contract is v1 |
| operator | 0.3.0 | CRDs settled and generated; reconcilers land here |
