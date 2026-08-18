# Roadmap

Where this repository goes after 0.1.0, in the order the dependencies
force. The organisation-wide milestones live in the `.github`
repository; this file is the k8s integration's own ladder. Dates are
deliberately absent — each rung ships when its gate is green, and the
gates are written down here so "done" is checkable.

## 0.1.x — the release machinery itself

The repository builds and smokes; what it does not yet do is *ship*.

- **`release.yml`**: merging a version bump into `main` builds the three
  images multi-arch (buildx, distroless, nonroot), pushes to
  **ghcr.io and Docker Hub** with the same digests, signs with cosign,
  attaches SBOMs, and mints the tag — the wheel wave's shape, aimed at
  registries. `DOCKERHUB_TOKEN` and `DOCKERHUB_USERNAME` are already in
  place.
- **The chart to ArtifactHub**: `helm push` to
  `oci://ghcr.io/dynamic-config-rs/charts` plus the metadata push
  documented in `deploy/helm/artifacthub-repo.yml`, then the claim.
  `Chart.yaml` already carries the annotations and the values schema.
- Gate: a release lands from one merge; `cosign verify` passes against
  both registries; the chart page renders on ArtifactHub with CRDs,
  images and links intact.

## 0.2.0 — the async stores, and identity where none exists

- **etcd, NATS, S3** join `--source` through an async agent path (their
  clients are async; the 0.1 agent drives the blocking trait). The
  admission-time and startup-time refusals already name this version.
- **etcd is the honest hard case for authentication.** It has no
  Kubernetes auth method to log into — nothing that takes a projected
  service-account token the way Vault's `kubernetes` mount or Consul's
  login endpoint does. So etcd lands with the two methods etcd itself
  speaks, both first-class:
  - **TLS client certificates** (`tls-secret`, the machinery exists) —
    the method etcd operators already deploy;
  - **username/password** via `token-secret`/`password-secret` —
    secret-based, supported deliberately and forever: not every store
    will ever speak workload identity, and a contract that punishes
    those stores punishes their users.
- **Identity-first, stated as a policy**: wherever a store can
  authenticate the *pod* instead of a distributed secret, that method
  is the documented default and the examples lead with it — as they
  already do for Vault (`kubernetes`), Consul (login) and Firestore
  (Workload Identity). Secret-based auth stays a supported peer, never
  a deprecated one; the security page's "where each thing is allowed
  to appear" rules cover both identically.
- Gate: the e2e matrix gains an etcd leg exercising both auth methods;
  the annotation reference's source table loses three "0.2.0" rows.

## 0.3.0 — self-rotating webhook TLS, the vault-agent-injector way

Today's two TLS modes trade against each other: chart-minted needs no
dependency but never rotates; cert-manager rotates but is a
dependency. The vault-agent-injector carries the third mode this rung
adds — **the webhook rotates itself**:

- On start, generate a CA and a leaf **in memory**; serve the leaf
  (the hot-reload plumbing in `tls.rs` already handles replacement);
  re-generate well before expiry on a jittered schedule.
- Patch the `caBundle` of **its own MutatingWebhookConfiguration, by
  name** — which costs this integration a deliberate purity: the
  webhook today holds no API credential at all, and in `selfRotate`
  mode it gains exactly one narrow one (get/patch on that single MWC,
  scoped by `resourceNames`). The chart states the trade where it
  states `failurePolicy`'s; the default mode does not change.
- Leader election among the replicas for the rotation itself (leases —
  the RBAC shape already drafted for the operator), so two webhooks do
  not fight over the bundle.
- Gate: a soak that rotates hourly for a day under admission load, with
  zero refused handshakes across rotations; the existing modes'
  golden behaviour untouched.

## 0.3.x — the operator's reconcilers

The CRDs are settled, generated and drift-gated; `DynamicConfigClass`
(store-plus-auth bundles pods reference instead of repeating endpoints)
and `DynamicConfigRender` (documents reconciled into ConfigMaps for
sidecar-averse workloads) get their loops. The book's operator page is
the contract, written before the code on purpose.

## Alongside, when their moment comes

- **Webhook metrics grow up**: histogram of admission latency, a
  ServiceMonitor toggle in the chart.
- **Template functions**, demand-driven: `b64`, `indent`, a JSON
  emitter — each addition argued in the book's Rendering page, never
  smuggled.
- **The macOS setLogger investigation** (node repository, recorded in
  the organisation's OUTSTANDING) — not this repository's, but the
  sibling entry most likely to teach this one something about
  FSEvents-adjacent silence.
