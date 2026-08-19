# NATS

A JetStream key-value bucket. The `key` annotation is
`<bucket>/<key>` — the bucket must already exist; a configuration
reader that provisions storage would hide a misconfigured deployment
behind an empty one.

## A credentials file

The way a NATS account authenticates — a `.creds` file, delivered as a
Secret and named by path:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: billing
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "nats"
    dynamic-config.rs/endpoint: "nats://nats.infra.svc:4222"
    dynamic-config.rs/key: "config/db.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    dynamic-config.rs/auth-token-path: "/etc/nats/nats.creds"
spec:
  containers:
    - name: app
      image: myapp:1
      volumeMounts:
        - { name: nats-creds, mountPath: /etc/nats, readOnly: true }
  volumes:
    - name: nats-creds
      secret: { secretName: nats-creds }
```

## A token

`token-secret`, the same one annotation every token-shaped credential
rides:

```yaml
    dynamic-config.rs/source: "nats"
    dynamic-config.rs/endpoint: "nats://nats.infra.svc:4222"
    dynamic-config.rs/key: "config/db.json"
    dynamic-config.rs/token-secret: "nats-token/token"
```

Anonymous — no auth annotation at all — is correct for a NATS without
auth, ordinary in development.

Ready-to-apply manifest: [`examples/nats-creds.yaml`](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/nats-creds.yaml).
