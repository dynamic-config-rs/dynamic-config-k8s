# Changelog

All notable changes to the `dynamic-config-k8s` components are documented
here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

The three components version together and ship as images, not crates.
Pre-1.0, a breaking change to the **annotation contract** bumps the minor
version — the contract is the API here.

## [Unreleased]

## [0.2.0] — 2026-08-21

### Fixed

- **`agent.defaults.perStore` could not be set as a string at all.** The
  chart's default for it was an empty *map*, and helm refuses to overwrite
  a table with a non-table — so the grammar spelling the values reference
  documents was rejected by the values file that documented it. The
  default is an empty string now, which takes either: a map overwrites a
  string happily, and the reverse is what was impossible.

- **The webhook injected the previous release's agent** wherever
  `DYNAMIC_CONFIG_AGENT_IMAGE` was not set — the embedded fallback still
  named `v0.1.1`, which is the supported way to run the webhook as a bare
  binary. It moves with the release now, and the golden fixtures say so.

- **A pod with an unrelated `dynamic-config-init` container was refused a
  sidecar**, and the reverse in init mode: the collision check reserved
  both container names whatever the mode asked for, so an admission failed
  over a name this injection was never going to take.

### Changed

- **The engine and store floors are 0.9**, and `serde` / `serde_json`
  move to `1.0.228` / `1.0.149` behind them — the floors the engine's
  own fold requires. Neither moves the MSRV, and nothing in the
  annotation contract changes.

- **The sidecar watches instead of polling.** The engine's 0.9 carries
  a watch contract and every store answers it, so the agent uses the
  mechanism each store actually has: a change in etcd, Consul, NATS,
  Redis or a config server arrives as it happens rather than up to one
  interval later, and a store that must be asked is asked the cheapest
  question it offers — an S3 object is a `HEAD` rather than a
  download every interval.

  `--watch <seconds>` keeps its spelling and gains a second meaning:
  the poll period for a store that must be asked, and the **resync**
  period for one that pushes. The resync is not belt and braces. The
  failure mode of a stream is silence — a subscription the broker
  forgot, a connection that dropped without an error — and it looks
  exactly like a store where nothing has changed.

  A watch that ends is reopened, waited out with a spread, backing-off
  pace rather than a tight loop against a store that is down.

  `examples/watch-driven.yaml` is a pod on that path, with the resync
  and the metrics port set and the gauge to alert on named.

- **The operator elects a leader.** Replicas contend for a Lease and
  only the holder reconciles; a leader that dies is replaced within the
  lease's fifteen-second term. The ClusterRole has granted `leases`
  since 0.1.0 and nothing used them, so two replicas both reconciled —
  which on a bad day is two different documents landing in whichever
  order the API server saw them. `operator.replicas` is a chart value,
  with a PodDisruptionBudget that is applied only above one replica.

- **A failed reconcile backs off.** Five seconds doubling to five
  minutes, per object, spread — where it was a flat thirty seconds for
  as long as a store stayed down, times every Render pointing at it.
  A successful requeue is spread too: a hundred Renders created by one
  `kubectl apply` used to come back to the store together, forever.

### Fixed

- **Admitting a pod twice injected the agent twice.** A mutating
  webhook is not called once: `reinvocationPolicy: IfNeeded` asks the
  API server to call it again whenever a later webhook changes the pod,
  and some controllers resubmit a spec that has already been admitted.
  The second pass added the agent again — two containers with one name,
  which the API server refuses, so the pod never started and the error a
  user saw was about a spec they did not write. The patch now marks the
  pod (`dynamic-config.rs/status: injected`) and a marked pod is passed
  through untouched. Found by reading the vault-agent-injector, which
  has carried the same guard since it shipped.

- **A pod that already used one of the injected container names was
  patched into an invalid pod.** Same failure, from the other
  direction. It is refused now, with the name to rename, and the
  refusal carries its own `conflict` reason so it aggregates apart from
  the other three.

