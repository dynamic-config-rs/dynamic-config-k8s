# The Three Deliveries: File, Env, Secret

One engine, three ways a document reaches a workload — because real
software disagrees about how it wants to be configured. Grafana re-reads
files; Airflow reads environment variables at boot and nothing else; a
Strimzi-shaped operator watches Kubernetes Secrets. Picking the delivery
is a one-line decision here; what follows is the map.

| | **File** (webhook + agent) | **Env** (`env-inject`) | **Secret** (operator target) |
|---|---|---|---|
| The consumer | reads/watches a file | reads environ at start | reads/watches a k8s Secret, or `envFrom` |
| Freshness | **live** — atomic rename, watcher cadence | frozen at container start (Kubernetes' rule); `env-restart` opts into a kubelet container-restart on change — seconds, no pod recreation | live object; watchers react, `envFrom` at next start |
| Touches etcd? | **never** — tmpfs emptyDir | never — same tmpfs file, sourced | **yes** — a Secret lives in etcd, stated out loud |
| Restart to update? | no | yes (next pod start) | no for Secret-watchers; yes for `envFrom` |
| Set up by | pod annotations | pod annotations (+ explicit `command`) | `DynamicConfigRender` CR |
| Real example | [Grafana](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/real/grafana-datasources.yaml) | [Airflow](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/real/airflow-scheduler.yaml) | [Kafka client](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/real/kafka-client-properties.yaml) |

The decision procedure, in order: **can the consumer read a file?**
File delivery — it is the only fully live one and the only one that
never touches etcd. **Does something else own the consumer** (an
operator that only reads Secrets)? The Secret target. **Env-only
software?** `env-inject`, with the start-freeze stated rather than
papered over.

## Against the Vault Agent Injector

The closest relative of the webhook+agent half — same architecture
(mutating webhook, injected init/sidecar, shared memory volume, pod
service-account auth, file permissions and run-as knobs), different
center of gravity:

| | Vault Agent Injector | dynamic-config-k8s |
|---|---|---|
| Backends | Vault | nine stores — Vault among them, plus Consul, etcd, git, Redis, NATS, S3, Firestore, config-server |
| Pod auth | Kubernetes auth (SA token) | the same, wherever the store speaks it (Vault, Consul); IRSA/Workload Identity for S3/Firestore; secret-based stays first-class where a store never will |
| What is delivered | rendered secrets, Consul-Template language | the **resolved configuration document** — precedence, validation, provenance — in json/toml/yaml/ini/properties or a minijinja template |
| File perms / ownership | annotations | annotations (`file-mode`, `agent-run-as-user/group`), root refused at admission |
| Env variables | you rewrite the command by hand to `source` the file | `env-inject` writes the wrap for you, refuses the impossible cases by name |
| k8s Secret objects | no | the operator's `secret:` target, when the consumer requires one |
| Lease renewal | vault-agent renews leases | the agent re-fetches on its interval; Vault reads are versioned-metadata-gated |
| Scope | secrets delivery | configuration delivery that treats secrets as first-class fields |

### The injector's template idiom, translated

The Vault injector spells "render me a connection string" as a
per-secret annotation pair; here the same result is one source and one
template, because the whole pod has one resolved document:

```yaml
# Vault Agent Injector:
#   vault.hashicorp.com/agent-inject-secret-db-creds: "secret/data/db-app"
#   vault.hashicorp.com/agent-inject-template-db-creds: |
#     {{- with secret "secret/data/db-app" -}}
#     postgres://{{ .Data.data.username }}:{{ .Data.data.password }}@postgres:5432/appdb
#     {{- end }}

# dynamic-config-k8s, the same string from the same KV secret:
dynamic-config.rs/source: "vault"
dynamic-config.rs/endpoint: "https://vault.vault.svc:8200"
dynamic-config.rs/key: "secret/db-app"
dynamic-config.rs/auth: "kubernetes"
dynamic-config.rs/auth-role: "db-app"
dynamic-config.rs/path: "/config/db.env"
dynamic-config.rs/template: |
  DATABASE_URL=postgres://{{ username }}:{{ password }}@postgres:5432/appdb
```

The template owns the bytes (minijinja, strict-undefined: a typo is an
error, not an empty string), so any shape works — a URL, an `.env`, a
whole config file. What does NOT translate is the `database/creds/…`
path in the injector's example: that is Vault's **dynamic secrets
engine**, credentials minted per-request with leases — the boundary the
next paragraph prices. This agent reads KV; for minted-with-TTL
credentials, run the Vault Agent beside it.

One more idiom, matched: the injector's **several `-secret-<name>`
pairs per pod** are this webhook's [named renders](annotations.md) —
`source.db`, `key.db`, `path.db` beside the default, one agent and one
file per name, all in one shared directory. When the pod wants them
MERGED into a single document instead, the
[config server](config-server.md) composes sections and the pod reads
one endpoint. What has no counterpart is `agent-inject-command` (a
post-render hook): [`env-restart`](annotations.md) covers the restart
case, and anything richer belongs to the app.

Honest edge the other way: for **dynamic Vault secrets with leases**
(database credentials minted per-pod, TTL renewal mid-life), the Vault
Agent is the purpose-built tool and this is not — this agent re-fetches
documents; it does not manage leases.

## Against External Secrets Operator

The closest relative of the operator half — same split between a
namespaced store and a platform-owned cluster store, different product:

| | External Secrets Operator | dynamic-config-k8s |
|---|---|---|
| Store definition | `SecretStore` / `ClusterSecretStore` | `DynamicConfigClass` / `ClusterDynamicConfigClass` — the same two scopes, allowlist included |
| Output | a Kubernetes Secret, always | a ConfigMap, a Secret, **or a file no etcd ever sees** |
| The etcd trade | every delivered secret lives in etcd | only the Secret target does, and choosing it is explicit — the file path exists precisely to avoid it |
| Data model | key-by-key secret mapping | whole configuration documents: precedence, validation, formats, provenance |
| Env delivery | `envFrom` the Secret | the same via `envEntries` — or `env-inject`, which needs no Secret at all |
| Backends | very many secret managers | nine configuration stores |
| Templating | Secret templates | minijinja over the resolved document |

Honest edge the other way: as a **secret-synchronisation fleet tool**
across dozens of managers (AWS/GCP/Azure SM, Doppler, 1Password…), ESO
has breadth this project does not chase — the store list here grows by
demand, not by roadmap.

## Use cases, mapped

- **Airflow / env-only software** → `env-inject` over a rendered
  dotenv ([example](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/real/airflow-scheduler.yaml)); add `env-restart: "true"` and a changed document
  restarts just that container in seconds — otherwise changes wait for
  the next pod start, and that limit is stated instead of hidden.
- **Grafana / anything that re-reads files** → the sidecar; live
  updates, zero etcd, tmpfs only ([example](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/real/grafana-datasources.yaml)).
- **Strimzi-shaped operators / JVM `client.properties`** → the Secret
  target, `file` or `envEntries` shape ([example](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/real/kafka-client-properties.yaml)).
- **Multi-tenant platforms** → `ClusterDynamicConfigClass` with a
  `namespaces` allowlist; tenants never see a credential
  ([example](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/real/cluster-class.yaml)).
- **A chart's `existingSecret` / an operator's `secretName:`** → the
  Secret target with `shape: entries` — leaf keys verbatim, so the
  names some other chart already chose are met exactly
  ([example](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/real/existing-secret-postgres.yaml)).
- **All of it at once** → the four-component
  [shop stack](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/real/full-stack-shop.yaml):
  three secrets injected three ways (a chart's `existingSecret`, a
  `secretKeyRef` env, a mounted-and-live file), the API on
  `env-inject` + `env-restart`, the worker on live files — credentials
  existing only in the platform namespace.
- **Vault dynamic database credentials with TTLs** → the Vault Agent
  Injector, genuinely. Use both: it owns the lease, this owns the
  configuration.
