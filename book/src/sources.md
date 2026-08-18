# The Six, and the Three Waiting

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
in the repository's `examples/` directory — thirteen files, each
self-contained with its Secret placeholders.

## etcd, NATS, S3 — 0.2.0

The other three store crates exist and work — from the engine, from the
bindings — but their clients are async, and the 0.1 agent drives the
blocking `RemoteSource` trait. The agent refuses them by name today:

```text
--source etcd lands in 0.2.0 (its client is async); consul, vault,
config-server, firestore, git and redis are the 0.1 stores
```

Until then, the pattern that works today: put a
[config server](config-server.md) in front. The server side speaks all
nine stores including the async three, and the agent speaks the server.
That indirection is also the answer when a fleet of pods should not
each hold store credentials — the server holds them once.
