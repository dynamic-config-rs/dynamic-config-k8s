# Policies

Kubernetes enforces these better than a policy engine inside this project
would, so what is here is worked examples rather than a feature.

Each file is a `ValidatingAdmissionPolicy` plus a binding, built in
[CEL](https://kubernetes.io/docs/reference/using-api/cel/), and each is
`Warn` by default so it can be installed and read before it is enforced.
Change `validationActions` to `[Deny]` when the report is empty.

```sh
kubectl apply -f policies/require-memory-volume.yaml
kubectl get events --field-selector reason=ValidatingAdmissionPolicy
```

| File | What it refuses |
|---|---|
| `require-memory-volume.yaml` | `volume-medium: disk` in the namespaces you name — a rendered secret on node-backed storage outlives the pod |
| `require-authenticated-tls.yaml` | `tls-skip-verify` anywhere, whatever the installation allows — the belt to the chart value's braces |
| `forbid-world-readable.yaml` | a `file-mode` any container in the pod can read |
| `require-pinned-agent.yaml` | an injected agent image that is not pinned by digest |

They select on the annotations rather than on the injected container,
because admission policies run **before** this webhook does: what they see
is what the author wrote.
