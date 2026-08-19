# dynamic-config

The injector chart: a mutating webhook that reads `dynamic-config.rs/*`
annotations and writes a rendering agent into annotated pods, plus the
operator's CRDs and, when enabled, the operator itself.

```sh
helm install dynamic-config ./deploy/helm
```

That is the whole install — no dependencies. The chart mints its own
webhook certificate by default; `webhook.certManager.enabled=true`
switches to cert-manager-issued, auto-renewed TLS; and
`webhook.selfRotate.enabled=true` is the third mode — the webhook
mints, rotates and re-trusts its own pair, no dependency AND renewal. The
[book](https://dynamic-config-rs.github.io/k8s/) carries the annotation
contract, the store pages and the security posture; this page is the
values reference.

## Values

### Global

| value | default | meaning |
|---|---|---|
| `nameOverride` / `fullnameOverride` | `""` | resource naming, for two releases side by side |
| `commonLabels` / `commonAnnotations` | `{}` | stamped on every rendered resource |
| `imagePullSecrets` | `[]` | pull credentials, applied to every component |

### Agent (what gets injected)

| value | default | meaning |
|---|---|---|
| `agent.image` / `agent.tag` / `agent.digest` | ghcr; tag empty = `v<appVersion>` | the injected image; `digest` wins over `tag`; `latest` is refused at render |
| `agent.defaults.cpuRequest` | `10m` | fleet-wide default for injected containers |
| `agent.defaults.memoryRequest` | `32Mi` | — |
| `agent.defaults.memoryLimit` | `64Mi` | — |
| `agent.defaults.cpuLimit` | `""` | empty on purpose: throttling a config agent buys nothing |
| `agent.defaults.fileMode` | `""` | fleet-default octal permissions for rendered files (empty = the agent's 0644); per-pod `file-mode` still wins |
| `agent.defaults.watchSeconds` | `""` | fleet-default watch interval, whole seconds (empty = 15); per-pod `watch-seconds` still wins |
| `agent.defaults.mode` | `""` | fleet-default injection mode `init`/`sidecar`/`both` (empty = sidecar) |
| `agent.defaults.volumeMedium` | `""` | fleet-default volume medium `memory`/`disk` (empty = memory) |
| `agent.defaults.nativeSidecar` | `""` | `"true"` makes native sidecars the fleet default (empty = false) |
| `agent.defaults.runAsUser` | `""` | fleet-default agent UID (empty = 65532; 0 refused) |
| `agent.defaults.runAsGroup` | `""` | fleet-default agent GID (same rule) |
| `agent.defaults.metricsPort` | `""` | serve agent metrics on this port by default; pods opt out with `metrics-port: "0"` |
| `agent.defaults.env` | `""` | fleet-wide agent environment, `NAME=value, …`; a pod's `agent-env` overrides name by name |
| `agent.defaults.perStore` | `{}` | per-STORE defaults, one tier above the fleet's — every store-shaped annotation (endpoint, auth, ca-configmap, token-secret, key, template, …) plus every knob: `vault: "endpoint=https://vault:8200!, auth=kubernetes"` |
| `agent.defaults.source` | `""` | fleet default source — a pod may omit `dynamic-config.rs/source` entirely |
| `agent.defaults.path` | `""` | fleet default rendered-file path |
| `agent.defaults.overridable` | `""` | `"true"` (default): annotations override installation values; `"false"`: every installation-SET value is pinned. Per store, `overridable=false` inside a `perStore` group pins that store's values; per value, `!` pins and `?` opens — closest word wins. Pinned conflicts are refused at admission |
| `webhook.agentEnvAllow` | `""` | which `agent-env` names pods may set, per namespace: `"payments: HTTPS_PROXY, AWS_*; *: RUST_LOG"`; empty = refused everywhere |
| `webhook.sourceAllow` | `""` | which stores pods may use, per namespace (empty = every store everywhere; non-empty = ONLY the listed) |
| `webhook.sourceDeny` | `""` | stores turned off, per namespace; outranks `sourceAllow` |

Per-pod `dynamic-config.rs/agent-*` annotations override these.

### Webhook

| value | default | meaning |
|---|---|---|
| `webhook.image` / `.tag` / `.digest` / `.imagePullPolicy` | ghcr, versioned, `IfNotPresent` | the webhook image |
| `webhook.replicas` | `2` | rides a PodDisruptionBudget |
| `webhook.port` | `8443` | bind port; Service targets it |
| `webhook.hostNetwork` | `false` | for CNIs that keep the API server off the pod network |
| `webhook.dnsPolicy` | `""` | defaults to `ClusterFirstWithHostNet` when hostNetwork |
| `webhook.serviceAccount.create/name/annotations` | `true` | token is **never mounted** either way |
| `webhook.certManager.enabled` | `false` | `false`: chart-minted cert, Secret reused across upgrades; `true`: cert-manager issues and renews |
| `webhook.certManager.issuerRef.name/kind` | — | required when enabled |
| `webhook.certManager.duration/renewBefore` | cert-manager defaults | certificate lifetime knobs |
| `webhook.selfRotate.enabled` | `false` | the third TLS mode: the webhook mints CA+leaf in memory, writes its own Secret, patches its own caBundle (two-CA transition window), rotates every 24h behind a Lease. Costs three name-scoped RBAC grants — stated beside the toggle in values.yaml. Mutually exclusive with certManager |
| `webhook.selfSignedDays` | `3650` | validity of the chart-minted pair |
| `webhook.failurePolicy` | `Ignore` | `Fail` couples pod CREATEs to webhook availability — the book's security page owns the trade |
| `webhook.timeoutSeconds` | `5` | admission deadline |
| `webhook.reinvocationPolicy` | `Never` | — |
| `webhook.excludeNamespaces` | `[]` | never touched, beyond kube-system/kube-node-lease/its own |
| `webhook.namespaceGating` | `false` | Istio-style opt-in: only namespaces labeled `dynamic-config.rs/injection: enabled` |
| `webhook.objectSelector` | `{}` | verbatim extra selector on the webhook configuration |
| `webhook.service.type/port/annotations` | `ClusterIP`/`443` | — |
| `webhook.resources` | small | requests/limits of the webhook pod |
| `webhook.readinessProbe.*` / `webhook.livenessProbe.*` | sane | delays/periods/thresholds; path and scheme are the binary's |
| `webhook.strategy` / `.revisionHistoryLimit` / `.minReadySeconds` / `.terminationGracePeriodSeconds` | k8s-ish | rollout shape |
| `webhook.topologySpread` | `true` | soft hostname spread |
| `webhook.podDisruptionBudget.enabled/minAvailable` | `true`/`1` | — |
| `webhook.priorityClassName` / `.nodeSelector` / `.tolerations` / `.affinity` | empty | scheduling |
| `webhook.podLabels` / `.podAnnotations` | `{}` | — |
| `webhook.extraEnv` / `.extraVolumes` / `.extraVolumeMounts` | `[]` | verbatim escape hatches |
| `networkPolicy.enabled` | `false` | ingress 8443 only, **egress empty** — the webhook calls nobody |

### Operator (CRDs install either way)

| value | default | meaning |
|---|---|---|
| `operator.enabled` | `false` | the deployment; CRDs come from `deploy/crds.json` regardless |
| `operator.image` / `.tag` / `.digest` / `.imagePullPolicy` | ghcr | — |
| `operator.serviceAccount.create/name/annotations` | `true` | — |
| `operator.rbac.create` | `true` | least-privilege ClusterRole: the two CRDs, ConfigMaps, events, leases |
| `operator.resources` / `.priorityClassName` / `.nodeSelector` / `.tolerations` / `.affinity` / `.podLabels` / `.podAnnotations` / `.extraEnv` | — | as the webhook's |

## ArtifactHub

The chart ships to `oci://ghcr.io/dynamic-config-rs/charts` and is
listed on artifacthub.io from there; `artifacthub-repo.yml` beside this
file carries the repository metadata and documents the publish
commands. `Chart.yaml` carries the artifacthub annotations — CRDs,
images, links — and `values.schema.json` makes `helm install` refuse a
mistyped value with its name instead of rendering something silent.

## Prefer plain manifests?

`deploy/kustomize/` carries the same resources as a kustomize base with
cert-manager and bring-your-own-cert overlays — see its README.
