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

The [book's store pages](https://dynamic-config-rs.github.io/k8s/) walk
through every one of these with the store-side setup commands; the
[annotation reference](https://dynamic-config-rs.github.io/k8s/annotations.html)
is the full contract.
