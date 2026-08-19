# Adding a Store

Can the agent learn a tenth store — Google Cloud Secret Manager, say?
Yes, and what follows is the map. What it is NOT is a plugin system:
stores are compiled in on purpose (a config agent that loads plugins is
its own supply-chain problem), and the organisation's standing rule is
**stores land by demand, not by roadmap** — the ask starts as an issue,
and the walkthrough below is what the work looks like once agreed.

## What a store is

One Rust type implementing one of the engine's two traits:

```rust,ignore
impl AsyncRemoteSource for GcpSecretManager {
    fn fetch(&self) -> BoxFuture<'_, Result<Fetched, Error>>;  // text + format
    fn describe(&self) -> String;                              // REDACTED — errors quote it
}
```

`fetch` answers the document's text and format; `describe` names the
store in errors and logs and must never carry a credential. Blocking
clients implement `RemoteSource` instead; the agent drives both — the
blocking six run under a blocking task, the async three (etcd, NATS,
S3) on the runtime directly. A new store picks whichever its client
dialect is.

## The walkthrough, with GCP Secret Manager as the worked example

The 0.1.1 change that landed etcd, NATS and S3 is the living template —
`git log --grep "async three"` in the two repositories shows every
touchpoint below with real diffs.

**1. The store crate** (`dynamic-config-remote` repository,
`dynamic-config-gcpsm/`). The Firestore crate is the closest twin:
Google auth, same project-scoped shape. The heart is small:

```rust,ignore
pub struct GcpSecretManager { project: String, secret: String, auth: Auth, /* … */ }

impl AsyncRemoteSource for GcpSecretManager {
    fn fetch(&self) -> BoxFuture<'_, Result<Fetched, Error>> {
        Box::pin(async move {
            // GET https://secretmanager.googleapis.com/v1/projects/{p}
            //     /secrets/{s}/versions/latest:access
            // Authorization: Bearer <metadata-server token>
            let text = self.access_latest().await?;

            Ok(Fetched { text, format: self.format })
        })
    }

    fn describe(&self) -> String {
        format!("gcp-sm {}/{}", self.project, self.secret) // no token, ever
    }
}
```

Identity-first applies: the default auth is the **metadata server**
(Workload Identity — the pod's own identity, no distributed secret),
exactly as the Firestore crate already does; an `access-token` arm
stays a supported peer. House rules the crate must keep: `describe`
redacted, TLS through the shared `TlsConfig` vocabulary, errors mapped
to the engine's kinds (`auth` for 401/403, `remote` for the rest),
`#![forbid(unsafe_code)]`.

**2. The agent** (`dynamic-config-agent/src/`):

- `sources.rs` — one async constructor arm beside `etcd`/`nats`/`s3`,
  returning `Built::Async(Arc::new(store))`.
- `spec.rs` — the source joins the `validated()` list, its auth rules
  join `validated_auth()` (refusals name the fix; that is the house
  voice), and `USAGE` learns the flags. Tests beside the etcd ones.

**3. The webhook** (`dynamic-config-webhook/src/annotations.rs`) — the
source name joins the admission allowlist, and its auth annotations
join the validation match so a typo is refused **at admission**, in
the pod's events, not twenty minutes later in a crashloop. A golden
test pins the happy path.

**4. The paper trail** — a store page in this book (auth methods, full
manifests, the identity-first default stated), a ready-to-apply file
in `examples/`, a row in the annotation reference's source table, and
— for a store with a live server in CI — an e2e leg shaped like
`e2e/stores-smoke.sh` — one cluster, every live store, a stage per source.

**5. The gates** — `just check` in both repositories, the webhook
goldens, the kind smoke. Nothing about the release train changes: the
store crate ships from `dynamic-config-remote`, the agent picks it up
on the next k8s release.

## A store in another language

Three lanes, all of them already open:

- **In-process, Python**: subclass the binding's `RemoteSource` ABC —
  `fetch()` and `describe()` — and hand the instance to
  `DynamicConfig.remote(...)`. The
  [Python book](https://dynamic-config-rs.github.io/python/) owns the
  chapter.
- **In-process, Node**: any JS object with `fetch()` and `describe()`
  through `useStore(config, store)` — the
  [Node remote package](https://dynamic-config-rs.github.io/node/)'s
  contract.
- **Out-of-process, ANY language — the one the agent can use**: the
  agent cannot load Go or Java, but it speaks the
  [config server](config-server.md)'s small HTTP contract. Implement
  `GET /{application}/{profile}` (bearer-token auth, the resolved
  document as the body) in your language, and every consumer here —
  the agent's `--source config-server`, the engine, both bindings —
  reads it like any other store. A Go service fronting your in-house
  store is an afternoon: one endpoint, one auth header, and TLS. The
  [remote book's server chapter](https://dynamic-config-rs.github.io/remote/config-server.html)
  tables the full route surface, `/stream` included for push.

## Today, without code

Two honest compositions cover most "we need store X now" cases:

- **External Secrets Operator in front**: ESO syncs GCP SM (or any of
  its many backends) into a Kubernetes Secret, and everything on
  [The Three Deliveries](injection-shapes.md) consumes that Secret —
  mounted as a live file, `envFrom`, or `secretKeyRef`. You trade the
  document model for breadth, which is exactly the trade the
  [comparison table](injection-shapes.md#against-external-secrets-operator) prices.
- **A mirror job**: a CronJob copies the document from the unsupported
  store into one the agent speaks (S3, git, Consul). Crude, visible,
  and often all a migration window needs.
