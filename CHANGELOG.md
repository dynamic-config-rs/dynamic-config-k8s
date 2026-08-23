# Changelog

All notable changes to the `dynamic-config-k8s` components are documented
here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

The four components version together and ship as images, not crates.
Pre-1.0, a breaking change to the **annotation contract** bumps the minor
version — the contract is the API here.

## [Unreleased]

## [0.3.0] — 2026-08-23

### Added

- **The injected agent is probed, and `/readyz` finally means something.**
  The endpoint answered `ok` before the first render, so nothing could tell a
  pod that had configuration from one that did not. It is 503 until a
  document has been rendered now, and the webhook attaches a `readinessProbe`
  to the injected container — pod readiness is already AND-ed across
  containers, so a Service sends no traffic to a pod whose configuration does
  not exist yet.

  The trade, said out loud: with the store unreachable at start and nothing
  cached, the pod never becomes ready. That is correct — the application has
  no configuration — but it turns a store outage into a visibly stalled
  rollout rather than pods that come up and misbehave.
  `dynamic-config.rs/readiness: "false"` is the escape hatch, and
  `agent.defaults.metricsPort` now defaults to `9110` because the probe
  answers on that port.
- **`dynamic-config.rs/startup-policy`** — `allow-cached` (the default),
  `require-fresh` or `best-effort`. The rendered volume is an `emptyDir`,
  which survives a *container* restart and dies with the pod, so an agent
  coming back after a crash usually finds its own last render on disk.
  Refusing to start on it turned a store outage into "every restarting pod
  stays down", when the file it needed was already there.

- **Dynamic secrets, end to end.** `dynamic-config.rs/dynamic: "true"` reads
  a Vault engine that mints credentials; the agent then renews the lease at
  about two thirds of its life — on the *lease's* clock, spread through the
  same pace the polls use — and hands it back on SIGTERM.
  `revoke-on-shutdown: "false"` is the opt-out for a lease something else is
  still using.

  Two rules the loop keeps: **a renewal is not a render**, because extending
  a lease keeps the same credential and rewriting the file would wake every
  application watching it for nothing; and a renewal that is refused
  *re-fetches*, because the credential is expiring and a new one is the only
  recovery — which is a new document, so that one does render.

  Four metrics come with it: `lease_renewals_total`,
  `lease_renewal_failures_total`, `lease_revocations_total` and
  `lease_ttl_seconds`.
- **A meta file beside the render**, behind `dynamic-config.rs/meta: "true"`
  — a digest of the bytes, the store's revision, and a clock. It answers the
  question an application cannot otherwise ask: *which configuration am I
  actually running?* Two pods holding the same file is a claim nobody can
  check from inside either of them; two pods printing the same digest is one
  anybody can. It describes the render and never contains it.
- **Three gaps against the reference implementation, closed.**
  `agent-ephemeral-request` and `agent-ephemeral-limit` set the injected
  container's ephemeral storage — which mattered for exactly one
  configuration and was unbounded: on `volume-medium: disk` the rendered
  volume is node storage rather than the pod's memory, `history` keeps
  copies beside it, and nothing declared that resource, so a pod could fill
  a node's disk without exceeding a limit it had declared.

  `timeout` is the deadline for one fetch attempt, per pod. Ten seconds
  everywhere was right for a store on the same network and wrong for a Git
  remote across a WAN; a workload in that position had no way to say so.

  `agent-image` names the image for this pod's own agent, which is how an
  agent upgrade stops being all-or-nothing — one Deployment tries it first,
  the way `canary-configmap` already lets one cohort try a document. Refused
  unless the installation lists a prefix it starts with, because an image on
  the injected container runs chosen code beside the application holding the
  store's credential.
