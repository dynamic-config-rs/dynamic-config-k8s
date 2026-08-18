# The Annotation Contract

v1, and it is the API: a change here is a breaking change of this
integration, whatever the binaries think. Two golden files in the
webhook's test suite byte-compare the full admission response, so the
contract cannot move without a reviewed diff saying it moved.

## The core seven

| annotation | required | meaning |
|---|---|---|
| `dynamic-config.rs/inject` | yes | `"true"` asks; anything else but `"false"` fails the admission |
| `dynamic-config.rs/source` | yes | `consul`, `vault`, `config-server`, `firestore`, `git`, `redis` (etcd, nats, s3: 0.2.0) |
| `dynamic-config.rs/endpoint` | one of the two | the store's address — a url; `<project>[/<database>]` for firestore |
| `dynamic-config.rs/endpoint-secret` | one of the two | `<secret>/<key>` holding the address, when the address carries a password (a redis url) |
| `dynamic-config.rs/key` | yes | the document's key; `mount/path` for vault, `application/profile` for config-server, the file's path for git |
| `dynamic-config.rs/path` | yes | where the rendered file lands; the extension picks the format |
| `dynamic-config.rs/mode` | no | `init`, `sidecar` (default), `both` |

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
| `dynamic-config.rs/template` | none | an inline minijinja template; [it owns the output bytes](rendering.md#templates) |
| `dynamic-config.rs/template-configmap` | none | `<name>` or `<name>/<key>` (default key `template`): the template from a ConfigMap, mounted read-only and re-read every render |

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
| `etcd` | — | — | refused at admission: 0.2.0 (async client) |
| `nats` | — | — | refused at admission: 0.2.0 |
| `s3` | — | — | refused at admission: 0.2.0 |

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
- any `dynamic-config.rs/*` key the contract does not list — including
  `source: etcd|nats|s3`, refused with a message naming the version
  that takes them
- `template` and `template-configmap` both set — one template, one
  place

Whatever passes the webhook is validated again by the agent, which
knows the store-by-store rules (vault `kubernetes` needs `auth-role`,
consul `kubernetes` needs `auth-mount`, a certificate needs its key).
Those refusals land in the injected container's log and the pod's
events.

`DynamicConfigClass` (0.3.0) shrinks all of this to a class reference —
the [operator page](operator.md) carries the shape.
