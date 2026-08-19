# Roadmap

Where this repository goes next, in the order the dependencies force.
The organisation-wide milestones live in the `.github` repository;
this file is the k8s integration's own ladder. Dates are absent on
purpose — each rung ships when its gate is green, and the gates are
written down here so "done" is checkable.

The original ladder — release machinery, the async stores (etcd both
auths, NATS, S3), self-rotating webhook TLS, and the operator's
reconcilers — shipped in full with **0.1.1**; the CHANGELOG is its
record. What remains is demand-gated:

- **Webhook metrics grow up**: histogram of admission latency, a
  ServiceMonitor toggle in the chart.
- **Template functions**, demand-driven: `b64`, `indent`, a JSON
  emitter — each addition argued in the book's Rendering page, never
  smuggled.