- **A node-level agent, delivered as a CSI driver** —
  `dynamic-config-node-agent`, a fourth image and a DaemonSet. One process
  per node instead of one container per render: 10,000 pods at 2.5 renders
  each is 25,000 sidecar containers, and that number is what this exists
  for.

  **These were two entries on the backlog and are one component**, because
  a DaemonSet that fetches for a whole node has to get bytes into a pod, and
  there are two ways: a `hostPath` the pod also mounts, which restricted Pod
  Security forbids for the reason it forbids it, or a CSI volume, which is
  the interface Kubernetes added for exactly this. Building them separately
  would have been building a thing and its only delivery mechanism as though
  they were unrelated.

  Pods on one node wanting the same document from the same store **under the
  same credential** share one fetch and one watch — a node whose hundred
  pods read one Consul key opens one connection. The credential is part of
  that identity rather than metadata beside it: sharing across it would hand
  one namespace's document to another. What is not shared is the rendered
  file, which each pod gets at its own path in its own format.

  One property a sidecar cannot offer comes free: the kubelet does not start
  a pod's containers until every volume is published, and publishing does
  the first fetch — so there is no window in which the file is missing, and
  no init container because there is nothing for one to do.

  **Off by default, and the sidecar stays the shape to reach for.** This
  holds the store credentials of every pod on its node, runs as root because
  the kubelet owns the volume directories, and mounts host paths. Not a
  better shape — a different trade, for a scale that makes the first one
  untenable, and one to make with a measurement rather than a preference.

  The CSI proto is vendored at v1.11.0 rather than fetched: a build that
  reaches the network fails in an air-gapped mirror. Registration is
  upstream's `node-driver-registrar`, because writing it here would
  reimplement a thing Kubernetes ships.
- **`dynamic-config.rs/class`** — the webhook resolves a
  `DynamicConfigClass` or `ClusterDynamicConfigClass`, so a pod names a
  store the platform team maintains instead of repeating its endpoint and
  auth. The operator has read these objects since 0.1.1 and the injector did
  not, which meant the two halves of this product solved the same problem
  twice.

  **The admission path still calls nothing.** That was the property this
  waited two rounds for: a webhook that reads the cluster in its hot path
  fails when the API server is busy, and every pod creation fails with it.
  The classes are listed on a background timer into a map in memory, and
  admission reads the map — the cost is a synchronisation delay a mounted
  ConfigMap already has, and a class created seconds ago is refused by name
  rather than admitted without a store.

  Off unless `webhook.classes.enabled`, which also creates the cluster-wide
  `list`/`watch` on the two class kinds — and on nothing else, least of all
  the Secrets they name. A class supplies defaults rather than pins, and a
  namespaced class shadows a cluster one of the same name.
- **`dynamic-config.rs/events`** — the agent writes `Warning` Events on its
  own pod for `RenderFailed` and `DocumentAbsent`, so `kubectl describe pod`
  shows them where somebody is already looking rather than one
  `kubectl logs -c` away.

  **Off, and twice opt-in**, because it is the one thing that puts an API
  credential in the sidecar: an administrator creates the Role
  (`agent.events.enabled`, `create` on `events` in named namespaces and
  nothing else) and sets `webhook.allowEvents`; only then may a pod ask, and
  asking without it is refused with the chart value named rather than
  admitted into a 403.

  Hand-written against `ureq`, which is already in this binary's tree for
  three of its stores — `kube` and `k8s-openapi` would roughly double the
  image for one POST. The service-account token is re-read per Event, the
  same rule the Vault store follows for the same file.
- **`dynamic-config.rs/canary-configmap`** — a change reaches part of the
  fleet first. The cohort is the pod's own name hashed into a bucket, so it
  is deterministic and stable across widening; the percentage is a **mounted
  ConfigMap**, so growing the cohort is an edit rather than a new pod spec —
  and a new pod spec is a rolling restart that discards the state a canary
  exists to watch.

  That last part is what kept this deferred: the missing piece was never the
  cohort arithmetic, it was who advances it without restarting everything.
  A pod outside the cohort **holds** what it fetched and publishes it when
  the number passes its bucket, so nothing is re-fetched and the store need
  not speak again.

  `0` holds everybody, `100` holds nobody, and a file that is missing or
  unparseable is no canary rather than zero — a typo must not freeze a fleet
  on its current document. `canary_holding` and `canary_percent` say what is
  happening, and `applied` beside them answers the question that decides
  whether to widen.
- **Consumer acknowledgement.** `renders_total` said a document reached
  disk and nothing said whether the application read it, so an application
  still running the previous one looked exactly like a converged fleet. The
  application now POSTs the fingerprint it is running to `/applied` on the
  metrics port — the same string `fingerprint()` answers in three languages,
  and the one the meta file already carries for everyone else — and four
  series come out of it: `acks_total`, `ack_mismatches_total`, `applied` and
  `unapplied_seconds`.

  `200` means current, `409` names the document that actually is. Neither a
  restarted application acknowledging what it read before the restart nor a
  slow one acknowledging a replaced generation is an error it caused, and
  neither is convergence.

  **`dynamic-config.rs/require-ack`** turns it into readiness: the pod stays
  unready until the application says it applied the document, so a Service
  sends no traffic to a pod running configuration nobody confirmed. Off by
  default, and refused without a readiness probe or on `mode: init` — it
  needs the application's cooperation, and one that never acknowledges would
  never become ready.