- **A minted certificate says how long it is good for** (security). The
  `selfRotate` mode wrote the rotation schedule into an annotation it
  read back, and left the certificate's own validity to the library
  default — years. A pair replaced every day but valid for four years
  is not a short-lived credential; it is a long-lived one that happens
  to be replaced often, and a copy taken from a node stayed good long
  after the rotation meant to retire it. Both the CA and the leaf now
  carry explicit `notBefore`/`notAfter`, five minutes back for clock
  skew.

  **The CA outlives its leaf, by one full interval.** The caBundle
  carries the new CA and the previous one so that leaves already
  serving keep verifying while the kubelet catches up — and a CA that
  expired with its leaf would have left that window trusting an
  authority that was no longer valid, breaking the transition at
  exactly the moment it exists to cover.

- **The rotation lease is renewed while a rotation runs.** The term is
  thirty seconds and a rotation is a mint plus two API patches — fast
  until the API server is not, at which point a second replica could
  take the lease and rotate on top of the first.

- **The webhook stops building a Kubernetes client every fifteen
  seconds.** One client for the life of the process; the old one
  re-read the service account token, rebuilt its TLS configuration and
  opened a new connection four times a minute, for a loop whose usual
  answer is "not yet".

- **The operator no longer parks its reactor on a network call.** A
  Class edit re-renders everything referencing it, and working out what
  that is meant listing the API from inside a synchronous mapper —
  `block_on`, once per class event, with every other reconcile waiting
  behind it. The controller's own reflector already holds those
  objects; reading it is a lock and a filter.

### Added

- **The installation can be written as YAML instead of as a grammar.**
  Everything an installation sets reaches the webhook as a string,
  because that is what an environment variable is — and several of
  those strings were little grammars
  (`"vault: overridable=false, endpoint=…; s3: file-mode=0640?"`). Fine
  to parse, unpleasant to write.

  `agent.defaults.perStore` and the three gates now take a **map** as
  well as a string. A map travels to the pod as a mounted ConfigMap and
  is rendered to the grammar *there*, by the same parser the string
  goes through — so there is one set of rules, one set of messages, and
  two spellings that cannot mean different things. An unknown setting
  is refused at startup rather than ignored, and a variable set on the
  container still wins over the document.

  **Kustomize gets the same thing**, which is why the document exists
  rather than a chart-side rendering: a base has no template engine, so
  a hand-written ConfigMap is the only structured form it can hand
  over. `deploy/kustomize/base/installation.yaml` ships empty and
  commented.

  The webhook reads it through its own engine — the YAML reader it
  gives applications is the one it reads its own configuration with.

- **Metrics the agent's failure modes are visible in**: deliveries and
  resyncs told apart (deliveries flat while resyncs climb *is* the
  stalled stream), `watch_connected`, reconnects, and a
  **staleness gauge** — seconds since the store was last read
  successfully, which is the number an alert fires on. Operator-side, a
  reconcile duration histogram and a failure counter.

- **Probes.** `/healthz` and `/readyz` on the port that was already
  serving metrics, so a deployment does not have to write an `exec`
  probe that shells out — and the operator's chart wires both.

- **The webhook's readiness means what it says.** `/readyz` is split
  from `/healthz` and answers 503 until a certificate this process can
  serve with is loaded. In `selfRotate` mode that is false for the
  first minute, and a replica reporting Ready without one is a replica
  the Service sends admissions to that cannot finish a handshake —
  which the API server reports as every pod creation failing.

- **Admission latency, as a histogram.** The webhook sits in the path
  of every pod creation in the cluster, and the API server's own
  ten-second timeout turns a slow admission into a refused one — so
  the question is what the tail does, which only a histogram answers.
  Beside it: patch bytes written, and refusals **labelled by kind** —
  a store the installation does not allow, a pinned value being
  overridden, and a malformed annotation are three different pages.
  The kind is a `status.reason` the refusal carries, not something a
  scrape works out by reading the English.

