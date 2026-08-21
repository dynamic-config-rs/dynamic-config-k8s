# Installation Defaults and Gates

Two kinds of installation-time decisions live in the webhook's
configuration, and they must not be confused: **defaults**, which a pod
may always override, and **gates**, which a pod may never override.
Both arrive as chart values, or as a
[ConfigMap of YAML](#writing-them-as-yaml) that kustomize can hand over
too, or as environment variables on the webhook Deployment — three
spellings of one thing, and every one of them is validated when the
webhook starts. A mistyped value stops the install, never the first
admission.

```text
defaults:  annotation  >  per-store default  >  fleet default  >  built-in
pins:      a value marked "!" (or under overridable: "false") refuses a
           DIFFERING annotation — the tiers still fill what pods omit
gates:     the installer's word is final
```

## Writing them as YAML

An installation reaches the webhook as **strings**, because that is what
an environment variable is — and several of those strings are little
grammars:

```text
DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS="vault: overridable=false, endpoint=https://vault:8200; s3: file-mode=0640?"
DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW="payments: vault, s3; *: consul"
```

Fine to parse and unpleasant to write. So the same settings may be
written as maps:

```yaml
# values.yaml
agent:
  defaults:
    perStore:
      vault:
        overridable: false
        endpoint: https://vault.vault.svc:8200
        auth: kubernetes
        watch-seconds: 30
      s3:
        agent-memory-limit: 128Mi
webhook:
  sourceAllow:
    payments: [vault, s3]
    "*": [consul]
  agentEnvAllow:
    payments: [HTTPS_PROXY, "AWS_*"]
```

**A map is defined as what it renders to.** It travels to the pod as a
mounted ConfigMap and is turned into the grammar *there*, by the same
parser the string form goes through — so there is one set of rules, one
set of messages, and two spellings that cannot mean different things.
Pins, markers and tiers work the same either way: `overridable: false`
in a map is the `overridable=false` in a string.

The webhook reads that document through its own engine: the YAML reader
it gives applications is the one it reads its own configuration with.

### Kustomize gets the same thing

This is why the document exists at all rather than a chart-side
rendering. A kustomize base has no template engine, so a hand-written
ConfigMap is the only structured form it can hand over —
`deploy/kustomize/base/installation.yaml`, which ships empty and
commented:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: dynamic-config-webhook-installation
data:
  installation.yaml: |
    storeDefaults:
      vault:
        overridable: false
        endpoint: https://vault.vault.svc:8200
    sourceAllow:
      payments: [vault, s3]
      "*": [consul]
```

### Which wins

An **environment variable set on the container beats the document**: a
document is the installation written down once, a variable is somebody
standing in front of it for this deployment — the more specific of the
two, which is the same rule the configuration layers themselves follow.

A setting the document does not know is **refused at startup**, with the
list of the ones there are. A default silently ignored is a default that
never applied, and the first sign of it would be a pod running without
the posture somebody thought they had set.

## The knob vocabulary

Every defaultable knob is spelled exactly as its annotation is
spelled — there is no second vocabulary to learn, and every tier is
validated with the same rules as the annotation it stands in for:

| knob | built-in | validation |
|---|---|---|
| `agent-cpu-request` | `10m` | a Kubernetes quantity |
| `agent-memory-request` | `32Mi` | a Kubernetes quantity |
| `agent-cpu-limit` | none | a Kubernetes quantity |
| `agent-memory-limit` | `64Mi` | a Kubernetes quantity |
| `file-mode` | the agent's `0644` | octal, at most `0777`, owner-readable |
| `watch-seconds` | `15` | whole seconds |
| `mode` | `sidecar` | `init` / `sidecar` / `both` |
| `volume-medium` | `memory` | `memory` / `disk` |
| `native-sidecar` | `false` | `"true"` / `"false"` |
| `agent-run-as-user` | `65532` | numeric, `0` refused |
| `agent-run-as-group` | `65532` | numeric, `0` refused |
| `metrics-port` | none | a port; a pod opts out of a default with `"0"` |
| `path` | none | absolute; the rendered file's location |
| `source` | none | fleet level only (`agent.defaults.source`): one of the nine stores |

Pod-wide knobs (mode, volume, resources, identity, metrics) resolve
against the DEFAULT render's store; per-render knobs (`watch-seconds`,
`file-mode`) resolve against [each render's own
store](annotations.md#several-documents-one-pod).

And per store, EVERY store-shaped annotation is defaultable — the
address, the document, the credentials' Secret names, the auth flags,
the templates:

```text
endpoint   endpoint-secret   key          token-secret   password-secret
ca-configmap   tls-secret    ssh-secret   aws-secret     section
auth   auth-mount   auth-role   auth-username   auth-token-path
namespace   ref   api-url   template   template-configmap
```

Each is validated as its annotation would be (`*-secret` values need
the `<secret-name>/<key>` slash), and the either-or pairs
(`endpoint`/`endpoint-secret`, `template`/`template-configmap`) resolve
as a LEVEL: any pod-side answer mutes both installation halves, so a
pod that chose `endpoint-secret` never inherits a stray plain
endpoint.

With `source` and `path` at the fleet tier and `endpoint` and `key` in
a store's tier, the minimal pod is a single line of intent:

```yaml
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
```

Defaulting `key` deserves a sentence of caution: two pods leaning on
the same store default read the SAME document. For a shared,
cluster-wide configuration that is exactly right; for per-app
documents, leave `key` to the pods.

What stays the pod's alone: `env-inject` and `env-restart` (they name
a container only the pod knows), `agent-env` (gated per pod), and
`inject` itself — a default that opts workloads in silently is not a
default, it is a surprise.

## Pinning: the override mode

Every installation value carries an override rule, and the closest
word wins:

1. a trailing `!` on the value — **pinned** — or a trailing `?` —
   **overridable**;
2. no marker: the STORE's own flag, when its `perStore` group carries
   `overridable=true|false`;
3. neither: whatever `agent.defaults.overridable` says (`"true"`
   unless set otherwise).

A pinned value REFUSES a differing annotation at admission — never
silently outvotes it, because a value the author wrote and did not
get is a debugging session. The same value restated passes, which
keeps migrations painless. Knobs the installation never set are
untouched by all of this: `overridable: "false"` pins what you SET,
not the whole contract.

```yaml
agent:
  defaults:
    overridable: "false"        # everything set below is pinned…
    fileMode: "0640"            # …like this
    watchSeconds: "30?"         # …except this one, explicitly opened
    perStore:
      # A store pins its own group: everything vault-shaped is the
      # platform team's word, except the watch interval.
      vault: "overridable=false, endpoint=https://vault.vault.svc:8200, auth=kubernetes, watch-seconds=30?"
      # And a store can OPEN its group under a strict fleet flag:
      # consul values stay the pods' even with the "false" above.
      consul: "overridable=true, endpoint=http://consul.infra.svc:8500"
```

All three rungs compose per store and per value: a `!` inside an
`overridable=true` group pins just that value; a `?` inside an
`overridable=false` group opens just that one. The rule always comes
from the TIER that supplied the value — a store's flag never pins a
fleet default.

The pin follows the pair rule too: a pinned `endpoint` also refuses a
pod that answers with `endpoint-secret` — the address is one decision,
and it is not the pod's to make. A pinned fleet `source`
(`source: "consul!"`) refuses pods that name any other store, which is
the enforcement twin of the `sourceAllow` gate below.

## Per-store defaults, all nine stores

`agent.defaults.perStore` is the tier between the annotation and the
fleet. One realistic installation, every store present — each line
pairs with the pod that uses it below:

The string form below is one of the two spellings; the same installation
[as maps](#writing-them-as-yaml) reads the same.

```yaml
# values.yaml — or, for kustomize, the same settings in
# base/installation.yaml, or joined with "; " into
# DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS
agent:
  defaults:
    watchSeconds: "30"          # the fleet's floor
    fileMode: "0640"
    perStore:
      # Local, cheap to poll: tighten the interval — and the address
      # is the platform's, PINNED, so pods need not carry it and
      # cannot point elsewhere.
      consul: "endpoint=http://consul.infra.svc:8500!, watch-seconds=10"
      # Secrets: the whole group is the platform team's word
      # (address, auth, CA), and the file lands owner-only, owned by
      # the app's uid.
      vault: "overridable=false, endpoint=https://vault.vault.svc:8200, auth=kubernetes, ca-configmap=vault-ca, file-mode=0400, agent-run-as-user=1000, agent-run-as-group=1000"
      # A JVM config server answers slowly; give the render room.
      config-server: "watch-seconds=20, agent-memory-limit=96Mi"
      # Billed per read: poll gently.
      firestore: "watch-seconds=120"
      # A clone per render costs memory and remote quota.
      git: "watch-seconds=120, agent-memory-limit=128Mi"
      # In-memory store, near-free reads.
      redis: "watch-seconds=10"
      # Watches are pushed by etcd itself; the interval is a backstop.
      etcd: "watch-seconds=60"
      # JetStream KV is push-cheap too.
      nats: "watch-seconds=10"
      # A GET per poll is a line on a bill; and S3 documents are often
      # the big ones.
      s3: "watch-seconds=60, agent-memory-limit=128Mi"
```

The pods, every field filled. None of them repeats a knob the
installation already set — the tier exists so they never have to:

```yaml
# consul — plain HTTP, a KV key
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "consul"
    dynamic-config.rs/endpoint: "http://consul.infra.svc:8500"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    # watch-seconds arrives from perStore.consul: 10
```

```yaml
# vault — kubernetes auth through the pod's own ServiceAccount,
# a private CA, one section of the secret
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "vault"
    dynamic-config.rs/endpoint: "https://vault.vault.svc:8200"
    dynamic-config.rs/key: "secret/myapp"
    dynamic-config.rs/section: "db"
    dynamic-config.rs/auth: "kubernetes"
    dynamic-config.rs/auth-role: "myapp"
    dynamic-config.rs/ca-configmap: "vault-ca"
    dynamic-config.rs/path: "/config/rendered.yaml"
    # file-mode 0400 and uid/gid 1000 arrive from perStore.vault
```

```yaml
# config-server — the Spring-style application/profile pair as the key
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "config-server"
    dynamic-config.rs/endpoint: "http://config-server.infra.svc:8888"
    dynamic-config.rs/key: "billing/prod"
    dynamic-config.rs/path: "/config/rendered.json"
    # watch-seconds 20 and the 96Mi limit arrive from perStore
```

```yaml
# firestore — the endpoint is the GCP project, the key a document path
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "firestore"
    dynamic-config.rs/endpoint: "acme-prod"
    dynamic-config.rs/key: "config/billing"
    dynamic-config.rs/path: "/config/rendered.json"
    # watch-seconds 120 arrives from perStore.firestore
```

```yaml
# git — a repository over ssh, a ref, a file inside the tree
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "git"
    dynamic-config.rs/endpoint: "git@github.com:acme/config.git"
    dynamic-config.rs/ref: "main"
    dynamic-config.rs/key: "billing/prod.yaml"
    dynamic-config.rs/ssh-secret: "config-deploy-key"
    dynamic-config.rs/path: "/config/rendered.yaml"
    # watch-seconds 120 and the 128Mi limit arrive from perStore.git
```

```yaml
# redis — the password rides in the URL, so the WHOLE endpoint is a
# Secret instead of an annotation
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "redis"
    dynamic-config.rs/endpoint-secret: "redis-cred/url"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    # watch-seconds 10 arrives from perStore.redis
```

```yaml
# etcd — a REQUIRED client certificate and a private CA (the
# password-auth twin swaps tls-secret for password-secret)
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "etcd"
    dynamic-config.rs/endpoint: "https://etcd.infra.svc:2379"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/tls-secret: "etcd-client-tls"
    dynamic-config.rs/ca-configmap: "etcd-ca"
    dynamic-config.rs/path: "/config/rendered.toml"
    # watch-seconds 60 arrives from perStore.etcd
```

```yaml
# nats — a JetStream KV bucket and the key inside it
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "nats"
    dynamic-config.rs/endpoint: "nats://nats.infra.svc:4222"
    dynamic-config.rs/key: "config/db.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    # watch-seconds 10 arrives from perStore.nats
```

```yaml
# s3 — the endpoint IS the bucket; api-url points at MinIO/Ceph/R2,
# and aws-secret carries static credentials those servers need
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "s3"
    dynamic-config.rs/endpoint: "myapp-config"
    dynamic-config.rs/key: "prod/db.json"
    dynamic-config.rs/api-url: "http://minio.infra.svc:9000"
    dynamic-config.rs/aws-secret: "minio-cred"
    dynamic-config.rs/path: "/config/rendered.toml"
    # watch-seconds 60 and the 128Mi limit arrive from perStore.s3
```

Any pod that DOES set `watch-seconds` (or any other knob) still wins —
the tiers only answer for what the pod left unsaid.

## The gates, in depth

Three gates, one authority model: they live in the webhook's own
configuration, owned by whoever installs it. None of them is a
namespace annotation — whoever edits a namespace is usually the tenant
being gated, and a gate its subject can open is not a gate. None of
them reads the namespace object either: the webhook holds no RBAC and
asks the API server for nothing; the pod's namespace arrives inside
the AdmissionReview, and the ruling comes from configuration alone.

All three share one grammar:

```text
spec   = group *( ";" group )
group  = [ namespace ":" ] names     ; no head, or "*" = every namespace
names  = name *( "," name )
```

### agent-env allow — closed until opened

```yaml
webhook:
  agentEnvAllow: "payments: HTTPS_PROXY, AWS_*; *: RUST_LOG"
```

`agent-env` puts environment on the container that holds store
credentials, and environment steers SDKs. Concretely, on this agent:
`HTTPS_PROXY` reroutes every vault/S3/consul request through a proxy
of the pod author's choosing — with the bearer tokens and signatures
inside; `AWS_CA_BUNDLE` and `SSL_CERT_FILE` swap the trust roots those
connections verify against; `AWS_EC2_METADATA_DISABLED`,
`AWS_PROFILE`, `NO_PROXY` all change where credentials come from or
where traffic goes. That is why this gate defaults to **closed**: an
empty allowlist refuses the annotation everywhere, and every name a
pod wants must be opened by the installer, optionally per namespace.

Name rules: `UPPER_SNAKE`, exact match, or a trailing `*` as a prefix
glob (`AWS_*`). A bare `*` opens everything — legitimate on a
single-team cluster, a finding on a shared one. The refusal a pod
sees names the variable, the namespace, what IS allowed there, and
the chart value that opens the gate; it does not enumerate other
namespaces' rules — one tenant's refusal must not describe another's
setup.

What the gate does NOT govern: `agent-env` values (only names), the
app container's environment (the agent's only), and the fleet's own
`agent.defaults.env` (next section).

### The fleet environment — no gate, on purpose

```yaml
agent:
  defaults:
    env: "HTTPS_PROXY=http://egress.infra.svc:3128, RUST_LOG=info"
```

`agent.defaults.env` is environment EVERY injected agent gets — the
cluster-wide egress proxy, a fleet log level. It passes no allowlist
because the installer sets both the values and the allowlist; a gate
you hold both sides of checks nothing. Merging rules, exactly:

- A pod's own `agent-env` overrides a fleet name — the pod said it
  more specifically. (The pod's name still needs the allowlist: the
  OVERRIDE is a pod-author action even when the name is fleet-known.)
- When a pod uses `aws-secret`, the fleet's `AWS_ACCESS_KEY_ID` /
  `AWS_SECRET_ACCESS_KEY` step aside — one credential, one place, and
  the pod named its Secret.
- Everything else rides along verbatim, on every render's agent.

### Source allow and deny — open until narrowed

```yaml
webhook:
  sourceAllow: "payments: vault, s3; *: consul"   # empty = every store
  sourceDeny: "sandbox: git"                       # subtractive, wins
```

The source gates decide which STORES a namespace may render from.
Two lists, because the two postures are different jobs:

- **`sourceAllow`** — empty means `*`: every store, everywhere, so an
  upgrade changes nothing until the installer says so. Non-empty
  flips the posture: ONLY the listed sources pass, per namespace. Use
  it when the store set is policy: payments reads vault and s3,
  everyone may read consul, and anything unlisted is refused with a
  message naming what IS allowed there.
- **`sourceDeny`** — always subtractive, and it outranks the
  allowlist: a source both listed and denied is denied. Use it for
  the surgical cut that does not flip the posture: git is off in the
  sandbox namespace, everything else stays open.

Both gates are judged against EVERY render on the pod — the default
one and each [named suffix](annotations.md#several-documents-one-pod):
a denied store cannot ride in as `source.cache`. Entries are
validated against the real store list at webhook startup, so
`sourceDeny: "sandbox: got"` fails the install instead of silently
gating nothing — in a security control, a typo that fails open is the
worst of the four outcomes.

Deciding between them:

| you want | use |
|---|---|
| nothing changes on upgrade | leave both empty |
| this namespace uses exactly these stores | `sourceAllow` |
| this store is banned here, rest stays open | `sourceDeny` |
| allow broadly, carve exceptions | both — deny wins on overlap |

### Kustomize, same doors

Every value above is either a key in
[`base/installation.yaml`](#kustomize-gets-the-same-thing) — the
readable form, and usually what you want — or one environment variable
on the webhook Deployment. The chart is convenience, not capability:

```yaml
# kustomization.yaml, an overlay patch
patches:
  - target: { kind: Deployment, name: dynamic-config-webhook }
    patch: |
      - op: add
        path: /spec/template/spec/containers/0/env/-
        value:
          name: DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS
          value: "vault: file-mode=0400; git: watch-seconds=120"
      - op: add
        path: /spec/template/spec/containers/0/env/-
        value:
          name: DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY
          value: "sandbox: git"
```

The chart's schema cannot see a kustomize patch — which is exactly why
the webhook re-validates the complete installation at startup and
refuses to serve on any error. Helm users get two doors; kustomize
users get the one that matters.

| chart value | environment variable |
|---|---|
| `agent.defaults.cpuRequest` … `.memoryLimit` | `DYNAMIC_CONFIG_AGENT_CPU_REQUEST` … `_MEMORY_LIMIT` |
| `agent.defaults.fileMode` | `DYNAMIC_CONFIG_AGENT_FILE_MODE` |
| `agent.defaults.watchSeconds` | `DYNAMIC_CONFIG_AGENT_WATCH_SECONDS` |
| `agent.defaults.mode` | `DYNAMIC_CONFIG_AGENT_MODE` |
| `agent.defaults.volumeMedium` | `DYNAMIC_CONFIG_AGENT_VOLUME_MEDIUM` |
| `agent.defaults.nativeSidecar` | `DYNAMIC_CONFIG_AGENT_NATIVE_SIDECAR` |
| `agent.defaults.runAsUser` / `.runAsGroup` | `DYNAMIC_CONFIG_AGENT_RUN_AS_USER` / `_GROUP` |
| `agent.defaults.metricsPort` | `DYNAMIC_CONFIG_AGENT_METRICS_PORT` |
| `agent.defaults.env` | `DYNAMIC_CONFIG_AGENT_ENV` |
| `agent.defaults.source` | `DYNAMIC_CONFIG_AGENT_SOURCE` |
| `agent.defaults.path` | `DYNAMIC_CONFIG_AGENT_PATH` |
| `agent.defaults.overridable` | `DYNAMIC_CONFIG_AGENT_DEFAULTS_OVERRIDABLE` |
| `agent.defaults.perStore` | `DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS` |
| `webhook.agentEnvAllow` | `DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW` |
| `webhook.sourceAllow` | `DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW` |
| `webhook.sourceDeny` | `DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY` |