- **`dynamic-config.rs/history`** — the replaced generation is kept beside
  the render, so *what was the file before?* has an answer. The rename that
  publishes a new document is the same rename that destroyed the old one,
  and by the time an incident asks, the store has moved on too.

  At most ten, off unless asked, each copy taking the mode the render had,
  and refused alongside `volume-medium: disk` where a replaced secret would
  outlive the pod. **Not a rollback**: putting an old document back needs
  the application to say the new one is bad, and nothing here can hear that
  yet.
- **Rotated TLS material no longer needs a pod restart.** The kubelet
  rewrites a mounted ConfigMap or Secret in place and nothing tells the
  process; every store client here reads its trust material once, when it
  builds. So a rotated CA used to be a restart, and a rotation nobody
  restarted for was a store that stopped answering at a moment unrelated to
  the rotation.

  The agent watches the files it was given and rebuilds the client when they
  move. **A rebuild is not a restart**: the process stays up, the rendered
  file never leaves the volume, the last-known-good stays, and the counters
  keep counting. `dynamic_config_agent_tls_reloads_total` counts them, and
  `dynamic-config.rs/tls-reload: "false"` opts out.

  A change is confirmed by reading twice a quarter-second apart, because a
  rotation writes more than one file and a client built from a half-written
  certificate and key fails in a way that reads as a bad certificate. Tokens
  needed none of this: the projected service-account token and the config
  server's bearer file are re-read on every use and always were.
- **The annotation contract is a registry rather than two lists.** Every key
  is one row carrying whether it takes a `.name` suffix and whether it has
  been retired; `KNOWN` and `PER_RENDER` are views of it. Two lists of the
  same names drift — a key added to one and forgotten in the other is either
  an annotation nothing accepts with a suffix or one accepted with a suffix
  no code reads.

  Nothing is deprecated yet, which is the state the table exists to be ready
  for: a pod setting a retired key will be **admitted with a warning** that
  names the replacement rather than refused, because a contract that breaks
  a working pod to make a point is one people pin an old version of. The
  mechanism is tested against a table of its own, since that is the only
  moment it can be proved without a real deprecation to point at — and the
  book's annotation page is checked against the registry, so a key cannot be
  accepted and left undocumented.
- **`dynamic-config.rs/notify-http`** — a localhost endpoint the agent POSTs
  to after the rename, for nginx, Prometheus and every other daemon that
  reloads on a request and on nothing else. One attempt, a two second
  deadline, never fatal: the file is already correct when it is sent.
  Localhost only by construction, checked at admission *and* in the agent —
  an agent that will POST anywhere is an SSRF primitive holding the pod's
  store credential. No signal form, and none planned: that needs
  `shareProcessNamespace` on the pod, which is a change to the process
  boundary between containers that a webhook should not make for a workload.
- **`dynamic-config.rs/on-drift`** — `warn` (the default), `repair` or
  `fail`, for a rendered file something else in the pod has written to. The
  agent owns the file and the volume is shared, so until now a stray write
  went unnoticed until the next change arrived from the store.
  `dynamic_config_agent_drift` and its counter move either way.
- **`dynamic-config.rs/tls-server-name`** and
  **`dynamic-config.rs/tls-skip-verify`**. The first names the certificate's
  own name for an endpoint written as an address it does not carry, and
  keeps the store authenticated. The second does not, and is gated four
  ways: an administrator must set `webhook.allowTlsSkipVerify`, it is
  refused alongside `ca-configmap`, it earns an admission warning, and the
  agent reports `dynamic_config_agent_tls_verification_skipped 1` so one
  alert finds every pod using it.
- **`dynamic-config.rs/revoke-grace`**, **`init-first`**,
  **`agent-run-as-same-user`** and **`extra-secret`** — four ergonomics the
  reference implementation has and this did not. `revoke-grace` replaces a
  fixed five seconds and is refused past the pod's
  `terminationGracePeriodSeconds`, where the kubelet would cut the
  revocation off anyway; `init-first` puts the injected init container ahead
  of the pod's own; `agent-run-as-same-user` takes the UID from the
  application rather than from a number somebody keeps in step, refusing
  absent and root; `extra-secret` mounts a Secret read-only into the agent
  alone, at a fixed path.
