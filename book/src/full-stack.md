# The Full Stack, One Deployment

Vault to sidecar to template to file to a live process — every hop on
one page, as the manifests you would actually apply. Each piece has its
own chapter; production is all of them at once, and the order they
come up in.

```text
Vault (secret/myapp)                        the values that move
  → injected agent, kubernetes auth        no distributed secret
  → minijinja template (ConfigMap)         the store's shape → the app's
  → /config/rendered.yaml (tmpfs)          off the node's disk
  → the app's own watcher                  live without a restart
```

## 0. The one-time pieces

```console
$ helm install dynamic-config oci://ghcr.io/dynamic-config-rs/charts/dynamic-config \
    --namespace dynamic-config --create-namespace

$ vault auth enable kubernetes
$ vault write auth/kubernetes/config kubernetes_host=https://kubernetes.default.svc
$ vault write auth/kubernetes/role/billing \
    bound_service_account_names=billing \
    bound_service_account_namespaces=shop \
    policies=billing-read ttl=1h
$ kubectl -n shop create configmap vault-ca --from-file=ca.crt=./internal-ca.pem
```

## 1. The template — the store's shape becomes the app's

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: billing-template
  namespace: shop
data:
  template: |
    db:
      host: {{ db.host }}
      pool_size: {{ db.pool_size | default(8) }}
    features:
      cache: {{ features.cache | default(false) }}
```

Re-read every render, so editing the ConfigMap is itself a live change —
no rollout. The [Rendering chapter](rendering.md#templates) owns the
semantics; the one rule worth restating is that a template failure
leaves the **previous rendered file in place**, which is last-known-good
at the file layer.

## 2. The workload — annotations are the whole integration

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: billing
  namespace: shop
spec:
  replicas: 3
  selector: { matchLabels: { app: billing } }
  template:
    metadata:
      labels: { app: billing }
      annotations:
        dynamic-config.rs/inject: "true"
        dynamic-config.rs/source: "vault"
        dynamic-config.rs/endpoint: "https://vault.vault.svc:8200"
        dynamic-config.rs/key: "secret/myapp"
        dynamic-config.rs/auth: "kubernetes"
        dynamic-config.rs/auth-role: "billing"
        dynamic-config.rs/ca-configmap: "vault-ca"
        dynamic-config.rs/template-configmap: "billing-template"
        dynamic-config.rs/path: "/config/rendered.yaml"
        dynamic-config.rs/native-sidecar: "true"
        dynamic-config.rs/watch-seconds: "30"
    spec:
      serviceAccountName: billing
      containers:
        - name: app
          image: myapp:1
          # /config arrives injected: tmpfs, shared with the agent.
          readinessProbe:
            httpGet: { path: /readyz, port: 8080 }
```

No secret in the manifest, no volume stanza to write, no sidecar to
maintain: the webhook injects the agent, the agent logs in with the
pod's own service account, and the token never exists as a Kubernetes
Secret. `native-sidecar: "true"` makes the agent an init container with
`restartPolicy: Always`, so a Job with these annotations still
finishes.

## 3. The app — the last hop is an ordinary file

The agent's output is a file, so the app's side is the engine's
ordinary file story — any of the three languages, here Rust:

```rust,ignore
#[dynamic_config]
#[derive(Deserialize)]
struct Db {
    host: String,
    pool_size: u32,
}

Db::builder("db").file("/config/rendered.yaml").init()?;
Db::builder("db")
    .file("/config/rendered.yaml")
    .watch(Duration::from_millis(500))?
    .detach();
```

The write is atomic (rename), so the watcher never reads half a
render — the same guarantee the kubelet's `..data` swap gives a mounted
ConfigMap, [held at both layers](https://dynamic-config-rs.github.io/kubernetes-files.html).

## What moves without a rollout, and what does not

| Change | Takes effect |
|---|---|
| the secret in Vault | next `watch-seconds` tick → render → app's watcher |
| the template ConfigMap | next render (kubelet sync + render tick) |
| an annotation | **rollout** — injection happens at pod creation |
| the chart's own values | `helm upgrade`, webhook restart, no app rollout |

The annotation row is the one that surprises: annotations are read by
the webhook when the pod is *admitted*, so changing them is a
Deployment edit and rolls pods — which is also why it is safe, because
every replica converges through the same admission path.

## Watching it work

```console
$ kubectl -n shop logs deploy/billing -c dynamic-config-agent --tail=5
$ kubectl -n shop exec deploy/billing -c app -- cat /config/rendered.yaml
$ vault kv put secret/myapp db='{"host": "db-2.internal", "pool_size": 16}'
# … within watch-seconds, the same two commands show the new values,
# and the app's /readyz generation has moved. No pod restarted.
```