- **Metrics on a port of their own** (`webhook.metrics`, on by
  default), with an opt-in ServiceMonitor. They were served over the
  admission port, which is mutual TLS against a CA the webhook mints
  for itself — so scraping them meant handing Prometheus a client
  certificate from that CA, and a deployment that did not simply had
  no metrics. The admission port still answers `/metrics`, so an
  existing scrape keeps working.

- **Rotation is visible**: a rotation counter and an expiry gauge. The
  pair a scrape wants — a counter that should climb on a schedule, and
  a wall-clock second that should always be in the future.

### CI

- **One Dockerfile, one dependency build, three images.** The three
  were byte-identical files that each did `COPY . .` into a fresh
  `cargo build`, so every image compiled the whole workspace's
  dependencies from nothing. `cargo-chef` cooks the dependency graph on
  a layer of its own and the three binaries share it, a `.dockerignore`
  keeps `target/` out of the build context, and the nightly e2e ladder
  builds the images **once per run** and hands the legs a tarball
  instead of rebuilding all three per leg. Release builds gain a buildx
  cache, where the arm64-under-QEMU half gains most.

### Added

- **The whole contract is installable, and any value can be pinned**:
  per-store defaults now take EVERY store-shaped annotation (endpoint,
  endpoint-secret, key, the credential Secrets, ca-configmap,
  tls-secret, ssh-secret, aws-secret, section, the auth flags,
  namespace, ref, api-url, the templates), and the fleet tier gains
  `source` and `path` — a pod can deploy carrying nothing but
  `inject: "true"`. Every installation value carries an override rule:
  a trailing `!` pins it (a DIFFERING annotation is refused at
  admission; the same value restated passes), `?` opens it,
  `overridable=false` inside a `perStore` group pins that one store's
  values, and `agent.defaults.overridable: "false"` flips the default
  for every value the installation set — never for knobs it left
  alone. The closest word wins: value marker, then store flag, then
  the installation's. Pins
  follow the either-or pairs: a pinned endpoint also refuses an
  endpoint-secret answer, so the address cannot be sidestepped.
- **Source gates, global and per namespace**: `webhook.sourceAllow`
  admits only the stores it names (empty = every store, so upgrades
  change nothing); `webhook.sourceDeny` turns stores off outright and
  outranks the allowlist. Judged against the pod's namespace on EVERY
  render — a denied store cannot ride in on a named suffix — and a
  gate entry that is not a real store fails webhook startup.
- **"Observability" book page**: the three Prometheus endpoints
  (webhook admissions by outcome, agent render counters and the
  staleness gauge, operator reconciles), alert rules for the gauge,
  PodMonitor and OTel-Collector scrape examples, the log contract,
  and an honest paragraph on why there is no OTel SDK inside.
- **"Installation Defaults and Gates" book page**: the tier model,
  every knob with its validation, a per-store example for all nine
  stores with every field filled, the three gates' semantics and
  threat model, and the chart-value-to-env-variable table kustomize
  installs work from.
- **Per-store defaults, and the full knob set fleet-wide**: defaults
  now come in tiers — annotation > per-store > fleet > built-in.
  `agent.defaults.perStore` sets any knob one store at a time
  (`vault: "watch-seconds=10, file-mode=0400"`), and the fleet tier
  grows `mode`, `volumeMedium`, `nativeSidecar`, `runAsUser`,
  `runAsGroup`, `metricsPort` (pods opt out with `metrics-port: "0"`)
  and `env` — installer-set environment on every agent, overridden
  name by name by a pod's own `agent-env`. One vocabulary, one set of
  validators, all of it checked at webhook startup; kustomize patches
  the same `DYNAMIC_CONFIG_AGENT_*` variables and walks through the
  same door.
