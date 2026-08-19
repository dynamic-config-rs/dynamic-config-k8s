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

## Behaviour

| annotation | default | meaning |
|---|---|---|
| `dynamic-config.rs/watch-seconds` | `15` | sidecar poll interval, whole seconds |
| `dynamic-config.rs/section` | whole document | the section key the document nests under |
| `dynamic-config.rs/native-sidecar` | `"false"` | `"true"` injects the watcher as an init container with `restartPolicy: Always` (Kubernetes 1.29+); [Jobs finish](security.md#native-sidecars) |
| `dynamic-config.rs/volume-medium` | `memory` | where the rendered file lives: `memory` (tmpfs, off the node's disk) or `disk` |
| `dynamic-config.rs/agent-cpu-request` | `10m` | the injected container's CPU request |
| `dynamic-config.rs/agent-memory-request` | `32Mi` | its memory request |
| `dynamic-config.rs/agent-cpu-limit` | none | its CPU limit — none by default, on purpose |
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
- any `dynamic-config.rs/*` key the contract does not list
- `template` and `template-configmap` both set — one template, one
  place

Whatever passes the webhook is validated again by the agent, which
knows the store-by-store rules (vault `kubernetes` needs `auth-role`,
consul `kubernetes` needs `auth-mount`, a certificate needs its key).
Those refusals land in the injected container's log and the pod's
events.

`DynamicConfigClass` (0.3.0) shrinks all of this to a class reference —
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
`metrics-port`, and a fleet-wide agent environment — and
`agent.defaults.perStore` sets the same knobs one store at a time
(`vault: "watch-seconds=30, file-mode=0640"`). Pod-wide knobs (mode,
volume, resources, identity) take the DEFAULT render's store tier;
per-render knobs (`watch-seconds`, `file-mode`) resolve against each
render's own store. Every tier is validated with the SAME rules as the
annotation it stands in for, at webhook startup.

[Installation Defaults and Gates](installation-defaults.md) carries
the full knob table, a per-store example for all nine stores, and the
gates in depth.
