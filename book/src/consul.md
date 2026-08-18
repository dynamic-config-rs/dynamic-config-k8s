# Consul

A KV path over HTTP. Four ways in, ordered from development to
production.

The key is a KV path with an extension — `myapp/config.json` — and the
extension names the *stored* format; [the rendered format is the `path`
annotation's extension](rendering.md), and the two need not agree.

## Anonymous

Correct for a Consul with ACLs disabled, and for a `default` policy
that allows reads — both ordinary in development. No auth annotation at
all:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: billing
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "consul"
    dynamic-config.rs/endpoint: "http://consul.infra.svc:8500"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
spec:
  containers:
    - name: app
      image: myapp:1
```

This is the exact flow the e2e harness runs on every pull request.

## An ACL token

The `CONSUL_HTTP_TOKEN` you already have, moved into a Secret:

```sh
kubectl create secret generic consul-token --from-literal=token=b3a7…
```

```yaml
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "consul"
    dynamic-config.rs/endpoint: "http://consul.infra.svc:8500"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    dynamic-config.rs/token-secret: "consul-token/token"
```

The webhook wires the Secret into the agent as
`DYNAMIC_CONFIG_AGENT_TOKEN`; nothing token-shaped appears in the pod
spec. A static token is also the one method that cannot recover on its
own — when it expires or is revoked, the fetch fails until the Secret
is updated. The two methods below fix that.

## Kubernetes: login with the pod's identity

Consul's auth methods issue a token in exchange for a bearer the method
trusts — for the `kubernetes` type, the pod's own service-account JWT.
Nothing is distributed; the token is minted per login.

Consul side, once (the [Consul docs on auth
methods](https://developer.hashicorp.com/consul/docs/security/acl/auth-methods/kubernetes)
carry the full story):

```sh
consul acl auth-method create -type kubernetes -name k8s-pods \
  -kubernetes-host https://kubernetes.default.svc \
  -kubernetes-ca-cert @/path/to/cluster-ca.crt \
  -kubernetes-service-account-jwt "$REVIEWER_JWT"

consul acl binding-rule create -method k8s-pods \
  -bind-type role -bind-name 'config-reader' \
  -selector 'serviceaccount.namespace==default'
```

Pod side, two annotations — `auth-mount` carries the auth method's
**name**, because that is the coordinate Consul's login endpoint wants:

```yaml
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "consul"
    dynamic-config.rs/endpoint: "http://consul.infra.svc:8500"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    dynamic-config.rs/auth: "kubernetes"
    dynamic-config.rs/auth-mount: "k8s-pods"
```

The JWT is read from disk at every login rather than once: the kubelet
rotates projected service-account tokens, and a copy taken at startup
expires with the pod still running. If a projected volume moved the
token off the conventional path, say where:

```yaml
    dynamic-config.rs/auth-token-path: "/var/run/secrets/tokens/consul"
```

## JWT: any bearer the method trusts

The same login endpoint, with the bearer supplied instead of read from
the service-account mount — an OIDC id token, a JWT signed by something
Consul trusts. The bearer is a secret, so it rides a Secret:

```yaml
    dynamic-config.rs/auth: "jwt"
    dynamic-config.rs/auth-mount: "oidc-ci"
    dynamic-config.rs/token-secret: "ci-idtoken/jwt"
```

## TLS

An internal Consul serving from a private PKI needs its CA trusted;
[one ConfigMap, one annotation](secrets-and-tls.md#a-private-ca-end-to-end):

```yaml
    dynamic-config.rs/endpoint: "https://consul.infra.svc:8501"
    dynamic-config.rs/ca-configmap: "consul-ca"
```

A cluster fronting Consul with mTLS adds
[`tls-secret`](secrets-and-tls.md#a-client-certificate).

## When it fails

| symptom | look at | usual cause |
|---|---|---|
| agent log: `403` | `consul acl token read -self` with the same token | the token lacks `key_prefix` read on the path |
| agent log: login refused | `consul acl auth-method read -name k8s-pods` | binding rule selector does not match the pod's namespace/SA |
| agent starts, file never updates | `consul kv get myapp/config.json` | the key moved, or the watch interval is long — check `watch-seconds` |

The agent keeps the last good render on any fetch failure — the
[rendering page](rendering.md) spells out that guarantee.

Beyond the agent's flags, the consul crate can also scope reads to a
datacenter and tune blocking-query waits; those knobs live on the
[store crate itself](https://dynamic-config-rs.github.io/remote/) for
applications embedding the engine directly.
