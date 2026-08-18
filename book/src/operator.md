# The Operator (0.3.0)

The CRDs are settled and generated today (`deploy/crds.json`, kept in
sync with the Rust types by a CI gate); the reconcilers are 0.3.0's
work. What follows is the contract they will honour, written first so the
annotation users of 0.1/0.2 can see where their YAML is going.

## `DynamicConfigClass` — name the store once

The class bundles source, endpoint and the token Secret, so pods stop
repeating them:

```yaml
apiVersion: dynamic-config.rs/v1alpha1
kind: DynamicConfigClass
metadata:
  name: infra-consul
  namespace: billing
spec:
  source: consul
  endpoint: http://consul.infra.svc:8500
  tokenSecret: consul-agent-token     # optional; its `token` key travels
```

A pod then says only what is its own:

```yaml
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/class: "infra-consul"      # 0.3.0
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
```

The long-form annotations keep working forever; the class is sugar over
them, not a replacement.

## `DynamicConfigRender` — a ConfigMap instead of a sidecar

For workloads that cannot take an injected container — third-party
charts, jobs, anything sidecar-averse — the operator renders into a
ConfigMap on a cadence, and the workload mounts it like any other:

```yaml
apiVersion: dynamic-config.rs/v1alpha1
kind: DynamicConfigRender
metadata:
  name: billing-config
  namespace: billing
spec:
  class: infra-consul
  key: myapp/config.json
  target:
    configMap: billing-rendered
    file: config.properties        # the extension picks the format here too
  intervalSeconds: 30
status:                             # written by the operator
  renderedAt: "2026-08-18T09:00:00Z"
  lastError: null                   # kind and path only, never a value
```

Two honesty notes, stated now so 0.3.0 cannot soften them:

- **ConfigMap propagation is slow** — kubelet syncs mounted ConfigMaps
  on its own cadence (up to a minute-plus). The sidecar's emptyDir is
  the low-latency path; `DynamicConfigRender` trades latency for
  no-sidecar. The page will carry measured numbers when the reconciler
  ships.
- **A ConfigMap is not a Secret.** Rendering a Vault document into a
  ConfigMap is a downgrade the operator will refuse unless the target
  says `secret:` instead — that field arrives with the reconciler.

## What the operator will not do

Own lifecycles inside pods, restart workloads on render, or template
documents. It renders and it reports; reacting is the workload's
business, and the whole engine exists so reacting is cheap.
