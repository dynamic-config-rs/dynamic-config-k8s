# The Annotation Contract

v1, and it is the API: a change here is a breaking change of this
integration, whatever the binaries think. Two golden files in the
webhook's test suite byte-compare the full admission response, so the
contract cannot move without a reviewed diff saying it moved.

## The core seven

| annotation | required | meaning |
|---|---|---|
| `dynamic-config.rs/inject` | yes | `"true"` asks; anything else but `"false"` fails the admission |
| `dynamic-config.rs/source` | yes | `consul`, `vault`, `config-server`, `firestore`, `git`, `redis`, `etcd`, `nats`, `s3` |
| `dynamic-config.rs/endpoint` | one of the two | the store's address — a url; `<project>[/<database>]` for firestore |
| `dynamic-config.rs/endpoint-secret` | one of the two | `<secret>/<key>` holding the address, when the address carries a password (a redis url) |
| `dynamic-config.rs/key` | yes | the document's key; `mount/path` for vault, `application/profile` for config-server, the file's path for git |
| `dynamic-config.rs/path` | yes | where the rendered file lands; the extension picks the format |
| `dynamic-config.rs/mode` | no | `init`, `sidecar` (default), `both` |

## Naming a store once

```yaml
    dynamic-config.rs/class: "team-vault"
    dynamic-config.rs/key: "billing/config.json"
    dynamic-config.rs/path: "/config/app.yaml"
```

A `DynamicConfigClass` — the object the operator has read since 0.1.1 —
holds the source, the endpoint and the credential's Secret. A pod that names
one says only what is its own.

The class supplies **defaults**: a pod that names both a class and its own
`endpoint` keeps its own, which is the rule the installation document
already follows. Its own namespace is looked in first, then the cluster
scope, so a namespace can override a platform default without asking the
platform.

**Off unless an administrator turns it on** (`webhook.classes.enabled`), and
the reason is worth reading. The admission path calls the API server
**nowhere** — that is what keeps a busy API server from failing every pod
creation in the cluster — and enabling this does not change it: the classes
are listed on a background timer into a map in memory, and admission reads
the map. What it does change is that the webhook holds a credential and a
cluster-wide read of two custom resources, which is worth having only if
pods actually name classes.

The cost is a synchronisation delay a mounted ConfigMap already has. A class
created seconds ago may not have been polled yet, and a pod naming it is
**refused with that sentence** rather than admitted without the store the
class was to supply.

Two refusals are about who may use what:

- a `ClusterDynamicConfigClass` whose `namespaces` list does not include the
  pod's — the list is what keeps cluster-scoped from meaning anyone
- a cluster class whose credential Secret lives in another namespace. A pod
  mounts Secrets from its own namespace and nowhere else, so that class is
  usable by a `DynamicConfigRender`, which reads the Secret itself, and not
  by an injected agent

## The namespace gate, before any pod is read

