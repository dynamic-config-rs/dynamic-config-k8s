# Install

```sh
# From the OCI registry (ArtifactHub lists the same chart):
helm install dynamic-config \
  oci://ghcr.io/dynamic-config-rs/charts/dynamic-config --version 0.2.0

# Or from a checkout:
helm install dynamic-config deploy/helm
```

Every image the chart deploys is on ghcr and mirrored to Docker Hub
(`docker.io/ctolon17/…`) with identical digests, multi-arch, SBOM
attested and cosign-signed keyless; the repository README carries the
`cosign verify` line.

That is the whole install: no dependencies, nothing to pre-create. The
chart mints a CA and a ten-year serving certificate at install time,
embeds the `caBundle` in the webhook configuration, and reuses the
Secret on upgrades so the trust does not silently rotate. The webhook
terminates that TLS in-process — the API server speaks HTTPS to
admission webhooks and nothing else.

## The cert-manager mode

```sh
helm install dynamic-config deploy/helm \
  --set webhook.certManager.enabled=true \
  --set webhook.certManager.issuerRef.name=<your-issuer>
```

With cert-manager the certificate is *renewed* — cainjector maintains
the `caBundle`, and the webhook picks up the renewed pair from disk
without a restart (it polls the mounted files for a changed
modification time). The trade between the two:

## The selfRotate mode

```sh
helm install dynamic-config deploy/helm \
  --set webhook.selfRotate.enabled=true
```

The third answer, the Vault-agent-injector shape: the webhook is its
own certificate authority. It mints a CA and leaf **in memory** at
rotation time, writes the pair to its own Secret — every replica
serves it through the same file hot-reload cert-manager uses — and
patches the webhook configuration's `caBundle` itself. A fresh pair
every 24 hours, leader-elected over a Lease so replicas do not race,
jittered so a fleet restarted together does not rotate together.

The price is stated in `values.yaml` beside the toggle: a
service-account token and three narrow, name-scoped permissions — the
zero-RBAC purity of the other two modes, knowingly traded for rotation
without a dependency.

| | self-signed (default) | cert-manager | selfRotate |
|---|---|---|---|
| dependencies | none | cert-manager installed | none |
| renewal | none — ten-year cert; rotate by deleting the Secret and upgrading | automatic, well before expiry | automatic, every 24h, leader-elected |
| caBundle | embedded at install | maintained by cainjector | patched by the webhook itself |
| RBAC | none | none | one Secret, one MWC, leases — by name |
| fits | getting started, edge clusters, air-gapped | anywhere cert-manager already runs | rotation wanted, cert-manager not |

All three mount the same Secret shape at the same path; the webhook's
serving loop cannot tell them apart, which is what makes switching
later a values change.

## A private mirror for the agent image

The injected agent is pulled in **application namespaces**, so a fleet
that mirrors images into a private registry needs two values:

```sh
helm install dynamic-config deploy/helm \
  --set agent.image=registry.internal/dynamic-config-agent \
  --set agent.pullSecret=mirror-cred
```

`agent.pullSecret` is **appended** to each injected pod's own
`imagePullSecrets`, never replacing them. Pull secrets are namespaced —
the Secret must exist in every namespace that injects, which is the
usual replication job (`kubectl create secret docker-registry … -n
<each>`, or a replicator you already run). The webhook's and operator's
own images use the chart-level `imagePullSecrets` list instead, because
those pods live in the release namespace.

## failurePolicy