- **`agent-env`, behind the installer's gate**: pods may set
  environment on their injected agents (`RUST_LOG`, proxy variables,
  `AWS_CA_BUNDLE`) — but only names the installation allowlists for
  that namespace pass admission. The gate is `webhook.agentEnvAllow`
  (`DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW` for kustomize):
  namespace-scoped groups, prefix globs, empty means refused
  everywhere. It is config, not a namespace annotation, because a gate
  its subject can open is not a gate — and reading namespaces would
  cost the webhook its zero-RBAC posture.
- **Fleet defaults for `file-mode` and `watch-seconds`**
  (`agent.defaults.fileMode` / `.watchSeconds`), joining the resource
  defaults — and EVERY fleet default is now validated when the webhook
  starts, so a mistyped octal stops the install instead of surfacing
  at the first admission. Helm's values schema refuses the same
  mistakes at render time; kustomize installs get the startup check.
- **selfRotate: the webhook is its own certificate authority.** The
  third TLS mode (`webhook.selfRotate.enabled`), the
  Vault-agent-injector shape: a CA and leaf minted **in memory** at
  rotation time, the pair written to the chart's own Secret — every
  replica serves it through the same file hot-reload cert-manager uses
  — and the webhook configuration's `caBundle` patched by the webhook
  itself. A fresh pair every 24h, leader-elected over a Lease,
  jittered ±10%. The price is stated in `values.yaml` beside the
  toggle: a service-account token and three narrow name-scoped
  permissions, the zero-RBAC purity of the other modes knowingly
  traded for rotation without a dependency. Gated by a rotation soak
  (`e2e/rotation-soak.sh`, nightly): admissions under
  `failurePolicy: Fail` across rotations, zero refused handshakes.
  The soak earned its keep on its first run: a single-CA `caBundle`
  refused one admission per rotation for the whole kubelet-sync window
  (the bundle flips instantly, the served leaf lags by up to a
  minute), so the bundle now carries a **two-CA window** — the new CA
  and the previous one — pruned at the next rotation.
