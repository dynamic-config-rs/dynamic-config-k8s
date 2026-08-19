# The Security Posture

Everything this integration does to a cluster, listed where an auditor
can find it. The one-line summary: **injection never relaxes a pod's
posture, secrets never appear where `kubectl describe` reaches, and the
webhook holds no credentials at all.**

## The injected agent complies with restricted PSS

Every injected container — init, sidecar, or native sidecar — carries
the full [restricted Pod Security Standard](https://kubernetes.io/docs/concepts/security/pod-security-standards/)
posture, so injection works in namespaces that enforce
`pod-security.kubernetes.io/enforce: restricted` and passes the audit
in ones that only warn:

```yaml
securityContext:
  runAsNonRoot: true
  runAsUser: 65532          # distroless nonroot
  runAsGroup: 65532
  allowPrivilegeEscalation: false
  capabilities: { drop: ["ALL"] }
  readOnlyRootFilesystem: true
  seccompProfile: { type: RuntimeDefault }
```

The root filesystem is read-only because the agent writes exactly one
place: the shared volume. Which is —

## The rendered file lives in memory

The shared `emptyDir` is `medium: Memory` by default: rendered
configuration regularly carries credentials, and tmpfs keeps them off
the node's disk and out of its backups. The file is gone when the pod
is. A pod that prefers disk (a giant document, a memory-tight node)
says so:

```yaml
    dynamic-config.rs/volume-medium: "disk"
```

Note the accounting: tmpfs pages count against the pod's memory.
Configuration documents are small; the default agent memory limit
below leaves room.

## The agent's resource ask

Injected with requests and limits so it can never be the reason the
node evicts the app:

```yaml
resources:
  requests: { cpu: 10m, memory: 32Mi }
  limits: { memory: 64Mi }        # no CPU limit: throttling a config
                                  # agent buys nothing, delays reloads
```

Four annotations move them per pod: `agent-cpu-request`,
`agent-memory-request`, `agent-cpu-limit`, `agent-memory-limit` — each
a Kubernetes quantity, refused at admission when it is not.

## Native sidecars

On Kubernetes 1.29+, ask for the sidecar as the platform now spells it:

```yaml
    dynamic-config.rs/native-sidecar: "true"
```

The watching agent becomes an init container with
`restartPolicy: Always` — started before the app containers, stopped
after them, and **a Job with one finishes**, where a classic sidecar
would hold it in `Running` forever. With `mode: "both"` the one-shot
init still lands first, so the file-exists-before-the-app guarantee
survives the move.

## Secrets: where each thing is allowed to appear

The [contract's rule](secrets-and-tls.md): names in annotations,
secrets in Secret-backed environment variables, key material in
read-only mounts into the agent container alone. The webhook enforces
it — there is no annotation that accepts a token value, and the
password slot has no flag on the agent at all.

## The webhook holds nothing

- Its ServiceAccount sets `automountServiceAccountToken: false`, and
  the deployment repeats it. The webhook reads the AdmissionReview it
  is handed and answers; it never calls the API server, so it carries
  no credential to steal.
- It terminates TLS in-process with the certificate the chart issued;
  the private key never leaves its mount. Renewals are picked up from
  disk without a restart.
- The optional NetworkPolicy (`networkPolicy.enabled=true`) writes both
  facts down for the CNI: ingress only on 8443, **egress empty**.

## The webhook cannot select itself

The webhook configuration excludes `kube-system`, `kube-node-lease`
and the release's own namespace by name — a mutating webhook that can
select its own pods can deadlock its own rollout, and one that can
mutate the control plane is a cluster risk with no matching reward.
Add more with `webhook.excludeNamespaces`.

## failurePolicy: the whole trade

`Ignore` (default): an unreachable webhook lets pods through
un-injected. The failure is *visible where it matters* — the annotated
pod's application waits for a file that never comes — and invisible
where it does not: un-annotated pods, which are most pods, never notice.

`Fail`: no annotated pod can start un-injected, and no pod at all can
start in selected namespaces while the webhook is down. Two replicas,
a PodDisruptionBudget and topology spread are the chart's mitigations;
they shrink the window, they do not close it.

Start on `Ignore`, alert on the webhook's availability, and flip to
`Fail` when the alert has been quiet long enough to trust.

## Every admission leaves a line

The webhook logs one structured line per decision that matters —
namespace, pod name, source, and `patched` or `refused` — and **no
annotation values**: endpoints and role names belong in the cluster,
not in every log aggregator downstream. Pods that never asked are
counted but not logged.

`GET /metrics` on the serving port exposes the counters in Prometheus
text format:

```text
dynamic_config_admissions_total{outcome="skipped"} 1042
dynamic_config_admissions_total{outcome="patched"} 63
dynamic_config_admissions_total{outcome="refused"} 2
```

A rising `refused` is somebody fighting the contract; alert on it.

## Typos cannot pass

An unknown `dynamic-config.rs/*` annotation fails the admission — the
[reference explains the rule](annotations.md#the-prefix-is-claimed-territory).
The enterprise version of the argument: a misspelled `token-secret` that
is silently ignored produces a pod that runs, connects anonymously, and
reads whatever the store's anonymous policy allows. Refusing at
admission turns a quiet posture downgrade into a loud create-time error.

## Templates are code, and scoped like data

A template renders **only the resolved document** — the same value the
application reads. There is no file access, no environment access, no
network in the template language; a hostile template can misrender the
config file it owns and nothing else. Undefined keys are strict errors,
so a template cannot silently swallow a value either. Keeping templates
in ConfigMaps puts them through the same review as the code they
effectively are.

## Namespace gating

`webhook.namespaceGating=true` flips injection to Istio-style opt-in:
only namespaces labeled `dynamic-config.rs/injection: enabled` are
selected at all. Two things follow:

- the blast radius of the webhook is exactly the namespaces that asked;
- `failurePolicy: Fail` becomes a per-namespace promise — a platform
  team can fail closed for its opted-in tenants without coupling every
  pod CREATE in the cluster to this webhook.

```sh
kubectl label namespace team-a dynamic-config.rs/injection=enabled
```

**A label, and it could not be an annotation**: the gate lives in the
webhook configuration's `namespaceSelector`, and Kubernetes selectors
match labels only — annotations are invisible to them. The alternative
(the webhook reading each pod's Namespace object to check an
annotation) would hand the webhook API access it pointedly does not
have; the zero-RBAC posture outranks the spelling preference. Either
way the per-POD `dynamic-config.rs/inject: "true"` annotation is still
required — the namespace gate is an outer guard, never an implicit
opt-in.

## Fleet-wide agent defaults

The injected container's resource defaults come from the chart
(`agent.defaults.*`), not from a constant in a binary — platform teams
set the fleet's floor once, and the per-pod annotations still override
it. The same values file pins the agent image the webhook injects.

## Supply chain

- Images are distroless, run as `nonroot`, and the chart **refuses
  `tag: latest`** at render time; a `digest` value pins harder than a
  tag can.
- The release workflow signs images with cosign and attaches SBOMs;
  ghcr.io and Docker Hub carry the same digests.
- The agent binary embeds the engine and the store crates from
  crates.io — the same audited path every other binding uses; there is
  no k8s-only fork of anything.

## What this integration never does

No `hostPath`, no `privileged`, no capabilities added, no writes
outside the shared volume, no API server calls from the webhook, no
credentials in flags or annotations, and no way — flag or annotation —
to turn TLS verification off toward any store.
