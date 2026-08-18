# Install

```sh
helm install dynamic-config deploy/helm
```

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

| | self-signed (default) | cert-manager |
|---|---|---|
| dependencies | none | cert-manager installed |
| renewal | none — ten-year cert; rotate by deleting the Secret and upgrading | automatic, well before expiry |
| caBundle | embedded at install | maintained by cainjector |
| fits | getting started, edge clusters, air-gapped | anywhere cert-manager already runs |

Both mount the same Secret shape at the same path; the webhook cannot
tell them apart, which is what makes switching later a values change.

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

## The smoke test

The e2e smoke (`e2e/smoke.sh`) is the install, end to end, against a
kind cluster: the chart in its zero-dependency default, a live Consul,
one annotated pod, the rendered file read back out of it, and the
injected container's security posture asserted on the running pod.
`CERT_MANAGER=1 e2e/smoke.sh` runs the same flow through the other TLS
mode.
