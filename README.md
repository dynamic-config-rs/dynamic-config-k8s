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

Three pieces, all shipped:

| | since | what it is |
|---|---|---|
| `dynamic-config-agent` | 0.1.0 | init/sidecar binary: store → resolved document → atomic file, any format — `.properties` and `.ini` included. **Nine stores** (consul, vault, config-server, firestore, git, redis; etcd, nats and s3 on the async path since 0.1.1), every auth method each store takes |
| `dynamic-config-webhook` | 0.1.0 | the mutating admission webhook that writes the agent into annotated pods; terminates its own TLS — chart-minted by default, cert-manager optional, or **selfRotate** (0.1.1): the webhook mints, rotates and re-trusts its own pair |
| `dynamic-config-operator` | 0.1.1 | `DynamicConfigClass` (store bundles) and `DynamicConfigRender` → ConfigMap reconciler, for sidecar-averse workloads; owner-reference cleanup, e2e-gated |
| `deploy/` | — | the chart (full values reference in its README), the kustomize base + overlays, and the CRDs — one generated source, three drift-gated copies |
| [`ROADMAP.md`](ROADMAP.md) | — | the original ladder shipped in full with 0.1.1; what remains there is demand-gated (admission-latency histogram, template functions) |
| `examples/` | — | twenty-two ready-to-apply manifests + six real-software walkthroughs (Airflow, Grafana, Kafka, Postgres existingSecret, multi-tenant, and a four-component end-to-end shop stack): every store, every auth method, the native-sidecar Job, the template that renders an env file |

Images: `ghcr.io/dynamic-config-rs/dynamic-config-{agent,webhook,operator}`,
mirrored to Docker Hub as `docker.io/ctolon17/dynamic-config-{agent,webhook,operator}`
with **identical digests** (one build, both registries), multi-arch
(amd64 + arm64), SBOM and provenance attested, cosign-signed keyless —
the signature's certificate names this repository's release workflow:

```sh
cosign verify ghcr.io/dynamic-config-rs/dynamic-config-agent:v0.1.1 \
  --certificate-identity-regexp 'github.com/dynamic-config-rs/dynamic-config-k8s' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

The chart lives at `oci://ghcr.io/dynamic-config-rs/charts/dynamic-config`
and on [ArtifactHub](https://artifacthub.io/packages/helm/dynamic-config/dynamic-config).

The library needs none of this — the engine runs in-process and the two
binding books say so. This exists for the pods that want files rendered
*for* them: Java services reading `.properties`, sidecar-pattern shops,
and anything that should not carry store credentials in-process.

The book: <https://dynamic-config-rs.github.io/k8s/>. `e2e/smoke.sh` is
thefive-minute proof against a kind cluster.

MIT licensed.

What you may build on and find unchanged tomorrow is written down: the [Compatibility Contract](https://dynamic-config-rs.github.io/compatibility.html).
