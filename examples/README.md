# Examples

One file per store-and-auth pairing, each self-contained: the Secret or
ConfigMap it needs (with placeholder values to replace) and the
annotated pod. Apply, adjust names, read the rendered file back:

```sh
kubectl apply -f examples/consul-anonymous.yaml
kubectl exec billing -c app -- cat /config/rendered.toml
```

| file | store | auth | shows off |
|---|---|---|---|
| `consul-anonymous.yaml` | consul | none | the smallest possible ask — five annotations |
| `consul-token.yaml` | consul | ACL token | `token-secret`, secrets-as-env |
| `consul-kubernetes.yaml` | consul | k8s auth method | login with the pod's identity |
| `vault-kubernetes.yaml` | vault | kubernetes | no distributed secret at all, plus a private CA |
| `vault-approle.yaml` | vault | approle | the two-halves pattern: role id public, secret id in a Secret |
| `vault-token.yaml` | vault | token | the tutorial method, honestly labelled |
| `config-server.yaml` | config-server | bearer | the aggregation front — including for etcd/nats/s3 today |
| `firestore-workload-identity.yaml` | firestore | metadata-server | zero-secret GCP; init mode |
| `git-token.yaml` | git | https token | a deploy token reading a tagged ref |
| `git-ssh.yaml` | git | deploy key | `ssh-secret` and the custom-image caveat |
| `redis-acl.yaml` | redis | ACL user in url | `endpoint-secret`: the whole address is the secret |
| `native-sidecar-job.yaml` | consul | none | a Job that finishes because the sidecar is native |
| `template-env.yaml` | consul | none | output templating: a document in, an `.env` file out |
| `etcd-tls.yaml` | etcd | TLS client certificate | the credential IS the certificate; etcd's operator-standard method |
| `etcd-password.yaml` | etcd | username + password | `auth-username` + `password-secret`; secret-based, first-class on purpose |
| `watch-driven.yaml` | etcd | username + password | the push path: a store whose watch streams, `watch-seconds` as the **resync** floor rather than a poll, and the staleness gauge to alert on |
| `nats-creds.yaml` | nats | `.creds` file | the NATS account idiom, named by `auth-token-path` |
| `s3-irsa.yaml` | s3 | IRSA (ambient chain) | workload identity; the service account carries the role |
| `file-mode-and-identity.yaml` | consul | none | `file-mode: "0640"` + `agent-run-as-user/group`: the rendered file's mode and owner matched to the app container |
| `env-inject.yaml` | consul | none | REAL OS env vars : init agent renders a dotenv, `env-inject` wraps the app's command — start-time by Kubernetes' own rule |
| `render-to-secret.yaml` | consul | none | the operator's Secret target + `shape: envEntries` (the envFrom shape): a Secret kept reconciled, watchers react with no restart |
| `multi-render.yaml` | vault + redis | kubernetes / url-secret | several documents, one pod: `.<name>`-suffixed annotations, one agent and one file per name |
| `namespace-gating.yaml` | consul | none | the opt-in namespace label for `webhook.namespaceGating=true` — the outer guard; the per-pod annotation stays required |

Under [`real/`](real/), the same machinery on real software:

| File | Software | Shows |
|---|---|---|
| [`real/airflow-scheduler.yaml`](real/airflow-scheduler.yaml) | Apache Airflow 2.10 | `AIRFLOW__SECTION__KEY` env dialect templated from the store, `env-inject` making it the scheduler's real environment |
| [`real/grafana-datasources.yaml`](real/grafana-datasources.yaml) | Grafana 11 | datasource provisioning YAML rendered where Grafana already looks, kept live by the sidecar |
| [`real/kafka-client-properties.yaml`](real/kafka-client-properties.yaml) | Kafka (bitnami 3.9) | the operator's Secret target carrying a `.properties` file a JVM client mounts |
| [`real/cluster-class.yaml`](real/cluster-class.yaml) | any tenant | `ClusterDynamicConfigClass`: platform-owned store + credential, tenant Renders with a `namespaces` allowlist |
| [`real/existing-secret-postgres.yaml`](real/existing-secret-postgres.yaml) | bitnami/postgresql | the `existingSecret` contract: `shape: entries` produces the exact key names another chart demands |
| [`real/full-stack-shop.yaml`](real/full-stack-shop.yaml) | four components, two stores | end-to-end: THREE secrets injected three ways (existingSecret, secretKeyRef env, mounted+live), API on `env-inject`+`env-restart`, worker on live files, credentials only in the platform namespace |

The [book's store pages](https://dynamic-config-rs.github.io/k8s/) walk
through every one of these with the store-side setup commands; the
[annotation reference](https://dynamic-config-rs.github.io/k8s/annotations.html)
is the full contract.
