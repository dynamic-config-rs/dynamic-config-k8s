# Config Server

This project's own server — the aggregation answer. The server side
speaks all nine store crates (including the async three the agent
cannot drive yet) and merges files, profiles and remote stores into one
document per `<application>/<profile>`; the agent then speaks the
server. The [remote book](https://dynamic-config-rs.github.io/remote/)
owns the server's full story; what follows is the k8s-side wiring.

Two reasons to put it between the pods and the stores:

- **Credential concentration.** A fleet of pods each holding a vault
  token is a fleet of tokens to rotate. The server holds the store
  credentials once; the pods hold short client tokens that grant
  exactly one application's sections.
- **The async stores today.** etcd, NATS and S3 reach the agent in
  0.2.0 — through the server they work now.

## The wiring

The key is `<application>/<profile>`:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: billing
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "config-server"
    dynamic-config.rs/endpoint: "http://config-server.infra.svc:8888"
    dynamic-config.rs/key: "billing/prod"
    dynamic-config.rs/path: "/config/rendered.json"
spec:
  containers:
    - name: app
      image: myapp:1
```

## The bearer token

The server's `[[server.clients]]` blocks name each client and the
applications it may read. The token is a secret; it rides a Secret:

```toml
# server.toml, server side
[[server.clients]]
name = "billing-pods"
token = "…at least 32 bytes…"
applications = ["billing"]
```

```sh
kubectl create secret generic config-server-token --from-literal=token=…
```

```yaml
    dynamic-config.rs/token-secret: "config-server-token/token"
```

There is no other method on this store — one bearer, scoped
server-side, is the whole model. Asking for `auth:` anything is refused
by the agent with a sentence saying exactly that.

## TLS

The server behind TLS from an internal PKI is the same one annotation
as everywhere:

```yaml
    dynamic-config.rs/endpoint: "https://config-server.infra.svc:8443"
    dynamic-config.rs/ca-configmap: "internal-ca"
```

A server requiring client certificates takes
[`tls-secret`](secrets-and-tls.md#a-client-certificate) alongside.

## When it fails

| symptom | look at | usual cause |
|---|---|---|
| `401` | server log | the token is not in any `[[server.clients]]` block |
| `403` | server log, `applications = […]` | the client's list does not include this application |
| `404` | `curl $SERVER/billing/prod` with the token | no `[[server.sections]]` matches the pair |
| stale values | the server's own watch config | the server polls its stores on its own cadence — two intervals stack |