- **`dynamic-config.rs/inject-containers`** — which of the pod's own
  containers receive the rendered volume. Every one of them by default, as
  before and as the reference implementation defaults; naming a subset is
  for the pod that runs a log shipper or a mesh proxy beside its
  application. `file-mode` cannot draw that line — a sidecar in the same pod
  usually runs as the same UID and reads a `0600` file exactly as well as
  the application does. A name the pod does not have is refused, as is
  leaving out the container `env-inject` wraps.
- **`dynamic-config.rs/on-delete`** — `retain` (the default), `remove` or
  `fail`. A document that disappears from the store used to be completely
  silent: the agent went on serving the file it last rendered, with no log
  line, no metric movement and every health check reporting fine. It is now a
  reported condition whichever policy is chosen — `dynamic_config_agent_absent`
  and its counter move, the log says whether the store answered *gone* or did
  not answer at all, and under `fail` the loop ends so the pod's restart
  policy takes over. `retain` stays the default because a vanished key is
  more often a mistake in the store than an instruction to the workload.
- **`--max-document-bytes`** in the agent, checked before the document is
  parsed rather than after, and a `sizeLimit` on the rendered `emptyDir` for
  both media. A 64Mi container facing an unbounded document had nothing
  between it and the OOM killer, and a `medium: Memory` volume with no limit
  charges the pod's memory budget for whatever is written into it.
- **`dynamic-config.rs/max-staleness`**, wired into `/readyz`.
  Last-known-good answers "is there a document" and leaves "is it too old to
  trust" open — a credential may be worthless after five minutes and a
  feature flag fine after a week, so the ceiling is off unless set.
- **`dynamic_config_agent_generation`**, the store's own revision of what was
  last rendered.
- **`dynamic-config-webhook validate <pod.yaml>`** — the admission decision
  without a cluster. The webhook was already a pure function of a pod and an
  installation and the golden tests already drove it that way; this is that
  call with a front door, so a mistyped annotation is caught in CI rather
  than by a rollout that will not start.
- **Admission warnings.** The response had no channel for anything that was
  worth saying and not worth refusing over, so a configuration that was legal
  and unwise arrived in silence. Four earn one: disk-backed storage for a
  rendered secret, a world-readable file mode, a poll interval under five
  seconds, and a lease nobody will hand back.
- **`deletionPolicy: Delete | Retain`** on a render's target. `Delete` is
  what already happened. `Retain` simply does not write the owner reference —
  and deliberately never *removes* one, because rewriting ownership
  retroactively is how an upgrade deletes somebody's Secret.
- **`observedGeneration`** on a render's status, and two reasons that were
  missing: `ClassNotFound` and `ClassNotAllowed`. Every failure used to be
  `RenderFailed`, including the operator's two most common answers — so
  telling them apart meant grepping a message.
- **OpenTelemetry, in the binaries.** All three export OTLP traces when
  `OTEL_EXPORTER_OTLP_ENDPOINT` is set and nothing at all when it is not, so
  no existing deployment changes behaviour and no annotation is needed to
  turn it on — the variable is OpenTelemetry's own, and a collector already
  injecting it into pods is enough.

  It is here rather than in the engine on purpose: a pipeline owns a runtime,
  a batch processor and a shutdown, and a *library* that takes those over is
  one that has to be fought. Programs may, because programs own `main`. The
  Prometheus text every binary already serves is untouched — the collector's
  `prometheus` receiver reads it directly, and neither way out is the price
  of the other.

  Resource attributes come from the downward API and are absent rather than
  invented when nobody wires them: a made-up pod name is worse than none.
- **Several files, one generation.** `dynamic-config.rs/also.<name>` is a
  further file cut from the **same** fetched document —
  `also-section.<name>` picks which part — and the set is published
  **all or none**: if any of them fails to resolve, render or validate,
  none is written and every last good file stays. An application reading
  two of them never sees one from before a change and one from after it
  because the second failed.

  Two things this deliberately is not. It is **not a second source**: two
  stores have no common instant, so "these files are one generation" is not
  something either could promise — a *named render* is the shape for a
  second store, and it stays a separate container with a separate
  generation because that is the truth about it. And it is **not atomicity
  across files**: each rename is atomic, the set is not, and a reader can
  in principle catch the microseconds between two of them. That window is a
  rename apart rather than a fetch apart, which is the difference worth
  having — calling it atomic would be a promise the filesystem does not
  make.