- **The operator reconciles.** `DynamicConfigRender` → ConfigMap,
  through the SAME source construction and rendering the sidecar agent
  uses (the agent's machinery became a library): status
  (`renderedAt`/`lastError`, kind-and-shape-only), error requeue,
  Class watch wired so a Class edit re-renders everything referencing
  it, and cleanup by `ownerReferences` — Kubernetes' own garbage
  collector, no finalizer to get wrong. Vault classes are **refused**
  by the recorded honesty rule: a ConfigMap is not a Secret, and the
  `secret:` target that would lift the refusal does not exist yet.
  Gated end-to-end (`e2e/operator-smoke.sh`): apply → render →
  propagate → delete → collected.
- **CRD fields are camelCase** (`intervalSeconds`, `target.configMap`,
  `status.renderedAt`) — the Kubernetes convention, fixed in v1alpha1
  before the reconciler's first release could freeze the snake_case
  spelling the generator emitted.
- **Nightly workflow**: the rotation soak (5h) and the full e2e ladder
  (smoke, etcd, operator) against a fresh kind.

### Fixed

- **The version story told the truth late**: the async stores, the
  selfRotate TLS mode and the operator's reconcilers all shipped in
  0.1.1, but the README, the roadmap, the book's introduction,
  operator/sources/config-server/stability pages, the kustomize README
  refs and two comments still promised them for "0.2.0" and "0.3.0" —
  versions that never existed. Every reference now says 0.1.1, and the
  roadmap keeps only what is NOT yet built.
- **s3 with `api-url` no longer needs an ambient region**: an explicit
  endpoint means a non-AWS server (MinIO, Ceph, R2) — there is no IMDS
  to ask, so the SDK's region lookup timed out and left the client
  region-less, which it refuses. The agent now falls back to
  `us-east-1` (ignored by those servers) after the normal chain, so
  `AWS_REGION` still wins when set.
- **The injected agent could not read its own TLS client credential.**
  `tls-secret` mounted at `defaultMode: 0400` — root-owned by the
  kubelet, owner-read only — while the agent runs nonroot, so every
  client-certificate store failed with `Permission denied` at the first
  fetch. Found by the new etcd e2e leg's mTLS pod; the mount is 0444
  now (only the agent mounts that volume). The ssh key mount stays
  owner-only on purpose: ssh clients refuse a key anyone else can read.

- **`auth: "kubernetes"` for config-server** — the server indirection
  goes zero-secret: the agent presents the pod's projected
  service-account token as the bearer (re-read per fetch; it rotates)
  and the server's new TokenReview auth maps the identity to
  applications. The last place a developer still held a client token,
  closed.
- **`aws-secret`** — static S3 credentials as one annotation (the
  Secret's `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` keys onto the
  agent), for the S3-compatibles that are not AWS; refused on any
  other source. Closes the MinIO gap the book had hand-waved at.
- **CI covers every feasible live store, in ONE leg**: kind stands up
  etcd twice (password against one server, a REQUIRED client
  certificate against another — v3 leaves with proper EKUs, because
  Go's TLS is entitled to refuse a v1 or a serverAuth-only cert as a
  client), plus vault, redis, NATS and MinIO, and renders through all
  six annotated pods with one image build. Consul rides the PR smoke
  and the operator leg; git, firestore and config-server clients are
  the remote repository's container suites, and their agent arms are
  identical flag plumbing — the exclusion is stated in the script, not
  implied.
- **Named renders — several documents, one pod.** Every store-shaped
  annotation takes a `.<name>` suffix (`source.cache`, `key.cache`,
  `path.cache` …); each name is one more injected agent writing one
  more file into the shared directory, with its own store, auth,
  credentials, template, cadence and file-mode — the Vault-injector
  multi-secret idiom in this contract's spelling. Pod-wide things stay
  pod-wide (mode, volume, resources, identity, env-inject). Refusals
  name the suffixed key; paths outside the default's directory are
  refused (one volume, stated).
- **Operator: a `Ready` condition and Events.** `kubectl wait
  --for=condition=Ready dynamicconfigrender/…` works; renders and
  failures land in `kubectl describe` as Normal/Warning events. Status
  keeps `renderedAt`/`lastError` beside the condition.
- **Metrics for the agent and the operator.** The agent takes
  `--metrics-port` via the `metrics-port` annotation (renders,
  failures, last-render timestamp in Prometheus text, watch-mode
  sidecars only); the operator serves the same at `:9090`
  (`DYNAMIC_CONFIG_OPERATOR_METRICS_ADDR` moves or empties it), and
  the chart exposes the port. The webhook already had its three
  admission counters.
- **Nightly CVE scan of the PUBLISHED images** — what users actually
  pull ages between releases; trivy fails the night on fixable
  HIGH/CRITICAL findings, and a red night here is the signal to cut a
  patch.
- **`env-restart`** — opt-in follow-up to `env-inject`: the wrapper
  exports the dotenv's fingerprint at start, a generated liveness
  probe compares the live file against it, and a re-rendered document
  makes the kubelet restart JUST the app container — which re-sources
  the new file. The closest thing to a live env update the kernel
  permits (environ freezes at start; the rule is honoured, not
  fought): seconds, no pod recreation, no rescheduling, no new IP.
  Needs `mode: both`; refused on containers that own a livenessProbe.
- **`shape: entries`** — every leaf verbatim
  (`auth.postgres-password` stays so): the `existingSecret` contract,
  where the Secret's KEY NAMES are some other chart's or operator's to
  choose and any mangling breaks them. The book's operator chapter
  carries the worked bitnami-postgresql recipe.
- **Kustomize caught up with the chart**: the base now lists the third
  CRD (ClusterDynamicConfigClass — it was generated but not included),
  and two new overlays land: `selfrotate` (env + the name-scoped RBAC +
  the empty Secret vessel, the helm mode's exact price sheet) and
  `with-operator` (the reconciler + its RBAC), composable with any TLS
  overlay in one Kustomization — the GitOps page and the kustomize
  README both walk it.
- **`ClusterDynamicConfigClass`** —
  split, drawn here too: a cluster-scoped class the platform team
  defines once, its credential in an explicitly named namespace
  tenants cannot read, and a `namespaces` allowlist enforced at
  reconcile (the refusal lands in `status.lastError` with the class
  named). A tenant's Render opts in with
  `classKind: ClusterDynamicConfigClass` and never sees a credential.
  Editing a cluster class re-renders every Render referencing it, in
  every namespace.
- **`agent.pullSecret`** — for fleets that mirror the agent image into
  a private registry: appended (never replacing) to each injected
  pod's `imagePullSecrets`. Pull secrets are namespaced; the chart
  README says the replication job out loud.
- **Real-software examples** under `examples/real/`: Apache Airflow
  (the `AIRFLOW__SECTION__KEY` env dialect through `env-inject`),
  Grafana (provisioning YAML kept live by the sidecar), a Kafka client
  (the Secret target carrying `client.properties`), and the
  multi-tenant cluster-class walkthrough.
- **The operator's Secret target** (`target.secret` +
  `shape: file | envEntries`) — for consumers the file path cannot
  reach: a Secret-watching operator  reacts to
  every reconcile with no pod restart, and `envFrom` turns
  `envEntries` (each leaf of the resolved document, `db.pool_size` →
  `DB_POOL_SIZE`) into environment variables at the next container
  start. A vault class is ALLOWED into the Secret target — the
  container a secret store's document belongs in — while the ConfigMap
  target keeps refusing it. Owner-collected like the ConfigMap;
  operator RBAC gains the Secret write verbs, stated in the chart.
- **`env-inject`: real OS environment variables** .
  Name a container and its command is wrapped in
  `set -a; . <path>; set +a; exec …`, so an init-rendered dotenv is
  the process's environment from its first instruction. The physics
  stated rather than papered over: environment freezes at container
  start (Kubernetes' rule), so `mode: init`/`both` is required, and a
  missing explicit `command` is refused by name — an ENTRYPOINT is
  invisible to the webhook.
- **`file-mode` + `agent-run-as-user`/`agent-run-as-group`** — the
  rendered file's octal permissions and the injected agent's UID/GID,
  so a tighter-than-0644 file is owned by exactly the uid the app
  container runs as. The mode lands on the scratch file **before** the
  atomic rename (a reader never sees the final path in a mode it will
  not keep); uid/gid `0` and owner-unreadable modes are refused at
  admission; the PR smoke asserts mode and ownership inside the pod.
- **Every example runs as applied**: the eighteen manifests now carry
  a real image (`busybox:1.36`) whose command tails the rendered file
  — apply one against a store and watch the document arrive.
- **The async three: etcd, NATS, S3.** The agent is now async at the
  top (`#[tokio::main]`) and drives both of the engine's source
  traits: the blocking six run under a blocking task — honest async,
  no `block_on`-inside-a-runtime hazard — and the async clients are
  driven directly. The 0.1 refusals-by-name (webhook and agent both)
  retire with this.
- **etcd with BOTH of its methods first-class**: TLS client
  certificates (`tls-secret`, the credential IS the certificate) and
  username/password (`auth-username` + `password-secret`) — etcd has
  no Kubernetes auth method to log into, and the org's identity-first
  policy explicitly does not punish stores that never will.
- **NATS**: a `.creds` file (the account idiom) via `auth-token-path`,
  or `token-secret`; `key` is `<bucket>/<key>`.
- **S3 and everything speaking its API**: the ambient AWS chain — IRSA
  on EKS, the workload's own identity — with `api-url` overriding the
  endpoint for MinIO/Ceph/R2, path-style always on.
- Three new book chapters, four new ready-to-apply examples
  (`etcd-tls`, `etcd-password`, `nats-creds`, `s3-irsa`).

## [0.1.1] — 2026-08-19

### Added

- **The release machinery itself** (`release.yml`): merging a version
  bump into `main` builds the three images multi-arch (buildx,
  linux/amd64 + linux/arm64, distroless, nonroot), pushes them to
  **ghcr.io and Docker Hub with identical digests** — one build, both
  registries in the tag list, verified in the job — signs images and
  chart with **cosign keyless** (the certificate names this repository
  and workflow, no key to rotate or leak), attaches SBOM and
  provenance attestations, pushes the chart to
  `oci://ghcr.io/dynamic-config-rs/charts` with its ArtifactHub
  metadata layer, and mints the tag last, so a tag marks what actually
  shipped. `cosign verify` against either registry proves the same
  bytes.

- **The deploy surface grew up.** The chart's values now cover the
  enterprise checklist — naming overrides, common labels/annotations,
  per-component images with pull policies and digests, service
  accounts, probe/rollout tuning, service type, objectSelector,
  cert-manager duration/renewBefore, `extraEnv`/`extraVolumes` escape
  hatches, operator RBAC toggle — with a full values reference in
  `deploy/helm/README.md` and install NOTES. CRDs now actually install
  with the chart (`crds/`), and `deploy/kustomize/` carries the same
  resources for helm-less shops: a base plus cert-manager and
  own-cert overlays. One generated CRD source, three drift-gated
  copies (`just crds`).

### Fixed

- **The webhook silently excluded `default` when installed there.** The
  self-deadlock guard excludes the release namespace; a chart installed
  into `default` therefore never injected anything beside it. The e2e
  smoke now installs into a dedicated namespace as production should,
  the NOTES warn on a `default` install, and the smoke prints webhook
  logs and events on failure instead of tearing the evidence down.
- The operator image build pinned `enum-ordinalize` back to the 1.88
  line the images build with.

- **Output templating.** `dynamic-config.rs/template` (inline) and
  `template-configmap` (mounted, re-read every render) put a minijinja
  template between the resolved document and the file — env files,
  framework-shaped nesting, headers. Strict undefined (a typo is an
  error, not an empty string), lowercase booleans, the trailing newline
  survives, keep-last-good covers a bad edit, and the template's
  context is exactly the resolved document — no files, no environment,
  no network. The agent grew `--template`/`--template-inline`, and a
  templated `--out` frees its extension (`.env` is legal there).
- **Namespace gating.** `webhook.namespaceGating=true` selects only
  namespaces labeled `dynamic-config.rs/injection: enabled` —
  Istio-style opt-in that bounds the blast radius and makes
  `failurePolicy: Fail` a per-namespace promise.

- **Enterprise round.** Unknown `dynamic-config.rs/*` annotations now
  FAIL the admission (a typo'd `token-secret` silently ignored is a pod
  without the auth it declared); `template`/`template-configmap` are
  reserved for 0.2.0's output templating and refused by version;
  `source: etcd|nats|s3` is refused at admission instead of at
  container crashloop. One structured audit line per patched/refused
  admission (names, never values) and `GET /metrics` with
  `dynamic_config_admissions_total{outcome}`. Hardcoded values moved to
  the chart: injected-agent resource defaults (`agent.defaults.*` →
  env), the webhook's bind address/port (`webhook.port`,
  `webhook.hostNetwork` for API-server-off-pod-network CNIs),
  `imagePullSecrets`, `webhook.podAnnotations`. Twelve ready-to-apply
  manifests landed in `examples/` — every store, every auth method,
  and the native-sidecar Job.

- **Production posture, end to end.** The webhook terminates TLS
  in-process (tokio-rustls over `ring`, the config server's own accept
  loop shape) with hot certificate reload and a graceful SIGTERM drain
  — an admission webhook is only ever called over HTTPS, and the
  previous plain-HTTP listener could never have been. cert-manager is
  now **optional**: the chart's default mints its own CA and ten-year
  certificate at install (Secret reused across upgrades, caBundle
  embedded), `webhook.certManager.enabled=true` keeps the renewing
  mode. Injected agents carry the full restricted-PSS security context,
  requests/limits (annotation-tunable: `agent-cpu-request`,
  `agent-memory-request`, `agent-cpu-limit`, `agent-memory-limit`), a
  memory-backed emptyDir by default (`volume-medium: disk` opts out),
  and `native-sidecar: "true"` injects the watcher as a
  `restartPolicy: Always` init container (Kubernetes 1.29+; Jobs
  finish). The chart grew the standard label set, a ServiceAccount with
  `automountServiceAccountToken: false`, topology spread, a
  PodDisruptionBudget, an optional deny-all-egress NetworkPolicy,
  namespace self-exclusion in the webhook configuration
  (kube-system, kube-node-lease, the release namespace,
  `webhook.excludeNamespaces`), `failurePolicy`/`timeoutSeconds`
  values, digest pinning, and a render-time refusal of `tag: latest`.
  The agent image reaches the webhook through
  `DYNAMIC_CONFIG_AGENT_IMAGE` — the chart's `agent.image` value now
  actually governs what gets injected. The e2e smoke runs the
  zero-dependency default (`CERT_MANAGER=1` covers the other mode) and
  asserts the injected posture on the live pod.

- **`dynamic-config-agent`** (0.1.0's piece): `--source
  consul|vault|config-server|firestore|git|redis`, the full
  per-store auth surface (`--auth` with each store's methods — vault's
  seven, consul's login flows, Workload Identity on firestore, tokens
  and ssh keys on git), secrets through `DYNAMIC_CONFIG_AGENT_TOKEN` /
  `_PASSWORD` / `_ENDPOINT` only (never flags), a private CA and a
  client certificate as data (`--ca`, `--tls-cert`/`--tls-key`),
  `--key`, `--out` (extension picks the format,
  `.properties` and `.ini` included — the agent owns flat emitters the
  engine's `save` refuses, and the round trip through the engine is a
  test), `--one-shot` for init containers, `--watch <seconds>` for
  sidecars, atomic write-then-rename, keep-last-good on fetch failure,
  JSON logs. etcd, nats and s3 join with the async path in 0.2.0 and
  are refused by name until then.
- **`dynamic-config-webhook`** (0.2.0's piece, golden-tested today): the
  `dynamic-config.rs/*` annotation contract v1, pure JSONPatch
  generation — a wrong ask *fails the admission* rather than silently
  not injecting — and an axum server over the organisation's own
  request-scope crate. The contract carries authentication end to end:
  pass-through `auth*` annotations, `token-secret`/`password-secret`/
  `endpoint-secret` wired as Secret-backed environment variables,
  `ca-configmap`, `tls-secret` (the `kubernetes.io/tls` pair) and
  `ssh-secret` (the `kubernetes.io/ssh-auth` convention, mounted
  `0400`, `auth: ssh-key` implied) as read-only mounts into the agent
  alone. Two golden files lock the shape. cert-manager is a declared
  dependency.
- **`dynamic-config-operator`** (0.3.0's piece, scaffolded): both CRDs
  as Rust types, `--crds` printing the manifests `deploy/` embeds (a
  `just crds` gate keeps them from drifting), and a watch-and-log loop
  the reconcilers land into.
- **`deploy/`**: the helm chart (webhook + Certificate + mutating
  configuration), kustomize base, generated CRDs. **`e2e/`**: the kind
  smoke — inject, render from a live Consul, read the file back out of
  the pod.
