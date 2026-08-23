# Observability

A dashboard and a rule set over the series this integration documents.

```sh
kubectl apply -f deploy/observability/rules.yaml            # needs kube-prometheus-stack
# Grafana → Dashboards → New → Import → Upload dashboard.json
```

`rules.yaml` comes first: four of the dashboard's stat panels read recording
rules from it, and without them they stay empty.

## What is here

| File | What it is |
|---|---|
| `rules.yaml` | four recording rules that aggregate the fleet away from per-pod cardinality, and eight alerts |
| `dashboard.json` | six fleet counters, then staleness, renders, watch health, leases, admission latency and operator reconciles |

## The two things to know before tuning them

**Staleness is the gauge to page on, not a failure counter.** A config agent
that fails one fetch and recovers is doing what it was built to do; an agent
whose store has not answered in half an hour is serving a document nobody
can vouch for. Every `for:` in `rules.yaml` is long for the same reason — an
alert that fires on ordinary recovery is an alert people learn to silence.

**`lease_renewal_failures_total` should sit at zero.** A lease the store
marked `renewable: false` is never sent a renewal at all, so anything
counted here is a lease that was expected to renew and did not. That is what
makes it worth alerting on at a low threshold.

## Cardinality

The recording rules aggregate `pod` away on purpose. A thousand agents cost
four series through them and four thousand without, and the alerts that do
keep `pod` are the ones whose whole purpose is naming which pod.

No metric in this integration carries a key path, a store key, a rendered
value or a source description as a label — that rule is the engine's and it
does not bend here. Anything added to these files should keep it.
