# Redis

A key read as a document, watched by polling. The key carries its
format in its extension (`myapp/config.json`); a key without one needs
the document format to be guessable, so give it one.

Redis is the one store whose credentials travel **in the url** —
`requirepass` and ACL users have no other place. That shapes the whole
page: the moment the url grows a password, it stops being an annotation
and becomes a Secret.

## An open Redis

Development, a sidecar cache, a cluster-internal instance behind a
NetworkPolicy:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: billing
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "redis"
    dynamic-config.rs/endpoint: "redis://redis.infra.svc:6379/0"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
spec:
  containers:
    - name: app
      image: myapp:1
```

The trailing `/0` is the database index; omit it for `0`.

## requirepass

The password goes into the url, and the url goes into a Secret —
`endpoint-secret` replaces `endpoint` entirely, and the agent reads the
address from `DYNAMIC_CONFIG_AGENT_ENDPOINT`:

```sh
kubectl create secret generic redis-url \
  --from-literal=url='redis://:s3cr3t@redis.infra.svc:6379/0'
```

```yaml
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "redis"
    dynamic-config.rs/endpoint-secret: "redis-url/url"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
```

Setting both `endpoint` and `endpoint-secret` fails the admission —
one address, one place. Error messages redact the password even when
the url cannot be parsed, because a parse error is the error most
likely to be pasted somewhere.

## An ACL user

Redis 6+ ACLs put a username before the password. Server side:

```text
ACL SETUSER config-reader on >s3cr3t ~myapp/* +get resetchannels
```

The url names the user, read-only on exactly the config prefix:

```sh
kubectl create secret generic redis-url \
  --from-literal=url='redis://config-reader:s3cr3t@redis.infra.svc:6379/0'
```

## TLS

`rediss://` (two esses) plus the CA:

```yaml
    dynamic-config.rs/endpoint: "rediss://redis.infra.svc:6380/0"
    dynamic-config.rs/ca-configmap: "redis-ca"
```

A `redis://` url with TLS material is refused — a deployment that
believes it is encrypted and is not — and there is no way to turn
verification off. A client certificate is
[`tls-secret`](secrets-and-tls.md#a-client-certificate), as everywhere.

With a password too, the whole `rediss://user:pass@…` url rides
`endpoint-secret` and the CA annotation stays as it is.

## When it fails

| symptom | look at | usual cause |
|---|---|---|
| `NOAUTH` / `WRONGPASS` | `redis-cli -u <the url> get myapp/config.json` | the url in the Secret lost its password half, or the ACL user is off |
| `NOPERM` | `ACL GETUSER config-reader` | the key pattern does not cover the config key |
| refused: url is not rediss | — | TLS material with a `redis://` url; add the second `s` |
| empty render | `redis-cli … type myapp/config.json` | the key holds a hash, not a string document |