- **Schema validation at the boundary.**
  `dynamic-config.rs/schema-configmap` mounts a JSON Schema and the agent
  checks the resolved document against it *before* the write — so a document
  that does not satisfy it never reaches the application and the last good
  file goes on serving. The engine's own validation is a *typed* one and
  belongs to Rust, Python and Node; this is the same guarantee for the
  consumers that are none of those.

  The validator is built with `default-features = false`, which drops the
  HTTP and file `$ref` resolvers and the TLS stack with them. That is
  deliberate: a schema arrives as a mounted ConfigMap, and a validator that
  could fetch a `$ref` over the network would be a config agent making
  requests on a schema's say-so — the same hazard as a template that can
  reach a store, which this family already refuses. A refusal names the path
  and the rule and never the value.
- **A chart upgrade leg**, nightly, from the published chart to the working
  tree. Every other leg installs fresh, which proves the install and says
  nothing about the thing an operator does on a Tuesday. It pins the three
  failure modes only an upgrade has: `deploy/helm/crds/` is where Helm puts
  CRDs it installs and **never upgrades**, the webhook's `caBundle` can be
  rendered empty over by the chart, and pods admitted by the previous
  version have to go on meaning what they meant.
- **A scale leg**, also nightly. The webhook has had an admission-latency
  histogram since 0.2.0 and nothing ever drove load through it, so its own
  "tens of microseconds" was an assertion rather than a measurement. Two
  hundred admissions, the histogram read back, and the agent's working set
  multiplied out — read from the kubelet's summary API, because the image is
  distroless and has no shell to read a cgroup with. Output rather than a
  gate: it fails only on a refusal, or on an admission that ate a second of
  the API server's ten.
- **A change→visible benchmark**, `e2e/latency.sh`, as a fourth kind leg.
  Every other leg here proves correctness — the document arrives, and it is
  the right one — and none of them ever produced a number, which is the
  thing the parity document says not to do. It times the whole path, from
  `consul kv put` to a shell inside the application's container reading the
  new value, and prints p50/p95/max into the job summary.

  Deliberately not the agent's own metrics: the number worth publishing is
  the one a consumer experiences, and an internal timestamp would flatter it
  by the width of the rename. The pod's poll interval is set to five minutes
  so a resync cannot deliver the change — otherwise the leg would be timing
  a poll, which is exactly the regression it exists to catch.
- **Six template filters** that minijinja does not ship and a configuration
  template needs: `b64encode`, `b64decode`, `json`, `yaml`, `quote` and
  `required`. `tojson` was unavailable even so — minijinja's `json` feature
  is off in this build — which left a template unable to emit the format half
  its consumers read.

  Every one is a pure function of the document already in hand, and that is
  the line: a template cannot read a file, reach a store or see an
  environment variable, so a rendered document stays a function of what was
  fetched. Consul Template's `secret()` is precisely the feature not copied.
- **The agent hears SIGTERM.** It passed a future that never completed, so
  the process was simply killed. Fine for a file; not fine for a credential
  minted for one pod, which would have outlived it.

### Security

- **Transport credentials are wiped when they go.** The token, the password
  and the keys used to *reach* a store are `Zeroizing` now, and `Spec` grew
  a hand-written `Debug` that prints `***` for both — it derived one, and a
  derived `Debug` on a struct holding credentials is precisely the accident
  every store crate here already guards against.

  The claim is narrow on purpose: the resolved document is **not** wiped. It
  has to be plaintext to be written to a file, and saying otherwise would be
  a promise this cannot keep.

### Fixed

- **A named render reading S3 gets the pod's AWS credential.** `aws-secret`
  has no per-render form and is refused outright unless the pod's own
  source is s3 — so a pod declaring it and then adding a named render on
  the same bucket was the only shape this could take, and the named
  render's agent was the one container never given the credential. It
  could not authenticate to the store its own arguments named. Shipped in
  0.2.0, when named renders and `aws-secret` arrived together.
- **A refusal that echoes an endpoint no longer echoes the credential in
  it.** A pinned-value refusal quotes what the pod asked for, on purpose —
  a value the author wrote and did not get is a debugging session. But an
  endpoint is a URL, a URL can carry `user:password@`, and a refusal is
  the `status.message` of a rejected API call: it reaches the server's
  audit log and the events of whatever controller was creating the pod.
  The userinfo is replaced; the rest of the value is untouched.
