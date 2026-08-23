# Threat model

What this integration is trusted with, what it is not, and what an attacker
gets from each thing they might compromise.

The material is scattered across `SECURITY.md` and the book's security page,
which answer *what the posture is*. This answers the other question: **if
this piece falls, what falls with it?**

Scope is the four binaries here — webhook, agent, operator, node agent —
and the objects they create.
The engine's own threat surface — parsing, redaction, the last-known-good
cache — is the [engine's security policy](https://github.com/dynamic-config-rs/dynamic-config/blob/main/SECURITY.md).

## Assets

| Asset | Where it lives | Why it matters |
|---|---|---|
| Store credentials | Kubernetes Secrets, mounted into the agent as environment or files | They authenticate to Vault, Consul, S3. Their blast radius is whatever the store's policy grants — usually more than one workload's configuration |
| The rendered document | an `emptyDir`, `medium: Memory` by default | It frequently *is* a credential: a database password, an API key, a certificate |
| Dynamic-secret leases | in the agent's memory, and in Vault's lease table | A lease id is capability-shaped: holding one is enough to renew or revoke that credential |
| The webhook's TLS key | a Secret, or memory under `selfRotate` | It authenticates the webhook to the API server. Whoever holds it can answer admission requests |
| The admission decision | the webhook's response | It writes containers, volumes, mounts and arguments into other people's pods |
| `ClusterDynamicConfigClass` credentials | a Secret in the operator's namespace | One credential, many tenants — the confused-deputy asset |
| Every store credential on a node | the node agent's memory, when it runs | One process for a whole node; its blast radius is the node rather than a workload |

## Trust boundaries

```text
   ┌──────────────────────────────────────────────┐
   │ cluster administrator                        │
   │   chart values, installation document,       │
   │   ClusterDynamicConfigClass                  │
   └───────────────┬──────────────────────────────┘
                   │  pins, allowlists, denylists
   ┌───────────────▼──────────────────────────────┐
   │ namespace owner                              │
   │   pod annotations, namespaced Class          │
   └───────────────┬──────────────────────────────┘
                   │  admission
   ┌───────────────▼──────────────────────────────┐
   │ the pod                                      │
   │   agent container │ application containers   │
   └───────────────┬──────────────────────────────┘
                   │  network
   ┌───────────────▼──────────────────────────────┐
   │ the store                                    │
   └──────────────────────────────────────────────┘
```

The boundary that carries the most weight is the second: **a namespace owner
must not be able to reach configuration the administrator did not grant
them.** Source allowlists, pinned installation values, the agent-env
allowlist and the `tls-skip-verify` gate are all that boundary.

## What each compromise reaches

### A compromised application container

Reaches: the rendered document, because it is mounted there. That is the
design — the document is what the application is for.

Does **not** reach: the store credential. It is environment and files on the
*agent* container, and containers do not share either.

Narrows with: `inject-containers`, which mounts the rendered volume only
into the containers named. In a pod running a log shipper or a mesh proxy
beside the application, those two stop holding the credential. `file-mode`
does not help here — a sidecar usually runs as the same UID.

### A compromised agent container

Reaches: the store credential, and therefore everything the store's policy
grants that credential. Also every lease it holds.

Does **not** reach: the Kubernetes API. The agent carries no service-account
token of its own beyond what the pod already had, and it holds no RBAC. It
also cannot write outside its rendered volume and its own read-only mounts.

Narrows with: a store policy scoped to the paths this workload needs.
`ClusterDynamicConfigClass` credentials are the wide ones — see the
confused-deputy note below.

### A compromised webhook

Reaches: every pod created while it is compromised. It can inject a
container of its choosing, mount volumes, and set arguments — which is to
say it can exfiltrate anything those pods are given.

Does **not** reach: existing pods, and the Kubernetes API beyond three
narrow grants — `secrets` **by resourceName**, `leases` by resourceName for
election, and its own MutatingWebhookConfiguration. Its ServiceAccount sets
`automountServiceAccountToken: false`, and the admission path itself calls
the API server **not at all**.

Detected by: the admission audit line the webhook writes for every decision,
and the injected image being a digest an admission policy can pin.

### A compromised node agent

Reaches: **every store credential every pod on that node uses**, and every
document they read. This is the one component whose compromise is not
bounded by a workload, and the reason it is off by default.

It also runs as root and mounts the kubelet's directories, so a compromise
here is a compromise of every pod's configuration volume on that node.

Narrows with: not running it. The sidecar shape exists and is the default;
this is the escape hatch for a scale where 25,000 sidecar containers is the
larger problem, and choosing it should follow a measurement rather than a
preference.

### A compromised operator

Reaches: the ConfigMaps and Secrets it manages, and the store credentials in
every Class it can read. A cluster-scoped Class makes this the widest
compromise in the system.

Narrows with: namespaced Classes where the tenancy allows it, and target
Secrets that no Class reaches by wildcard.

### A network attacker between the agent and the store

Reaches: nothing, normally — TLS authenticates the store and there is no way
to turn that off from a pod.

**Unless `tls-skip-verify` is on.** Then this attacker reaches everything:
they supply the configuration the pod runs on and the credentials it uses.
The annotation is refused unless a cluster administrator turned it on
installation-wide, it emits an admission warning on every use, and every pod
using it reports `dynamic_config_agent_tls_verification_skipped 1` — an
alert on that gauge is the control.

## The confused deputy

The sharpest structural risk here, and it is worth naming rather than
leaving implicit.

A `ClusterDynamicConfigClass` holds one credential and admits many
namespaces. A tenant who can create a `DynamicConfigRender` in an admitted
namespace chooses the `key` — so a tenant in `team-a` can name
`prod/payments/db` and receive it, under the platform's credential, with no
policy in the store having said yes.

Today the answer is operational: **scope the Class credential to what its
tenants may read**, and prefer namespaced Classes where the tenancy allows.
A `key` prefix policy on the Class would make this structural, and it is not
built — it is the first thing to build if this integration grows multi-tenant
users.

## What is out of scope

- **A compromised Kubernetes API server or etcd.** Everything here trusts
  the API server; a Secret is a base64 string in etcd, which is the
  platform's encryption-at-rest problem and not this integration's.
- **A malicious cluster administrator.** They install the webhook.
- **The store's own authorization.** Whether `myapp` may read
  `prod/payments/db` is Vault's policy engine, and duplicating that decision
  here would create a second answer that could disagree.
- **Side channels between containers in a pod.** Shared kernel, shared node,
  `shareProcessNamespace` if somebody set it. That boundary is Kubernetes'.

## Reporting

Security issues go to the address in [`SECURITY.md`](SECURITY.md), not to
the issue tracker.
