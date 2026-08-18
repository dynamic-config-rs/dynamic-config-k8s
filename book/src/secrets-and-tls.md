# Secrets, Certificates, and What Shows Where

Before wiring any store's authentication, one map of where things are
visible. Kubernetes shows different fields to different eyes:

| where a value sits | who sees it |
|---|---|
| an annotation | anyone with `get pod` — and every system that logs admission objects |
| a container argument | anyone with `get pod`; `kubectl describe pod` prints `args` in full |
| an environment variable from a Secret | the pod spec shows only the Secret's *name*; the value needs `get secret` rights |
| a mounted Secret/ConfigMap | same — the spec names the object, the bytes stay behind RBAC |

That table decides the whole contract:

- **Names travel in annotations** — a role name, an auth method's
  mount, a username. Reading them tells an attacker nothing they could
  not guess.
- **Secrets travel as environment variables drawn from Secrets** — the
  agent reads three, and there is no flag for the second one on
  purpose:

| variable | annotation that fills it | carries |
|---|---|---|
| `DYNAMIC_CONFIG_AGENT_TOKEN` | `token-secret: <secret>/<key>` | the bearer/access token |
| `DYNAMIC_CONFIG_AGENT_PASSWORD` | `password-secret: <secret>/<key>` | the second secret, where a method has one: approle's secret id, userpass/ldap's password |
| `DYNAMIC_CONFIG_AGENT_ENDPOINT` | `endpoint-secret: <secret>/<key>` | the address, when the address embeds a password — a redis url |

- **Key material travels as mounts**, read-only, into the agent
  container alone — the application containers never see them:

| annotation | object | lands at |
|---|---|---|
| `ca-configmap: <name>[/<key>]` | ConfigMap, default key `ca.crt` | `/etc/dynamic-config/ca/` |
| `tls-secret: <name>` | `kubernetes.io/tls` Secret | `/etc/dynamic-config/tls/tls.crt` + `tls.key` |
| `ssh-secret: <name>[/<key>]` | `kubernetes.io/ssh-auth` Secret, default key `ssh-privatekey`, mounted `0400` | `/etc/dynamic-config/ssh/` |

## A private CA, end to end

Most internal Vaults, Consuls and git hosts serve TLS from an internal
PKI. The chain is one ConfigMap and one annotation:

```sh
kubectl create configmap vault-ca --from-file=ca.crt=./internal-ca.pem
```

```yaml
metadata:
  annotations:
    dynamic-config.rs/endpoint: "https://vault.vault.svc:8200"
    dynamic-config.rs/ca-configmap: "vault-ca"
```

The agent gets `--ca /etc/dynamic-config/ca/ca.crt`, and the store
crate under it adds the CA to its trust roots — the same
`TlsConfig` every store crate takes, so the spelling is identical for
all six stores.

There is no way to turn verification off. The store crates refuse
that setting by design, and the agent adds no flag for it: a
configuration channel that skips TLS verification is a configuration
channel anyone on the path can write to.

## A client certificate

Some stores authenticate *with* the certificate ([vault's `cert`
method](vault.md#cert-a-tls-client-certificate)), some merely allow
mTLS in front. Either way it is one `kubernetes.io/tls` Secret:

```sh
kubectl create secret tls vault-client \
  --cert=./client.pem --key=./client-key.pem
```

```yaml
    dynamic-config.rs/tls-secret: "vault-client"
```

The certificate and key must come together; the agent refuses one
without the other before any byte leaves the pod.

## Why the pod's own identity beats all of this

Three of the six stores can authenticate a pod with **no distributed
secret at all** — the pod's service-account token or the node's cloud
identity:

- vault: [`auth: kubernetes`](vault.md#kubernetes-the-pods-own-identity)
- consul: [`auth: kubernetes`](consul.md#kubernetes-login-with-the-pods-identity)
- firestore: [`auth: metadata-server`](firestore.md#metadata-server-workload-identity)

Where one of those is available, prefer it: nothing to rotate, nothing
to leak, and revocation is the platform's own. The token-shaped methods
on every page exist for the stores and shops where it is not.
