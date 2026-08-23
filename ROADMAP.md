# Roadmap

Where this repository goes next, in the order the dependencies force.
The organisation-wide milestones live in the `.github` repository;
this file is the k8s integration's own ladder. Dates are absent on
purpose — each rung ships when its gate is green, and the gates are
written down here so "done" is checkable.

The original ladder — release machinery, the async stores (etcd both
auths, NATS, S3), self-rotating webhook TLS, and the operator's
reconcilers — shipped in full with **0.1.1**; the CHANGELOG is its
record. **0.2.0** answered the webhook-metrics item — a histogram of
admission latency, refusals labelled by kind, and a ServiceMonitor
toggle — and took the agent off polling.

**0.3.0** closed the last item on that ladder and a great deal that was
not on it. Template functions landed as six named filters — `b64encode`,
`b64decode`, `json`, `yaml`, `quote`, `required` — each argued in the
book's Rendering page. Beside them: last-known-good with a
`startup-policy`, a readiness probe that means *there is a document*,
Vault dynamic secrets end to end, a deletion policy, a staleness ceiling,
schema validation at the agent boundary, admission warnings, a `validate`
CLI, native arm64 builds, OTLP from all three binaries, and latency,
upgrade and scale legs in CI.

## What is next

Nothing is scheduled. The honest next need is not architecture — it is
soak time and production use, and the things worth building after that
will be named by whoever runs into them.

What has been *considered and set aside* is written down rather than
forgotten: post-render notification, a `class` annotation the webhook
resolves, a node-level agent, a CSI driver, a circuit breaker, Kubernetes
Events from the agent, render history, config canary, drift detection,
and `networkPolicy` on by default. Each has a reason and a condition that
would change it.
