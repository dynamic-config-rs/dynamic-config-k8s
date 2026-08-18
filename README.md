# dynamic-config-k8s

The [dynamic-config](https://dynamic-config-rs.github.io/) Kubernetes
integration, the agent-injector shape: annotate a pod, and an agent
appears in it that renders configuration from a remote store to a file
the application watches.

```yaml
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "consul"
    dynamic-config.rs/endpoint: "http://consul:8500"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
```

Three pieces, staged shippable:

| | ships in | what it is |
|---|---|---|
| `dynamic-config-agent` | 0.1.0 | init/sidecar binary: store → resolved document → atomic file, any format — `.properties` and `.ini` included. Six stores (consul, vault, config-server, firestore, git, redis), every auth method each store takes |
| `dynamic-config-webhook` | 0.2.0 | the mutating admission webhook that writes the agent into annotated pods; terminates its own TLS (chart-minted certificate by default, cert-manager optional) |
| `examples/` | — | thirteen ready-to-apply manifests: every store, every auth method, the native-sidecar Job, the template that renders an env file |
| `dynamic-config-operator` | 0.3.0 | `DynamicConfigClass` (store bundles) and `DynamicConfigRender` (store → ConfigMap, for sidecar-averse workloads) |

Images: `ghcr.io/dynamic-config-rs/dynamic-config-{agent,webhook,operator}`,
mirrored to Docker Hub, multi-arch, SBOM attached, cosign-signed.

The library needs none of this — the engine runs in-process and the two
binding books say so. This exists for the pods that want files rendered
*for* them: Java services reading `.properties`, sidecar-pattern shops,
and anything that should not carry store credentials in-process.

The book: <https://dynamic-config-rs.github.io/k8s/>. `e2e/smoke.sh` is
thefive-minute proof against a kind cluster.

MIT licensed.
