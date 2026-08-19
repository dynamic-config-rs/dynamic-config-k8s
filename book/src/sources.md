# The Nine Stores

One page per store the agent speaks, each with the full pod YAML for
**every** authentication method the store takes — copy, adjust names,
apply. Everything is runnable; the consul flow is what the e2e harness
runs on every pull request.

| store | speaks | auth methods | its page |
|---|---|---|---|
| Consul | KV over HTTP | anonymous, token, kubernetes (login), jwt | [Consul](consul.md) |
| Vault | KV v2 | token, kubernetes, approle, jwt, userpass, ldap, cert | [Vault](vault.md) |
| Config Server | this project's own server | bearer token | [Config Server](config-server.md) |
| Firestore | Google Cloud | metadata-server (Workload Identity), access-token, emulator | [Firestore](firestore.md) |
| Git | any git host | anonymous, token, ssh key | [Git](git.md) |
| Redis | RESP | in the url (`requirepass`, ACL users) | [Redis](redis.md) |

Common to all six:

- The store's **address** rides `dynamic-config.rs/endpoint` — or
  [`endpoint-secret`](secrets-and-tls.md) when the address itself
  carries a password.
- The **document's key** rides `dynamic-config.rs/key`; the per-store
  syntax (`mount/path`, `application/profile`, a file path) is on the
  store's page.
- **Secret material rides Secrets**, never annotations; the
  [geography page](secrets-and-tls.md) has the one diagram.
- A **private CA** is the same one annotation everywhere:
  `dynamic-config.rs/ca-configmap`.

Every pairing on these pages also exists as a ready-to-apply manifest
in the repository's `examples/` directory — twenty-two manifests plus six real-software walkthroughs, each
self-contained with its Secret placeholders.

## etcd, NATS, S3 — the async three

Since 0.1.1 the agent drives both of the engine's source traits: the
blocking six run under a blocking task, and [etcd](etcd.md),
[NATS](nats.md) and [S3](s3.md) — whose clients are async — are driven
directly by the agent's own runtime. The 0.1 refusal-by-name retired
with this.

The [config server](config-server.md) indirection remains the answer
to a different question: a fleet of pods that should not each hold
store credentials — the server holds them once.

A tenth store — GCP Secret Manager, Azure App Configuration — is a
compile-time addition with a well-worn path:
[Adding a Store](adding-a-store.md) walks it end to end, worked example
included, plus the two no-code compositions that cover the meantime.