- **A `THREAT_MODEL.md`**, a Grafana dashboard with recording and alert
  rules under `deploy/observability/`, four `ValidatingAdmissionPolicy`
  samples under `policies/`, and `scripts/airgap-bundle.sh` — which carries
  the signatures and attestations across with the images, because the
  supply-chain work is worth nothing offline if they are left behind with
  the registry.
- **A lease that cannot be renewed is no longer asked to.** `renewable` came
  back on every `Fetched` and nothing read it: the agent sent
  `sys/leases/renew` on a fixed fraction of every lease's life, including
  the ones Vault had already said were not renewable — every `pki/issue`,
  and any database credential past its role's maximum. That was a round trip
  per cycle per pod that could only be refused, and a
  `lease_renewal_failures_total` that climbed steadily on a fleet where
  nothing was wrong, which is the kind of counter people learn to ignore.

  Such a lease is re-issued instead, at **90%** of its life rather than the
  65% a renewal uses. The two fractions are different on purpose: a renewal
  that fails still leaves a third of the lease to recover in, while a
  re-issue *is* the recovery — running it early only shortens the credential
  in use and wakes every application watching the file for it.
- **A dropping stream no longer reopens at full rate.** The reopen backoff
  and the store-readable backoff were one `Pace`, so a store whose watch kept
  failing while its *reads* kept working had its backoff wiped by every
  resync — the connection was reopened on the interval, forever. They are two
  now: a delivery resets the stream's pace, a fetch resets the store's. The
  reopen also records the failure *before* drawing its wait, which it did the
  other way round — so the first reopen after a drop always waited the
  healthy interval however long the stream had been failing.
- **A burst no longer tears the watch down.** The delivery slot was an
  `mpsc` of capacity one — a *queue* that holds one, not the latest-wins slot
  its own comment described. Blocking stores blocked their own watch loop on
  it; async stores returned an error that ended the connection and counted a
  reconnect, so the loudest signal for a sick stream fired hardest when the
  stream was healthy. It is a `watch` channel now: the newest document
  replaces one nobody has rendered yet.
- **A hung read no longer stalls everything else.** The resync was awaited
  inside a `select!` arm body, which holds the whole loop — one unanswered
  `GET` stopped deliveries, lease renewals and the shutdown branch with it.
  The resync is a spawned producer into the same slot now, one at a time.
- **A failed write is no longer fatal.** Every other runtime failure warned
  and kept the last good file; this one ended the process, so a transient
  `ENOSPC` on the tmpfs killed a sidecar that was holding a perfectly good
  document.

### Changed

- **arm64 is built on arm64.** Both architectures used to come out of one
  job, with the arm64 half emulated under QEMU — 1h39m for the agent on
  0.2.0, and 1h49m for the webhook. Each half now builds on its own runner
  (free for public repositories), pushes **by digest** with no tag, and a
  join job assembles the manifest list per registry.

  Four things that had to move with it. The **index** is what `cosign` signs,
  because `cosign verify …:v0.3.0` resolves a tag to the list and signing
  what sits underneath would leave that command failing against images that
  are perfectly genuine. The SBOM and provenance stay on the
  per-architecture images, because that is what they describe —
  `SECURITY.md` now says which digest carries which, with the command to
  resolve one. The layer cache is scoped per architecture as well as per
  component, since one scope shared by two legs is two legs overwriting each
  other. And `chart` and `tag-and-release` wait for the join rather than the
  build, or the tag would be minted naming images nobody can pull.

  The cross-registry digest assertion survives, rewritten against the index,
  and a new one checks that the list actually carries both architectures.
- **Named renders are observable.** `dynamic-config-agent-db` and its
  siblings had no metrics block at all, so they reported nothing whatever
  went wrong in them. `metrics-port.<name>` gives each one its own endpoint —
  per render because a port is a pod-wide resource that N containers cannot
  share, and named rather than allocated because `port + n` reads as tidy
  right up to the afternoon it lands on whatever the application is
  listening on. The port is left unnamed and the probe uses the number: a
  Kubernetes port name is at most fifteen characters and a render suffix may
  be thirty-two.
- **CI builds the three images once.** The kind legs each ran `just images` —
  nine uncached builds of the workspace per pull request. One job builds them
  with a layer cache and hands them over as an artifact, which is the shape
  the nightly already used. `images` is named in the aggregate check as well
  as by the legs that consume it: without that, a failed image build merely
  *skips* the legs, and a skipped job is a pass.
- Store pins move to **0.10**, and `AGENT_IMAGE` — a source constant, not a
  chart value — moves with the release to `v0.3.0`.

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