Every key below is per-pod. One optional guard sits a level
above: with `webhook.namespaceGating=true` the webhook selects only
namespaces **labeled** `dynamic-config.rs/injection: enabled` — a
label, not an annotation, because the gate lives in the webhook
configuration's `namespaceSelector` and Kubernetes selectors cannot
see annotations. Inside a gated namespace the per-pod
`dynamic-config.rs/inject: "true"` is still required; the
[security page](security.md#namespace-gating) owns the trade
(blast radius, per-namespace `failurePolicy: Fail`), and
[`examples/namespace-gating.yaml`](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/namespace-gating.yaml)
is the ready-to-apply shape.

## What the webhook writes back

| annotation | value | meaning |
|---|---|---|
| `dynamic-config.rs/status` | `injected` | this pod has been through admission and carries the agent |

**Written by the webhook, never by a pod.** A mutating webhook is not
called once — `reinvocationPolicy: IfNeeded` asks the API server to call
it again whenever a later webhook changes the pod, and some controllers
resubmit a spec that has already been admitted. A marked pod is passed
through untouched; without the mark, the second pass would add the agent
again, and two containers with one name is a pod the API server refuses.

A pod that sets this annotation to anything else is refused, with a
message saying so. Setting it to `injected` by hand is a way of saying
"do not inject me", which `inject: "false"` already says more clearly.

## Behaviour

| annotation | default | meaning |
|---|---|---|
| `dynamic-config.rs/timeout` | the store's own, 10s | the deadline for **one fetch attempt**, where the store's client has a door for it. Ten seconds is right for a store on this network and wrong for a Git remote across a WAN or a bucket in another region — until this, a workload in that position had no way to say so and its only recourse was a fetch that kept timing out |
| `dynamic-config.rs/agent-image` | the installation's | the image for **this pod's** agent. How an agent upgrade stops being all-or-nothing: one Deployment tries it first. Refused unless the installation lists a prefix it starts with — an image on the injected container runs chosen code beside the application, holding the store's credential |
| `dynamic-config.rs/watch-seconds` | `15` | how often the sidecar asks a store that must be asked, and how often it re-reads one that pushes; whole seconds. See [Watching](observability.md#watching) |
| `dynamic-config.rs/section` | whole document | the section key the document nests under |
| `dynamic-config.rs/native-sidecar` | `"false"` | `"true"` injects the watcher as an init container with `restartPolicy: Always` (Kubernetes 1.29+); [Jobs finish](security.md#native-sidecars) |
| `dynamic-config.rs/volume-medium` | `memory` | where the rendered file lives: `memory` (tmpfs, off the node's disk) or `disk` |
| `dynamic-config.rs/init-first` | `"false"` | `"true"` puts the injected init container **ahead** of the pod's own, for a pod whose init container reads the rendered file. Appending stays the default: another injector's init container that must run first keeps running first. Refused with `mode: sidecar`, where there is no init container |
| `dynamic-config.rs/agent-run-as-same-user` | `"false"` | take the agent's UID from the application container instead of naming one, so the rendered file's owner matches without two numbers to keep in step. The application's own `runAsUser` first, then the pod's; absent is refused rather than guessed — inheriting whatever the image runs as is a UID that moves when the image does. Root is refused, as always |
| `dynamic-config.rs/extra-secret` | none | a Secret mounted read-only under `/etc/dynamic-config/extra`, **into the agent alone**. For the file a store's own client wants that this contract does not model. The path is fixed rather than chosen: an annotation that took a mount path could be aimed at the rendered volume or the service-account token |
| `dynamic-config.rs/inject-containers` | every container | a comma-separated list of the pod's **own** containers that receive the rendered volume. The default matches the reference implementation's; naming a subset is for the pod that runs a log shipper or a mesh proxy beside its application — neither has any business holding a rendered credential, and `file-mode` cannot draw that line because a sidecar usually runs as the same UID. A name the pod does not have is refused, and leaving out the container that `env-inject` wraps is refused too |
| `dynamic-config.rs/agent-cpu-request` | `10m` | the injected container's CPU request |
| `dynamic-config.rs/agent-memory-request` | `32Mi` | its memory request |
| `dynamic-config.rs/agent-cpu-limit` | none | its CPU limit — none by default, on purpose |
| `dynamic-config.rs/agent-ephemeral-request` | none | ephemeral-storage request. Matters for exactly one configuration and is unbounded without it: on `volume-medium: disk` the rendered volume is node storage rather than the pod's memory, `history` keeps copies beside it, and nothing else here declares that resource — a pod could fill a node's disk without exceeding a limit it had declared |
| `dynamic-config.rs/agent-ephemeral-limit` | none | its ephemeral-storage limit. A request larger than it is refused, because the scheduler's own refusal would name the pod rather than these |
| `dynamic-config.rs/agent-memory-limit` | `64Mi` | its memory limit |
| `dynamic-config.rs/file-mode` | umask's answer (0644) | the rendered file's octal permissions, e.g. `"0640"` — set on the scratch file **before** the atomic rename, so a reader never sees the final path in a mode it will not keep |
| `dynamic-config.rs/agent-run-as-user` | `65532` | the injected container's UID, so the rendered file's **owner** matches what the app runs as; `0` is refused — the agent stays nonroot in every configuration |
| `dynamic-config.rs/agent-run-as-group` | `65532` | its GID, same rule |
| `dynamic-config.rs/aws-secret` | none | s3 only: a Secret whose `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` keys become exactly those variables on the agent — static credentials for S3-compatibles that are not AWS (MinIO, Ceph, R2); on AWS, IRSA needs none of it |
| `dynamic-config.rs/metrics-port` | none | the agent serves Prometheus text on this port ([the series](observability.md#the-agents-metrics)); only meaningful with a watching agent, and `"0"` opts out of an installation-wide default |
| `dynamic-config.rs/agent-env` | none | comma-separated `NAME=value` pairs, set as environment on **every** injected agent — SDK knobs like `RUST_LOG`, `AWS_CA_BUNDLE`, proxy variables. Gated: only names the installation's allowlist permits in this namespace pass admission ([the gate](#the-agent-env-gate)) |
| `dynamic-config.rs/env-inject` | none | a container name: its command is wrapped in `set -a; . <path>; set +a; exec …`, so the rendered dotenv is the process's REAL environment. Needs `mode: init` or `both` (env freezes at container start — Kubernetes' rule) and an explicit `command` (an ENTRYPOINT is invisible to the webhook); both refusals name the fix |
| `dynamic-config.rs/env-restart` | `"false"` | with `env-inject` and `mode: both`: when the sidecar re-renders the dotenv, the kubelet restarts JUST the app container (a liveness probe compares the file against the fingerprint the wrapper exported at start) and the wrapper re-sources the new file — the closest thing to a live env update the kernel permits: seconds, no pod recreation, no new IP. Refused if the container already has its own livenessProbe |
| `dynamic-config.rs/template` | none | an inline minijinja template; [it owns the output bytes](rendering.md#templates) |
| `dynamic-config.rs/template-configmap` | none | `<name>` or `<name>/<key>` (default key `template`): the template from a ConfigMap, mounted read-only and re-read every render |

## Freshness, and what happens when a store is not there

The store is not always reachable, and the document does not always still
exist. Five annotations say what the agent does about it; every default is
the one that keeps a running application running.

| annotation | default | meaning |
|---|---|---|
| `dynamic-config.rs/startup-policy` | `allow-cached` | what a **first** fetch failure means. `allow-cached` serves the file already on the volume if it parses — the rendered volume survives a container restart, so this is a real cache rather than a hopeful one — and goes on watching. `require-fresh` refuses to start without a fresh document, for a pod that must never come up on stale credentials. `best-effort` starts regardless |
| `dynamic-config.rs/max-staleness` | none | how old the document may get before `/readyz` reports 503 — `"6h"`, `"90s"`. Last-known-good answers *is there a document*; this answers *is it still worth trusting*. A credential may be worthless after five minutes and a feature flag fine after a day, which is why there is no default |
| `dynamic-config.rs/on-delete` | `retain` | what a document **disappearing** from the store means. `retain` keeps serving the last render, `remove` truncates the file so a consumer reads nothing rather than a revoked secret, `fail` ends the agent so the pod's restart policy takes over. Whichever is chosen it is reported: `dynamic_config_agent_absent` moves and the log says whether the store answered *gone* or did not answer at all |
| `dynamic-config.rs/require-ack` | `"false"` | readiness waits for the application to POST the fingerprint it is running, not merely for a document to exist. [What it is for](#knowing-the-application-actually-applied-it) |
| `dynamic-config.rs/history` | none | keep the last N replaced generations beside the render. [What it is for](#what-the-file-was-before) |
| `dynamic-config.rs/readiness` | `"true"` | whether the webhook attaches a readiness probe to the injected container. Pod readiness is AND-ed across containers, so a Service sends no traffic to a pod whose configuration has not arrived. `"false"` opts out, for a deployment that would rather start |
| `dynamic-config.rs/max-document-bytes` | `8388608` | the largest document the agent will accept, checked **before** it is parsed. The injected container's memory limit is 64Mi by default and a document is held more than once while it is resolved |

## Dynamic secrets

For a Vault path that *mints* a credential rather than storing one —
`database/creds/…`, `pki/issue/…`, `aws/creds/…`.

| annotation | default | meaning |
|---|---|---|
| `dynamic-config.rs/dynamic` | `"false"` | read the path as a dynamic engine: no KV `data/` nesting, and keep the lease Vault answered with. A renewable lease is renewed at 65% of its TTL, spread; one Vault marked `renewable: false` — every `pki/issue` — is never sent a renewal at all and is **re-issued at 90%** instead. A renewal keeps the file; a re-issue is new credentials, so it re-renders. For a certificate, whichever expires first binds: the lease, or the certificate's own `notAfter`. [Vault](vault.md#dynamic-secrets) has the whole of it |
| `dynamic-config.rs/revoke-grace` | `5s` | how long the agent may spend handing the lease back. Refused past the pod's `terminationGracePeriodSeconds` — beyond that the kubelet sends SIGKILL, the revocation is cut off mid-request, and the lease stays out anyway, which is what the annotation was set to avoid |
| `dynamic-config.rs/revoke-on-shutdown` | `"true"` | give the lease back on SIGTERM. Best-effort with a short deadline: a pod that cannot reach Vault while terminating still terminates. Setting it without `dynamic` is refused — there is no lease to revoke |

Vault revokes an expired lease on its own eventually; revoking on the way
out is what makes *eventually* into *now*. See [Vault](vault.md).

## Reaching the store over TLS

Two settings beside `ca-configmap` and `tls-secret`, and they are not the
same kind of thing: one moves which name is checked, the other stops
checking.

| annotation | default | meaning |
|---|---|---|
| `dynamic-config.rs/tls-server-name` | the endpoint's host | the name the store's certificate must carry, for an endpoint written as an address it does not name — a Service's cluster IP, a load balancer, a `NodePort`. **The server stays authenticated**: the certificate still chains to a trusted authority and still has to carry this name |
| `dynamic-config.rs/tls-skip-verify` | `"false"` | connect **without authenticating the store at all** |
| `dynamic-config.rs/tls-reload` | `"true"` | rebuild the store's client when `ca-configmap`, `tls-secret` or `ssh-secret` changes on disk — **without restarting the pod**. `"false"` keeps the old client until something restarts it |

**Not every store can express either.** The clients differ, and a store that
cannot say something refuses the whole configuration by name rather than
ignoring it:

| store | `tls-server-name` | `tls-skip-verify` |
|---|---|---|
| vault, consul, firestore | no | yes |
| config-server | yes | yes |
| git | no | yes |
| etcd | yes | no |
| redis, nats, s3 | no | no |

The two columns are almost disjoint, and that is the clients rather than a
choice: `ureq` can turn verification off and cannot override a name,
`tonic` can override a name and cannot turn verification off.

### What `tls-skip-verify` costs

It is not a weaker TLS. It is TLS without the part that makes it mean
anything: any party on the network path can present any certificate, read
what is sent, and rewrite what comes back — and what comes back from a store
is the configuration and the credentials this pod is about to run on.

So it is the one annotation in this contract a workload cannot reach on its
own. Four things gate it:

- an administrator must set `webhook.allowTlsSkipVerify: true`, or the
  annotation is refused with that value named
- it is refused alongside `ca-configmap`: naming an authority and then not
  checking it are two answers to one question
- every pod using it earns an **admission warning**, which `kubectl` prints
- the agent logs it at start and reports
  `dynamic_config_agent_tls_verification_skipped 1`, so one alert finds
  every pod doing it

### Rotation, without a restart

The kubelet rewrites a mounted ConfigMap or Secret **in place**: the file
the container sees changes and nothing tells the process. Every store client
in this family reads its trust material once, when it builds — so before
0.3.0 a rotated CA meant a pod restart, and a rotation nobody restarted for
meant a store that stopped answering at a moment unrelated to the rotation.

The agent now watches the files it was given and rebuilds the client when
they move. **A rebuild is not a restart**: the process stays up, the
rendered file never leaves the volume, the last-known-good stays, and every
counter keeps counting — only the client is new.
`dynamic_config_agent_tls_reloads_total` counts them, and a fleet where that
stays at zero through a CA rotation is a fleet that did not notice.

A rotation writes more than one file, so a change is confirmed by reading
twice a quarter-second apart: rebuilding a TLS client from a half-written
certificate and key is a failure that reads as a bad certificate.

Tokens needed none of this and never did — the projected service-account
token and the config server's bearer file are re-read on **every use**, so
the rotation that actually happens hourly was always picked up.

**Try `ca-configmap` first.** The two situations people usually reach for
skip-verify in — a development server with a self-signed certificate, an
enterprise private CA — are both one more certificate to trust, which is one
annotation and keeps the server authenticated.

## Telling the application what it is running

| annotation | default | meaning |
|---|---|---|
| `dynamic-config.rs/meta` | `"false"` | write a sibling `.<name>.meta` beside the render — `/config/app.yaml` gets `/config/.app.yaml.meta` — holding the digest of the bytes, the store's own revision, and when it landed. It describes the render and never contains it: no values, ever |
| `dynamic-config.rs/schema-configmap` | none | `<name>` or `<name>/<key>` (default key `schema.json`): a JSON Schema the resolved document must satisfy **before** it is published. A document that fails is refused and the last good one keeps serving, which is the behaviour a consumer that is not Rust, Python or Node cannot get any other way |

## Telling the application the file moved

```yaml
    dynamic-config.rs/notify-http: "http://127.0.0.1:8080/-/reload"
```

The rename is atomic, so a consumer never sees half a document — but nobody
tells it the document changed. nginx, Prometheus and most legacy daemons
reload on a request and on nothing else.

After the rename, never before: the whole promise of the notification is
that the document is already there when it arrives. One attempt, a two
second deadline, and never fatal — the file is already correct, so a
notification that did not land has undone nothing. `notifications_total` and
`notification_failures_total` say how it went.

**Localhost only, by construction.** `http://127.0.0.1`, `http://localhost`
or `http://[::1]`, and nothing else — an agent that will POST to an
arbitrary URL is an SSRF primitive holding this pod's store credential. The
address is checked at admission *and* in the agent, because the two run in
different places. Refused with `mode: init`, where the container writes once
and exits before the application it would notify has started.

There is **no signal form**. Signalling a sibling container needs
`shareProcessNamespace: true` — a pod-wide change to the process boundary
between containers, which the webhook will not make on a workload's behalf.

## Failures where somebody is already looking

```yaml
    dynamic-config.rs/events: "true"
```

`kubectl describe pod` is where an operator looks first, and a render
failure was not there — it was in the sidecar's log, one
`kubectl logs -c dynamic-config-agent` away from the question. With this the
agent writes a `Warning` Event on its own pod for a render that failed
(`RenderFailed`) and for a document that vanished from the store
(`DocumentAbsent`).

**Off, and twice opt-in.** The sidecar carries no API credential in any
other configuration — that is a property the rest of this design leans on —
so writing Events means mounting the pod's service-account token beside the
application. An administrator has to create the Role (`agent.events.enabled`
with the namespaces, which grants `create` on `events` and nothing else) and
set `webhook.allowEvents`, and only then may a pod ask. Asking without the
installation offering it is refused, with the chart value named, rather than
admitted into a 403 at the first failure.

An Event is commentary on work that already happened, so it is written off
the loop: a render never waits on the API server, and an Event that could
not be written is logged once and dropped.

## Some of the fleet before all of it

```yaml
    dynamic-config.rs/canary-configmap: "rollout"    # or "rollout/percent"
```

```sh
kubectl create configmap rollout --from-literal=percent=5
# look at the five per cent
kubectl patch configmap rollout --type merge -p '{"data":{"percent":"100"}}'
```

A change published to every pod at once either works or is an incident.
This buys the third outcome: a few pods take it, somebody looks, and the
rest follow or do not.

**Which pods** is the pod's own name hashed into a bucket from 0 to 99, so
the cohort is deterministic and stable — a cohort that reshuffled as it
widened would put every pod through the new document eventually and prove
nothing about any of them. No coordination and no leader: five thousand
agents each answer for themselves.

**Who widens it** is whoever edits the ConfigMap. That is why the
percentage is a mounted file and not an annotation: the kubelet rewrites a
mount in place, so the cohort grows with **no pod restart** — and a restart
would discard the very state a canary exists to watch. A pod outside the
cohort holds what it fetched and publishes it the moment the number passes
its bucket, so nothing has to be re-fetched and the store need not say
anything again.

`0` holds everybody and `100` holds nobody; both ends read as what they
mean. A file that is missing or does not parse is **no canary at all**
rather than zero — a typo must not freeze the fleet on its current
document.

Two series say what is happening: `canary_holding` is 1 while this pod is
outside the cohort, and `canary_percent` is the number it last read.
Together with `applied` from below they are the question somebody has to
answer before widening: *did the pods that took it actually run it?*

Refused with `mode: init`, where the agent publishes once and exits.

**One interaction to know.** A held pod's document is by definition not the
newest, so `max-staleness` will eventually call it stale. That is correct —
it is stale — but a long-running canary under a short staleness ceiling
will make held pods unready, and the two numbers should be chosen together.

## Knowing the application actually applied it

```yaml
    dynamic-config.rs/meta: "true"          # so the app can read the digest
    dynamic-config.rs/require-ack: "true"   # and readiness waits for it
```

`renders_total` says a document reached disk. It says nothing about whether
the application read it, and an application still running the previous one
while every dashboard reports success is the outage this closes.

The application POSTs the fingerprint it is running to `/applied` on the
metrics port:

```sh
curl -fsS -XPOST --data-binary @- http://127.0.0.1:9110/applied   <<< "$(jq -r .fingerprint /config/.app.yaml.meta)"
```

`200` means that is the current document, `409` means a different one is
published and names it — a restarted application acknowledges what it read
before the restart, and a slow one acknowledges a generation the store has
already replaced. Neither is an error the application caused, and neither is
convergence.

A Rust, Python or Node application does not need the meta file: `fingerprint()`
is the same string, from the same digest.

Three series come out of it, and the third is the one to alert on:

| series | what it says |
|---|---|
| `acks_total` | acknowledgements received |
| `ack_mismatches_total` | acknowledgements naming a document this agent never published. Climbing steadily means acknowledgements and renders are talking past each other |
| `applied` | 1 when the application is running what was published |
| `unapplied_seconds` | how long the published document has gone unacknowledged |

**`require-ack` makes that readiness.** The pod stays `0/2` until the
application says it applied the document, so a Service sends no traffic to a
pod running configuration nobody confirmed. Off by default and refused
without a readiness probe or on `mode: init`: it needs the application's
cooperation, and one that never acknowledges would never become ready.

An application that never acknowledges is not penalised. It leaves `applied`
at zero and nothing else changes.

## What the file was before

```yaml
    dynamic-config.rs/history: "3"
```

The rename that publishes a new document is the same rename that destroys
the old one, so *what was the file before?* — the question an incident
starts with — has no answer by the time anybody asks it. This keeps the
replaced generation beside the render, under
`/config/.app.yaml.history/<when>-<digest>.yaml`, newest kept and oldest
pruned past the count.

At most ten, and **off unless asked**: the rendered volume is the pod's own
memory by default, so every kept generation is charged to a limit that is
64Mi by default. Each copy takes the mode the render had, so a history entry
cannot be read by anything the document itself could not be.

Refused with `volume-medium: disk`, where a replaced *secret* would sit on
node-backed storage, outlive the pod that held it, and survive a reboot.

**This is not a rollback.** Putting an old document back needs the
application to say the new one is bad, and nothing here can hear that — half
a feature that implies the other half is worse than neither. This keeps
files; a person reads them.

## When something else writes to the rendered file

```yaml
    dynamic-config.rs/on-drift: "repair"     # or warn (default), fail
```

The agent owns the file; the volume is shared. A debug session, an init
container with an opinion, an application that rewrites its own
configuration — any of them can change it, and until the next change arrives
from the store, nothing notices.

Checked on the same cadence the store is read. `warn` is the default because
the agent cannot know whether the write was a mistake or the point; what it
can do is stop the difference being invisible. `repair` writes the rendered
document back, for a file whose contents are the store's and nobody else's.
`fail` ends the agent, so the pod restarts and renders again.

Either way `dynamic_config_agent_drift` goes to 1 and `drift_total` counts
it. An unreadable file counts as drifted too: one that has been deleted, or
has become a directory, is not the file this agent wrote either.

## Several files, one fetch

`also.<name>` cuts more files from the **same fetched document**, and
publishes them all or none of them:

```yaml
dynamic-config.rs/source: "vault"
dynamic-config.rs/key: "secret/myapp"
dynamic-config.rs/path: "/config/app.yaml"
# the same document, a section of it, as a second file:
dynamic-config.rs/also.db: "/config/db.env"
dynamic-config.rs/also-section.db: "database"
```

One fetch, one generation, one refusal if any of them cannot be written —
so a failure in the third does not leave the first two published. Each
write is still its own atomic rename: a reader can catch the microseconds
between two of them, which is a rename apart rather than a fetch apart.

This is *within* one document. Several **stores** cannot share a
generation — two stores have no common instant and no protocol between
them can say "these two reads are the same" — so a second store stays a
named render, below.

## Several documents, one pod

Every store-shaped key accepts a **`.<name>` suffix**, and each name is
one more injected agent writing one more file into the same directory:

```yaml
# the default render:
dynamic-config.rs/source: "vault"
dynamic-config.rs/key: "secret/myapp"
dynamic-config.rs/path: "/config/app.yaml"
dynamic-config.rs/auth: "kubernetes"
# a second, named `cache`:
dynamic-config.rs/source.cache: "redis"
dynamic-config.rs/endpoint-secret.cache: "redis-url/url"
dynamic-config.rs/key.cache: "myapp/cache.json"
dynamic-config.rs/path.cache: "/config/cache.toml"
```

Per-name: everything a store needs — source, endpoint(+secret), key,
path, section, watch cadence, every auth key, CA/TLS/ssh material
(mounted under suffixed paths), templates, file-mode. Pod-wide, on
purpose: `mode`, volume medium, resources, run-as identity,
`env-inject` (the default render's file is the one that can become the
environment). Two rules, refused with the fix named: every path lives
in the **default path's directory** (one shared volume), and each
name's `source`/`key`/`path` are required exactly like the default's.
Container names follow the suffix — `dynamic-config-agent-cache` in
`kubectl get pod`, so a broken render says which one it is.

## Authentication

Each store takes its own methods; the store's page spells every one of
them out with full manifests. The webhook forwards these to the agent
verbatim, and the agent refuses a wrong combination at startup — in the
pod's events, not as a store error twenty minutes later.

| annotation | meaning |
|---|---|
| `dynamic-config.rs/auth` | the method: consul `token \| kubernetes \| jwt`; vault `token \| kubernetes \| approle \| jwt \| userpass \| ldap \| cert`; firestore `metadata-server \| access-token \| emulator`; git `anonymous \| token \| ssh-key` |
| `dynamic-config.rs/auth-mount` | vault: the auth method's mount path when not the default; consul: the auth method's **name** (required for `kubernetes` and `jwt`) |
| `dynamic-config.rs/auth-role` | vault `kubernetes`: the role to assume (required); vault `approle`: the role id; vault `jwt`/`cert`: optional |
| `dynamic-config.rs/auth-username` | vault `userpass`/`ldap`: the user; git: the basic-auth user when the host wants one |
| `dynamic-config.rs/auth-token-path` | where the service-account token is mounted, when a projected volume moved it |
| `dynamic-config.rs/namespace` | the Vault namespace (Vault Enterprise) |
| `dynamic-config.rs/ref` | git: `main`, `branch:main`, `tag:v1.4`, or `commit:<sha>` |
| `dynamic-config.rs/api-url` | firestore: the API endpoint when it is not Google's — the emulator |

## Secrets and certificates

Secret material never rides an annotation — `kubectl describe pod`
prints annotations and arguments to anyone with pod read access. These
four name Kubernetes objects instead; the webhook mounts them and the
agent reads them. [The geography is fixed](secrets-and-tls.md).

| annotation | form | becomes |
|---|---|---|
| `dynamic-config.rs/token-secret` | `<secret>/<key>` | env `DYNAMIC_CONFIG_AGENT_TOKEN` |
| `dynamic-config.rs/password-secret` | `<secret>/<key>` | env `DYNAMIC_CONFIG_AGENT_PASSWORD` — the approle secret id, the userpass/ldap password |
| `dynamic-config.rs/endpoint-secret` | `<secret>/<key>` | env `DYNAMIC_CONFIG_AGENT_ENDPOINT` |
| `dynamic-config.rs/ca-configmap` | `<name>` or `<name>/<key>` (default key `ca.crt`) | a read-only mount under `/etc/dynamic-config/ca` and the agent's `--ca` |
| `dynamic-config.rs/tls-secret` | `<name>` — a `kubernetes.io/tls` Secret | a read-only mount under `/etc/dynamic-config/tls` and `--tls-cert`/`--tls-key` (that Secret type fixed its two keys as `tls.crt`/`tls.key`) |
| `dynamic-config.rs/ssh-secret` | `<name>` or `<name>/<key>` (default key `ssh-privatekey`, the `kubernetes.io/ssh-auth` convention) | a `0400` mount under `/etc/dynamic-config/ssh`, `--ssh-key`, and `auth: ssh-key` implied when no auth was named |

## Value forms, source by source

What `endpoint`, `key` and `auth` take for each value of `source` — the
store pages carry the full manifests, this table is the lookup:

| `source` | `endpoint` | `key` | `auth` values |
|---|---|---|---|
| `consul` | `http(s)://host:8500` | KV path with extension: `myapp/config.json` | *(none)*, `token`, `kubernetes`, `jwt` |
| `vault` | `http(s)://host:8200` | `<mount>/<path>`: `secret/myapp` | *(none = token)*, `token`, `kubernetes`, `approle`, `jwt`, `userpass`, `ldap`, `cert` |
| `config-server` | `http(s)://host:8888` | `<application>/<profile>`: `billing/prod` | *(none — bearer via `token-secret` only)* |
| `firestore` | `<project>` or `<project>/<database>`: `acme-prod` | `collection/document`: `config/billing` | *(none = metadata-server)*, `metadata-server`, `access-token`, `emulator` |
| `git` | any clone url: `https://…`, `git@host:org/repo.git` | file path in the repository: `billing/prod.yaml` | *(none = token if set, else anonymous)*, `anonymous`, `token`, `ssh`, `ssh-key` |
| `redis` | `redis://` / `rediss://` url — via `endpoint-secret` when it carries a password | key with extension: `myapp/config.json` | *(none — credentials live in the url)* |
| `etcd` | (no `auth` key) | `tls-secret` client certificates, or `auth-username` + `password-secret` — etcd's own two methods, both first-class | `--key` is the etcd key |
| `nats` | (no `auth` key) | a `.creds` file via `auth-token-path`, or `token-secret`; anonymous otherwise | `--key` is `<bucket>/<key>` |
| `s3` | (no `auth` key) | the ambient AWS chain — IRSA on EKS, the workload's own identity | `--endpoint` is the bucket; `api-url` overrides for MinIO/Ceph/R2 |

## The prefix is claimed territory

Every `dynamic-config.rs/*` annotation must be a key this page lists —
an unknown one **fails the admission**. The rule exists for the typo:
`tokne-secret` silently ignored would be a pod running without the
authentication it declared, and nobody would know until the audit.
Annotations outside the prefix are none of this webhook's business and
pass untouched.

### Retiring a key

Nothing here is deprecated. When something is, the contract has somewhere to
say so: every key is one row of a registry inside the webhook, carrying the
release that retired it and what to write instead. A pod that sets a retired
key is **admitted with a warning** naming its replacement, rather than
refused — a contract that breaks a working pod to make a point is a contract
people pin an old version of.

Two properties come from the key list being one table rather than several.
Whether a key may take a `.name` suffix is read off the same row, so the two
can no longer disagree; and the documentation is checked against that table
by a test, so a key cannot be accepted and left undocumented.

The one liberty the strictness buys back: because every key is
validated, `template` and `template-configmap` could ship later without
a migration — pods that used them early were refused, not silently
ignored. They shipped; the [Rendering page](rendering.md#templates)
owns them.

## What fails the admission

**A wrong ask fails the admission.** A pod that says `inject: "true"`
and misspells the rest is refused with the reason, not started without
its configuration — silence there is how an outage begins. The refusals,
verbatim from the tests:

- `inject` set to anything but `"true"`/`"false"`
- a missing required annotation, named in the message
- `mode` outside `init | sidecar | both`
- `watch-seconds` that does not parse as whole seconds
- a `*-secret` value without the `<secret-name>/<key>` slash
- `endpoint` and `endpoint-secret` both set — one address, one place
- `ssh-secret` alongside an `auth` other than `ssh-key`
- `volume-medium` outside `memory | disk`; `native-sidecar` outside
  `true | false`
- a resource annotation that is not a Kubernetes quantity
- `file-mode` outside octal `0400`–`0777` — setuid bits answer no
  question, and an owner-unreadable file is write-only noise
- `agent-run-as-user`/`-group` of `0` — the agent stays nonroot in
  every configuration
- `inject-containers` naming a container the pod does not have, or set to
  nothing at all — a render nothing can read is a render nobody asked for
- `init-first` with `mode: sidecar`, where there is no init container
- `agent-run-as-same-user` with no `runAsUser` to inherit, alongside
  `agent-run-as-user`, or on an application that runs as root
- `revoke-grace` past the pod's `terminationGracePeriodSeconds`, at `"0"`,
  or without `dynamic`
- `notify-http` at anything but a localhost address, or with `mode: init`
- `on-drift` outside `warn | repair | fail`
- `history` at `"0"`, past ten, or alongside `volume-medium: disk`
- `agent-ephemeral-request` larger than `agent-ephemeral-limit`
- `timeout` at `"0"`, which is no deadline rather than the store's own
- `agent-image` naming an image no `webhook.agentImageAllow` prefix admits
- `require-ack` without a readiness probe, or with `mode: init`
- `canary-configmap` with `mode: init`
- `events` where the installation does not offer it — the refusal names the
  chart value an administrator sets
- `class` naming one that is not visible, one whose `namespaces` list
  excludes this pod, or one whose credential is in another namespace
- `tls-skip-verify` where the installation does not offer it, or alongside
  `ca-configmap`
- `env-inject` naming a container that `inject-containers` leaves out: the
  wrapper sources the rendered file, so it has to be able to read it
- `env-inject` with `mode: sidecar`, a container the pod does not
  have, or a container with no explicit `command`
- `env-restart` without `env-inject`, without `mode: both`, or on a
  container that already owns a `livenessProbe`
- `agent-env` entries that are not `NAME=value`, names that are not
  UPPER_SNAKE, a name set twice, or a name that shadows what
  `aws-secret` already sets
- an `agent-env` name outside the installation's allowlist for the
  pod's namespace — the refusal names the chart value that opens it
- a source `sourceDeny` turns off in the pod's namespace, or one
  missing from a non-empty `sourceAllow` — checked on every render,
  named suffixes included
- an annotation that differs from a PINNED installation value
  (`!`-marked, or any set value under `overridable: "false"`) — the
  refusal names both values; restating the pinned value passes
- any `dynamic-config.rs/*` key the contract does not list
- `template` and `template-configmap` both set — one template, one
  place

## What passes with a warning

Not every misconfiguration earns a refusal. Kubernetes lets an
admission response carry `warnings`, which `kubectl` prints and a
controller records, and four configurations earn one:

| the pod said | the warning |
|---|---|
| `volume-medium: disk` | the rendered document is on node-backed storage, where it outlives the pod and is readable by anything that can read the node's disk |
| a world-readable `file-mode` | every container in the pod can read it, including ones added later |
| `watch-seconds` below 5 | every replica polls the store at that rate, and a store with a native watch delivers changes without one |
| `dynamic` with `revoke-on-shutdown: "false"` | the credential stays valid after the pod is gone, until its lease expires |

The bar is high on purpose: a warning on every admission is a warning
nobody reads. Each of these is a configuration that works and is probably
not what was meant.

## Checking a manifest before the cluster does

The webhook is a pure function of a pod and an installation, so the same
decision is available without a cluster:

```console
$ dynamic-config-webhook validate pod.yaml deployment.yaml
pod.yaml: allowed, and an agent would be injected
deployment.yaml: refused (InvalidAnnotation)
  dynamic-config.rs/auth-role is required for vault kubernetes auth
```

JSON or YAML, one exit code for the lot, installation defaults from the
process's own environment — so a CI job that runs the webhook's image with
the chart's environment gets the answer the cluster would give. The point
of catching it here is that the alternative is catching it from a rollout
that will not start.

Whatever passes the webhook is validated again by the agent, which
knows the store-by-store rules (vault `kubernetes` needs `auth-role`,
consul `kubernetes` needs `auth-mount`, a certificate needs its key).
Those refusals land in the injected container's log and the pod's
events.

`DynamicConfigClass` shrinks all of this to a class reference —
the [operator page](operator.md) carries the shape.

## The agent-env gate

`agent-env` puts variables on the container that holds store
credentials, and environment steers SDKs — `HTTPS_PROXY` reroutes the
agent's traffic, `AWS_CA_BUNDLE` and `SSL_CERT_FILE` swap its trust
roots. So the names that may pass are the **installer's** decision,
declared once:

```yaml
# values.yaml
webhook:
  agentEnvAllow: "payments: HTTPS_PROXY, AWS_*; *: RUST_LOG"
```

Semicolons separate groups; each group takes an optional
`namespace:` head (absent or `*` means every namespace); a trailing
`*` on a name is a prefix glob. Empty — the default — refuses the
annotation everywhere. Kustomize installs set the same grammar in the
`DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW` variable, and either way the
webhook validates it at startup and refuses to serve on a typo.

The gate is NOT a namespace annotation, by design: whoever edits a
namespace is usually the tenant being gated, and a gate its subject
can open is not a gate. It is also not read from the namespace object
at admission time — the webhook holds no RBAC and asks the API server
for nothing; the pod's namespace arrives inside the AdmissionReview,
and the ruling comes from the webhook's own configuration.

## The source gates

The same authority model gates which STORES a pod may use:

```yaml
webhook:
  sourceAllow: "payments: vault, s3; *: consul"   # empty = every store
  sourceDeny: "sandbox: git"                       # subtractive, wins
```

`sourceAllow` empty means every store everywhere — the safe default
for an upgrade; non-empty means ONLY the listed, per namespace.
`sourceDeny` turns stores off outright and outranks the allowlist.
Both cover every render on the pod — a denied store cannot ride in on
a [named suffix](#several-documents-one-pod) — and both refusals name
the namespace and the value that opens the gate. Kustomize sets
`DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW` / `_SOURCE_DENY`; a name that is
not a real store fails webhook startup, so a typo cannot silently
gate nothing.

## Defaults come in tiers

Every default in the table above is only the LAST tier of four:

```text
annotation  >  per-store default  >  fleet default  >  built-in
```

The middle tiers are the installation's ([the full
list](install.md#fleet-wide-defaults-validated-at-the-door)): fleet
defaults cover every knob — resources, `file-mode`, `watch-seconds`,
`mode`, `volume-medium`, `native-sidecar`, `agent-run-as-user`/`-group`,
`metrics-port`, `path`, `source`, and a fleet-wide agent environment —
and `agent.defaults.perStore` sets, one store at a time, the same
knobs PLUS every store-shaped annotation: `endpoint`, `key`, `auth`
and its friends, the credential Secrets, the templates. A pod can
deploy carrying nothing but `inject: "true"`. Pod-wide knobs (mode,
volume, resources, identity) take the DEFAULT render's store tier;
per-render knobs resolve against each render's own store. Every tier
is validated with the SAME rules as the annotation it stands in for,
at webhook startup — and any value can be PINNED (`!`, or
`overridable: "false"`), refusing a differing annotation instead of
being overridden by it.

[Installation Defaults and Gates](installation-defaults.md) carries
the full knob table, a per-store example for all nine stores, and the
gates in depth.
