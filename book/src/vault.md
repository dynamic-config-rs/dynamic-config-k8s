# Vault

KV v2, and the widest auth surface of the six — all seven of the vault
crate's methods work through annotations. Ordered from development to
production; if the pod runs on Kubernetes (it does — you are reading
the k8s book), start with [kubernetes](#kubernetes-the-pods-own-identity).

The key is `<mount>/<path>`, the way vault CLI users write it:
`secret/myapp` is the `myapp` path on the `secret` KV mount. What the
agent renders is the secret's data — the fields under `data.data` in
vault's own JSON.

## A token

The method every tutorial starts with, and the only one that cannot
recover on its own: there are no credentials behind it to log in again
with. A renewable token is still renewed; a revoked one is the 3 a.m.
page.

```sh
vault policy write myapp-read - <<'HCL'
path "secret/data/myapp" { capabilities = ["read"] }
HCL
vault token create -policy=myapp-read -ttl=768h -format=json \
  | jq -r .auth.client_token \
  | xargs -I{} kubectl create secret generic vault-token --from-literal=token={}
```

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: billing
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "vault"
    dynamic-config.rs/endpoint: "https://vault.vault.svc:8200"
    dynamic-config.rs/key: "secret/myapp"
    dynamic-config.rs/path: "/config/rendered.yaml"
    dynamic-config.rs/token-secret: "vault-token/token"
    dynamic-config.rs/ca-configmap: "vault-ca"
spec:
  containers:
    - name: app
      image: myapp:1
```

## Kubernetes: the pod's own identity

No secret distributed anywhere: the agent presents the pod's
service-account JWT to vault's `kubernetes` auth method and gets a
token scoped to a role. This is the pod YAML the webhook's second
golden file locks:

```yaml
metadata:
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "vault"
    dynamic-config.rs/endpoint: "https://vault.vault.svc:8200"
    dynamic-config.rs/key: "secret/myapp"
    dynamic-config.rs/path: "/config/rendered.yaml"
    dynamic-config.rs/auth: "kubernetes"
    dynamic-config.rs/auth-role: "myapp"
    dynamic-config.rs/ca-configmap: "vault-ca"
```

Vault side, once:

```sh
vault auth enable kubernetes

vault write auth/kubernetes/config \
  kubernetes_host=https://kubernetes.default.svc

vault write auth/kubernetes/role/myapp \
  bound_service_account_names=billing \
  bound_service_account_namespaces=default \
  policies=myapp-read ttl=1h
```

Three details that bite:

- **The JWT is re-read from disk at every login**, not cached: the
  kubelet rotates projected tokens, and a copy taken at startup expires
  with the pod still running. Nothing to configure — stated here so a
  security review can check the box.
- **A projected token with a custom audience** lives off the
  conventional path; point at it:

  ```yaml
      dynamic-config.rs/auth-token-path: "/var/run/secrets/tokens/vault"
  ```

- **A second `kubernetes` mount** (multi-cluster Vaults mount one per
  cluster) is `auth-mount`:

  ```yaml
      dynamic-config.rs/auth-mount: "kubernetes-prod-eu"
  ```

## AppRole

The usual choice for a service *outside* Kubernetes, and for shops that
want an identity Vault owns rather than the cluster. Two halves: the
role id is public and rides an annotation; the secret id is a secret
and rides a Secret.

```sh
vault auth enable approle
vault write auth/approle/role/myapp policies=myapp-read \
  secret_id_ttl=90d token_ttl=1h

vault read -field=role_id auth/approle/role/myapp/role-id
vault write -f -field=secret_id auth/approle/role/myapp/secret-id \
  | xargs -I{} kubectl create secret generic vault-approle --from-literal=secret-id={}
```

```yaml
    dynamic-config.rs/auth: "approle"
    dynamic-config.rs/auth-role: "<the role id>"
    dynamic-config.rs/password-secret: "vault-approle/secret-id"
```

The secret id travels as `DYNAMIC_CONFIG_AGENT_PASSWORD` — the second
secret slot, which has no flag equivalent on purpose.

## JWT / OIDC

Any JWT a `jwt` mount trusts — a CI job's id token, a workload identity
from another platform. The JWT itself is the credential, so it rides
the token Secret:

```yaml
    dynamic-config.rs/auth: "jwt"
    dynamic-config.rs/token-secret: "workload-jwt/jwt"
    dynamic-config.rs/auth-role: "myapp"      # when the mount has no default
    dynamic-config.rs/auth-mount: "jwt-ci"    # when not mounted at "jwt"
```

## Userpass and LDAP

Same shape, different directory. The username is a name and rides an
annotation; the password rides a Secret:

```sh
kubectl create secret generic vault-ldap --from-literal=password=…
```

```yaml
    dynamic-config.rs/auth: "ldap"            # or "userpass"
    dynamic-config.rs/auth-username: "svc-billing"
    dynamic-config.rs/password-secret: "vault-ldap/password"
```

## Cert: a TLS client certificate

The certificate IS the credential: vault's `cert` method authenticates
the TLS handshake itself. One `kubernetes.io/tls` Secret carries the
pair:

```sh
kubectl create secret tls vault-client --cert=client.pem --key=client-key.pem
```

```yaml
    dynamic-config.rs/auth: "cert"
    dynamic-config.rs/tls-secret: "vault-client"
    dynamic-config.rs/auth-role: "myapp"      # when the mount does not pick by subject
    dynamic-config.rs/ca-configmap: "vault-ca"
```

Vault side:

```sh
vault auth enable cert
vault write auth/cert/certs/myapp certificate=@client-ca.pem \
  allowed_common_names=billing.default policies=myapp-read
```

## Vault Enterprise namespaces

One annotation, passed through to the `X-Vault-Namespace` header:

```yaml
    dynamic-config.rs/namespace: "team-payments"
```

## How the token lifecycle behaves

Worth knowing before the first incident, whatever the method:

- Close to expiry, the token is **renewed — or, if it cannot be,
  replaced by a fresh login**. This is the path that normally fires.
- After a request, a `403` is treated as *the token stopped working*
  and triggers **exactly one** fresh login and retry. Clocks skew,
  Vault revokes, a lease is shorter than it said.
- If a fresh token also gets `403`, the problem is the policy rather
  than the lease, and the error says so instead of hanging in a retry
  loop.

## Dynamic secrets

A path under `database/`, `pki/` or `aws/` does not *hold* a secret; it
**mints** one, with a lease. One annotation says to read it that way:

```yaml
    dynamic-config.rs/source: "vault"
    dynamic-config.rs/dynamic: "true"
    dynamic-config.rs/key: "database/creds/billing"
    dynamic-config.rs/path: "/config/db.env"
    dynamic-config.rs/auth: "kubernetes"
    dynamic-config.rs/auth-role: "billing"
```

The pod now holds a database credential nobody else has, for as long as it
needs it. What changes:

- **The path is read as it is.** KV v2's `data/` nesting is not inserted
  and `data.data` is not unwrapped, because a dynamic engine has neither.
- **The lease is kept.** `lease_id`, `lease_duration` and `renewable` come
  back with the document.
- **A lease that says it cannot be renewed is never sent a renewal.**
  Vault answers `renewable: false` for every `pki/issue`, and for a
  database credential that has reached its role's maximum. Such a lease is
  **re-issued at 90% of its life** instead. Asking it to renew first would
  be a request that can only be refused — a round trip per cycle per pod,
  and a `lease_renewal_failures_total` that climbs steadily on a fleet
  where nothing is wrong.
- **A renewable lease is renewed at 65% of its TTL**, spread, and what
  Vault *grants* is what the next renewal is scheduled from — a role's
  `max_ttl` is a ceiling the pod cannot see, so asking for an hour and
  being given ten minutes has to be believed rather than assumed.
- **The two fractions are not the same number by accident.** A renewal is
  cheap and reversible: it fails, and a third of the lease is still there
  to get a new credential in. A re-issue *is* the new credential, so doing
  it early only shortens the one in use and wakes every application
  watching the file for it.
- **A renewal does not re-render.** It extends the same credential; the
  file is already correct. A re-issue mints new credentials and does
  re-render, and that is what happens both when a renewal stops working
  and when the lease was never renewable.
- **A backoff never sleeps past the lease.** This is the one place the
  usual ceiling is wrong: waiting five minutes to retry a credential that
  expires in twenty seconds is a pod that comes back to an expired secret.
- **SIGTERM revokes.** Best-effort, with a short deadline —
  `revoke-on-shutdown: "false"` opts out, for a lease something else is
  still using. Vault expires the lease on its own eventually; revoking on
  the way out is what turns *eventually* into *now*.

### Certificates are on their own clock

`pki/issue` is the one dynamic engine where the lease is not the whole
truth. A PKI role can hand back a lease longer than the certificate it
issued — the lease is Vault's accounting record, `notAfter` is what a TLS
peer enforces — so the agent takes **whichever runs out first**.

Vault reports the certificate's `notAfter` as `data.expiration`, seconds
since the epoch, which is why this needs no X.509 parsing: the number is
already in the response.

One guard comes with it. That timestamp is Vault's wall clock compared
against the pod's, and the two are not the same clock. A certificate that
looks *already* expired from inside the pod is far more likely to be skew
than an expired certificate — Vault would not have issued one — so the
lease's own number wins there, rather than the agent re-issuing in a tight
loop against a server that thinks everything is fine.

The watch capability drops to `Interval`: a dynamic engine has no version
to poll, and asking whether it changed is not separable from asking for a
new credential.

**One path per render.** Every read mints its own lease, so merging
several paths into one document would leave every lease but one unrenewed
and unrevoked. A second dynamic path is a second named render.

The four `lease_*` series in
[the agent's metrics](observability.md#the-agents-metrics) are what to
watch: `lease_ttl_seconds` is what Vault last granted, and a rising
`lease_renewal_failures_total` is the signal that a re-fetch is coming.

## When it fails

| symptom | look at | usual cause |
|---|---|---|
| `403` on every read | `vault token capabilities <token> secret/data/myapp` | the policy grants `secret/myapp`, not `secret/data/myapp` — KV v2 inserts `data/` |
| login refused (kubernetes) | `vault write auth/kubernetes/login role=myapp jwt=@/tmp/jwt` by hand | role's bound SA/namespace does not match the pod's |
| x509 errors | `openssl s_client -connect vault:8200` | the CA in `ca-configmap` is not the one vault serves |
| works, then `403` after days | vault audit log | orphan token hit its max TTL; move to kubernetes/approle auth |