`Ignore` by default, and the book owns the trade: `Fail` would make the
webhook a single point of failure for every pod creation in selected
namespaces, while `Ignore` means an annotated pod created during a
webhook outage starts *without* its agent — loudly, because the file
its application waits for never appears. Flip it with
`--set webhook.failurePolicy=Fail` once the webhook has earned it in
your cluster; the [security page](security.md#failurepolicy-the-whole-trade)
carries the full argument.

## Namespace gating

Clusters that prefer opt-in injection (the Istio shape) set
`webhook.namespaceGating=true` and label the namespaces that want it:

```sh
kubectl label namespace payments dynamic-config.rs/injection=enabled
```

Everything else is invisible to the webhook — the
[security page](security.md#namespace-gating) explains what that buys.

## What the chart hardens for you

The [security page](security.md) is the complete inventory; the short
list: both deployments run as non-root with the restricted-PSS
container posture, the webhook's ServiceAccount mounts no API token,
kube-system and the release namespace are excluded from injection, two
replicas ride a PodDisruptionBudget, and `tag: latest` fails the
render. An optional NetworkPolicy writes down that the webhook accepts
the API server and calls nobody.

## One namespace of its own

Install into a dedicated namespace, always:

```sh
helm install dynamic-config deploy/helm -n dynamic-config --create-namespace
```

The webhook configuration excludes its own namespace by name — the
self-deadlock guard — so a release installed into `default` silently
excludes every workload sharing `default` with it. The chart's NOTES
print a warning when that happens; the e2e smoke installs the dedicated
way for the same reason.

## Fleet-wide defaults, validated at the door

What a pod does not say per annotation, the installation says once —
and it can say it twice, because defaults come in tiers: annotation >
per-store default > fleet default > built-in. Every knob the
annotations know is defaultable; there is no second vocabulary:

```yaml
agent:
  defaults:
    cpuRequest: 10m        # agent-cpu-request still wins per pod
    memoryRequest: 32Mi
    memoryLimit: 64Mi
    cpuLimit: ""           # empty on purpose
    fileMode: "0640"       # empty = the agent's 0644; file-mode wins per pod
    watchSeconds: "30"     # empty = 15; watch-seconds wins per pod
    mode: "both"           # empty = sidecar
    volumeMedium: ""       # empty = memory
    nativeSidecar: ""      # empty = false
    runAsUser: "1000"      # empty = 65532; 0 refused, same as the annotation
    runAsGroup: "1000"
    metricsPort: "9102"    # empty = no metrics; pods opt out with metrics-port "0"
    env: "HTTPS_PROXY=http://egress.infra.svc:3128"  # every agent; pod's agent-env wins per name
    source: "consul"       # pods may omit source entirely
    path: "/config/rendered.toml"
    overridable: ""        # "false" pins every value set here; "!"/"?" per value
    perStore:              # the tier between annotation and fleet
      vault: "endpoint=https://vault.vault.svc:8200!, auth=kubernetes, watch-seconds=10"
      s3: "agent-memory-limit=128Mi"
webhook:
  agentEnvAllow: "payments: HTTPS_PROXY, AWS_*; *: RUST_LOG"
  sourceAllow: ""          # empty = every store, everywhere
  sourceDeny: "sandbox: git"
```

`perStore` keys are spelled exactly as the annotations spell them
(`watch-seconds`, not `watchSeconds`) — one grammar for the value,
whether it arrives per pod, per store, or per fleet — and they cover
EVERY store-shaped annotation, so a developer can deploy knowing
nothing but `inject: "true"`. The `!` above PINS the vault address: a
pod annotating a different endpoint is refused, not silently
corrected. `agent.defaults.env` needs no allowlist: the installer owns
both the values and the gate.

Helm's schema refuses a malformed value at render time; the webhook
re-validates ALL of it at startup and refuses to serve on a typo — so
an installation written any of the three ways gets the same refusal at
the same door. The readable form for kustomize is
`base/installation.yaml`, a ConfigMap of the same settings as YAML
([Installation Defaults](installation-defaults.md#writing-them-as-yaml));
the variables below are the other way, and still work:

```yaml
# kustomization.yaml, an overlay patch
patches:
  - target: { kind: Deployment, name: dynamic-config-webhook }
    patch: |
      - op: add
        path: /spec/template/spec/containers/0/env/-
        value: { name: DYNAMIC_CONFIG_AGENT_FILE_MODE, value: "0640" }
```

[`agentEnvAllow`](annotations.md#the-agent-env-gate) and the
[source gates](annotations.md#the-source-gates) are security gates,
not defaults. [Installation Defaults and Gates](installation-defaults.md)
is the full treatment: every knob with its validation, per-store
examples for all nine stores with every field filled, the gates'
semantics and threat model, and the kustomize equivalents.

## Values, all of them

The [chart README](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/deploy/helm/README.md)
is the full values reference — naming overrides, common labels,
per-component images and pull policy, service accounts, probe and
rollout tuning, `extraEnv`/`extraVolumes` escape hatches, namespace
gating, the operator's RBAC toggle. Everything the templates read is in
that table.

## Without helm: kustomize

`deploy/kustomize/` carries the same resources as a base — including
`installation.yaml`, the fleet defaults and gates written as YAML rather
than as environment-variable grammar — plus TLS overlays for
cert-manager, bring-your-own-PEMs via `secretGenerator`, and the
self-rotating mode.
Its [README](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/deploy/kustomize/README.md)
is the three-step walkthrough, including the one `caBundle` patch
kustomize cannot express. The CRDs ship inside the base, drift-gated
against the operator's `--crds` output like every other copy.

## The smoke test

The e2e smoke (`e2e/smoke.sh`) is the install, end to end, against a
kind cluster: the chart in its zero-dependency default, a live Consul,
one annotated pod, the rendered file read back out of it, and the
injected container's security posture asserted on the running pod.
`CERT_MANAGER=1 e2e/smoke.sh` runs the same flow through the other TLS
mode.
