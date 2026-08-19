# etcd

A key (or several) from etcd v3. etcd is the honest hard case for
authentication: it has no Kubernetes auth method to log into — nothing
that takes a projected service-account token the way Vault's
`kubernetes` mount does. So this store lands with the two methods etcd
itself speaks, **both first-class**: TLS client certificates, and
username/password. Identity-first is the org's policy where a store
can authenticate the pod; where it cannot, the secret-based method is
supported without apology — a contract that punishes such stores
punishes their users.

There is no `auth` annotation for etcd: the flags present ARE the
method.

## TLS client certificates

The method etcd operators already deploy — the certificate is the
credential, delivered as a Secret the same way every store's TLS
material is ([the geography page](secrets-and-tls.md)):

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: billing
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "etcd"
    dynamic-config.rs/endpoint: "https://etcd.infra.svc:2379"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    dynamic-config.rs/tls-secret: "etcd-client-tls"
    dynamic-config.rs/ca-configmap: "etcd-ca"
spec:
  containers:
    - name: app
      image: myapp:1
```

```sh
kubectl create secret tls etcd-client-tls \
  --cert=./billing.crt --key=./billing.key
kubectl create configmap etcd-ca --from-file=ca.crt=./etcd-ca.pem
```

## Username and password

etcd's other method. The user rides an annotation; the password rides
a Secret, never an annotation — `kubectl describe pod` prints
annotations to anyone with pod read access:

```yaml
    dynamic-config.rs/source: "etcd"
    dynamic-config.rs/endpoint: "https://etcd.infra.svc:2379"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/auth-username: "myapp"
    dynamic-config.rs/password-secret: "etcd-password/password"
    dynamic-config.rs/ca-configmap: "etcd-ca"
```

Both together is also legal, and is etcd's own combination: the
certificate authenticates the channel, the user authenticates the
principal.

## Several keys

`key` takes a comma-free single key; several documents merging into
one is the [config server](config-server.md)'s job or a template's.
The stored format is the key's extension, exactly as with
[Consul](consul.md).

Ready-to-apply manifests: [`examples/etcd-tls.yaml`](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/etcd-tls.yaml),
[`examples/etcd-password.yaml`](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/etcd-password.yaml).
