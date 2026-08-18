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

## The gate

```sh
just check     # fmt, clippy, tests, CRD drift
just e2e-smoke # needs docker + kind; CI runs it on every PR
```
