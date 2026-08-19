# GitOps: ArgoCD and Flux

Running the chart under a GitOps controller works, with two things
worth knowing in advance: certificates, and the difference between
the two Gits in play.

## The two Gits

A GitOps setup around dynamic-config has two repositories doing two
jobs, and conflating them causes most confusion:

- **ArgoCD's Git** delivers *manifests* — the chart, its values, your
  Deployments with their `dynamic-config.rs/*` annotations. Changing
  an annotation is a Deployment change: ArgoCD applies it, the
  rollout creates new pods, the webhook injects the new agent config.
  Cadence: deploys.
- **The agent's [Git store](git.md)** delivers *configuration
  documents* — the agent polls the repository directly, inside the
  pod, no sync loop involved. Cadence: seconds-to-minutes, no
  rollout, no ArgoCD.

Config-in-Git does not require routing config *through* ArgoCD. The
agent's Git store gives you Git-audited configuration that updates
without a pod restart; keep ArgoCD for the manifests.

## Certificates under a sync loop

The chart's default TLS mode mints a CA and certificate **at template
time** (`helm template` runs `genCA`). Under ArgoCD that means every
sync renders a *new* certificate — a permanent diff, and a webhook
whose caBundle churns on every sync.

Under GitOps, pick one of:

- **cert-manager mode** (recommended where cert-manager runs):
  `webhook.certManager.enabled: true`. cert-manager issues and renews;
  cainjector maintains the caBundle; the rendered manifests are stable.
- **selfRotate mode**: rotation without the dependency — the webhook
  patches its own caBundle at runtime, so tell ArgoCD that field is
  runtime-owned (the `ignoreDifferences` block below, caBundle entry).
- Kustomize-native Flux/Argo setups get the same choices as
  [overlays](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/deploy/kustomize/README.md):
  `cert-manager`, `own-cert`, `selfrotate`, plus `with-operator` for
  the Render reconciler — composable in one Kustomization.
- **Provided-cert mode**: mount your own Secret
  ([Secrets & TLS](secrets-and-tls.md)) and set the caBundle in
  values — everything is declarative, nothing is generated.
- If you must keep the self-signed default, tell ArgoCD to look away:

```yaml
ignoreDifferences:
  - kind: Secret
    name: dynamic-config-webhook-tls
    jsonPointers: [/data]
  - group: admissionregistration.k8s.io
    kind: MutatingWebhookConfiguration
    jqPathExpressions: [".webhooks[].clientConfig.caBundle"]
syncPolicy:
  syncOptions: [ApplyOutOfSyncOnly=true]
```

## CRDs

Helm installs the `crds/` directory on install and **never upgrades
it** — standard Helm behaviour, and under ArgoCD it depends on how the
chart is rendered. Two reliable shapes:

- ArgoCD renders charts with `helm template --include-crds`; the CRDs
  become ordinary tracked resources and upgrades apply. Add
  `ServerSideApply=true` to `syncOptions` — the rendering CRDs carry
  large schemas and can exceed the client-side annotation limit.
- Or manage `deploy/crds/` as its own Application (or Flux
  Kustomization) in an earlier sync wave, which is also the shape the
  operator's future CRD changes will assume.

## Order of arrival

The webhook must be serving before annotated workloads are admitted.
In one Application that is a race; in practice it is self-healing
(`failurePolicy: Ignore` is the default — an early pod is admitted
un-injected and the next rollout catches it), but a strict setup puts
the chart in an earlier sync wave than the workloads:

```yaml
metadata:
  annotations:
    argocd.argoproj.io/sync-wave: "-1"   # the chart's Application
```

With `failurePolicy: Fail` the wave separation stops being optional —
see [The Security Posture](security.md) for that trade.
