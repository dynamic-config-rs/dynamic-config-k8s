# The Operator

Shipped as of 0.3.0: a `DynamicConfigRender` becomes a ConfigMap,
reconciled through the SAME source construction and rendering the
sidecar agent uses — one implementation, no drift between the two
paths a document can take into a pod. The CRDs stay generated from the
Rust types (`deploy/crds.json`, drift-gated in CI), and the e2e suite
drives the full loop: apply, render, propagate, delete,
garbage-collect.

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

## Classes: namespaced, or cluster-scoped

Two class kinds, the same split External Secrets Operator drew between
`SecretStore` and `ClusterSecretStore`:

- **`DynamicConfigClass`** (namespaced) — a team's own store, its
  `tokenSecret` read from the same namespace. Self-service, blast
  radius one namespace.
- **`ClusterDynamicConfigClass`** (cluster-scoped) — the platform
  team's store, defined once. Its credential names the namespace it
  lives in explicitly, so **tenants reference the class without ever
  being able to read the credential**:

```yaml
apiVersion: dynamic-config.rs/v1alpha1
kind: ClusterDynamicConfigClass
metadata:
  name: platform-consul
spec:
  source: consul
  endpoint: http://consul.infra.svc:8500
  tokenSecret:
    name: consul-token
    namespace: platform        # RBAC keeps tenants out of here
    key: token                 # `token` by default
  namespaces: [team-a, team-b] # the allowlist; absent = every namespace
```

A tenant's Render opts in by kind:

```yaml
spec:
  class: platform-consul
  classKind: ClusterDynamicConfigClass
```

Two rules, out loud. **The allowlist is enforced at
reconcile**: a Render in a namespace the class does not list fails with
the class named, in `status.lastError` — the platform team's boundary,
not a convention. And **the credential read happens with the
operator's identity**, which is why the operator's RBAC has cluster
`get` on Secrets: the tenant's own service account never touches the
platform namespace. Editing a cluster class re-renders every Render
referencing it, in every namespace — the same live wiring the
namespaced class has.

## Targets: a ConfigMap, or a Secret

`target` names exactly one destination:

```yaml
  target:
    configMap: billing-rendered   # workloads that mount files
    file: config.properties
```

```yaml
  target:
    secret: myapp-env             # workloads that read SECRETS natively
    shape: envEntries             # …or take environment through envFrom
```

The Secret target is for the consumers the file path cannot reach: an
operator that watches Kubernetes Secrets  reacts to
every reconcile with **no pod restart** — the Secret is a live object —
and an `envFrom` block turns `shape: envEntries` (every leaf of the
resolved document, dotted paths upper-snaked: `db.pool_size` →
`DB_POOL_SIZE`) into environment variables at the next container start.
Environment freezes at start; that is Kubernetes' rule, and this page
will not pretend otherwise. A **vault class is allowed into the Secret
target** — that is the container a secret store's document belongs in —
while the ConfigMap target keeps refusing it.

### Feeding a name someone else already chose

The commonest enterprise shape: **a helm chart or an operator demands a
Secret by name** — `auth.existingSecret` in half the chart ecosystem, a
`secretName:` field in an operator's CRD — and refuses env vars or
files. That named Secret is exactly what a Render produces:

```yaml
spec:
  class: platform-vault
  classKind: ClusterDynamicConfigClass
  key: secret/postgres
  target:
    secret: pg-credentials
    shape: entries          # leaf keys VERBATIM: postgres-password, …
```

```sh
helm install db bitnami/postgresql --set auth.existingSecret=pg-credentials
```

Three shapes, three contracts: `file` when the consumer wants one
document under one key; `envEntries` when it reads through `envFrom`
(keys upper-snaked to the env dialect); **`entries` when the key names
are someone else's contract** — every leaf verbatim, `postgres-password`
staying `postgres-password`, because any mangling breaks a name you do
not own. The password never appears in values, in git, or in a
developer's hands; the store document is the single source, and the
Secret follows it on the Render's interval.

Deleting the Render deletes its target: it is owned via
`ownerReferences`, so the cleanup is Kubernetes' own garbage collector
— no finalizer to get wrong. `status.renderedAt` says when the last
render landed; `status.lastError` carries kind and shape only, never a
value.

Two honesty notes, stated before the reconciler shipped and still
true:

- **ConfigMap propagation is slow** — the kubelet syncs mounted
  ConfigMaps on its own cadence (up to a minute-plus; the
  [engine book's Kubernetes Files
  page](https://dynamic-config-rs.github.io/kubernetes-files.html)
  walks the mechanism). The sidecar's emptyDir is the low-latency
  path; `DynamicConfigRender` trades latency for no-sidecar.
- **A ConfigMap is not a Secret.** The reconciler **refuses vault
  classes into ConfigMaps** with exactly that sentence; the `secret:`
  target is the lift, and the refusal message points at it.

## The class annotation — still ahead

`dynamic-config.rs/class` on a pod (the webhook resolving a class so
annotations shrink) is contract-only: it needs the webhook to read
CRs, which trades away its zero-RBAC posture the way
[selfRotate](secrets-and-tls.md) does, and that trade is taken
per-feature, not by default.

## What the operator will not do

Own lifecycles inside pods, restart workloads on render, or template
documents. It renders and it reports; reacting is the workload's
business, and the whole engine exists so reacting is cheap.
