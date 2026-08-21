# Working in this repository

Three binaries that put dynamic-config configuration into pods: an agent
that renders, a webhook that injects the agent, an operator that renders
into ConfigMaps.

```text
dynamic-config-agent/      store → resolved document → atomic file
dynamic-config-webhook/    AdmissionReview → JSONPatch (pure) + transport
dynamic-config-operator/   the two CRDs, generated manifests, reconcile
deploy/                    helm (+ crds/) + kustomize (base/overlays) + crds.json — the CRD copies are ALL generated: `just crds` gates them
e2e/                       the kind harness; smoke.sh is the contract
```

## The rules

1. **The annotation contract is the API.** A change to
   `dynamic-config.rs/*` names or semantics is breaking, bumps the
   minor, and regenerates the golden file *in the same commit* —
   `tests/golden.rs` says how.
2. **The webhook's patch generation stays pure.** If it needs a cluster
   to test, the design broke.
3. **A wrong ask fails the admission.** Silently not injecting is how a
   pod starts without the configuration it declared.
4. **`deploy/crds.json` is generated** — edit the Rust types, run
   `just crds-write`, never the JSON.
5. **No value in any log line**, agent included: paths, keys and
   byte-counts only, the organisation's standing rule.
6. **A watch delivers a change; the current value is not one.** Every
   store keeps that contract, so anything that renders only on delivery
   leaves a pod whose app opens a file that is not there. The agent
   fetches and renders once *before* it watches — `e2e/smoke.sh`'s
   `MINIMAL OK` line is what catches a regression, and
   `tests/sidecar.rs` catches it without a cluster.
7. **Refusal kinds are a `status.reason` slug**, written where the
   refusal is. Nothing downstream reads the English: that was tried, and
   none of the three real messages matched the patterns written for
   them.

## The gate

```sh
just check     # fmt, clippy, tests, CRD drift
just e2e-smoke # needs docker + kind; CI runs it on every PR
```

Rendering is part of the gate too, because half of what ships here is
YAML:

```sh
helm lint deploy/helm
helm template dc deploy/helm >/dev/null
kubectl kustomize deploy/kustomize/overlays/with-operator >/dev/null
```

`deploy/kustomize/overlays/own-cert` does *not* render on its own and is
not broken: it expects the operator to drop `tls.crt`, `tls.key` and
`ca.crt` beside it.

## What is written down for an agent

`.claude/` carries the same three things every repository in this
organisation carries, spelled for this one:

```text
.claude/settings.json                   what a tool may run — nothing that
                                        reaches a cluster, nothing that signs
.claude/hooks/contract-drift.sh         names the files a change has to
                                        travel to, at the moment it is made
.claude/skills/review-for-release/      the checks that have caught things
.claude/skills/change-an-annotation/    the contract change, in order
.claude/skills/triage-security/         alerts, images and the chart
.claude/agents/admission-reviewer.md    the webhook's invariants, reviewed
.claude/agents/deploy-reviewer.md       the chart, the overlays, the CRDs
```

The two agents split the way the repository does: one reads Rust that
decides what a pod becomes, the other reads YAML that somebody else's
cluster installs. Both are told to render or run what they claim — a
finding that cannot be reproduced by a command in this repository is a
guess, and says so.

`CLAUDE.md` is one line — `@AGENTS.md` — so there is one file to keep
true rather than two that drift.
