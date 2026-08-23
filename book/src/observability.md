# Observability

Three components, three Prometheus endpoints, and OTLP traces beside them
when a collector is configured. The metrics are hand-rolled exposition text
— a handful of counters do not earn a metrics crate — and are scrapeable by
anything that speaks the format, the OTel Collector included.

## What each component exposes

| component | where | how it turns on |
|---|---|---|
| webhook | its own plain-HTTP port, and `GET /metrics` on the serving (TLS) port too | `webhook.metrics.enabled`, on by default |
| agent | its own port, plain HTTP | `metrics-port` annotation, or the `agent.defaults.metricsPort` fleet default |
| operator | `0.0.0.0:9090`, plain HTTP | on by default; `DYNAMIC_CONFIG_OPERATOR_METRICS_ADDR=""` turns it off |

## The webhook's metrics

The webhook sits in the path of every pod creation in the cluster, so
the first question about it is never "did it work" but **"how long did
it take"** — the API server's own ten-second timeout turns a slow
admission into a refused one, and only a histogram answers what the
tail is doing.

```text
# TYPE dynamic_config_admissions_total counter
dynamic_config_admissions_total{outcome="skipped"} 41
dynamic_config_admissions_total{outcome="patched"} 12
dynamic_config_admissions_total{outcome="refused"} 3
# TYPE dynamic_config_admission_refusals_total counter
dynamic_config_admission_refusals_total{reason="policy"} 2
dynamic_config_admission_refusals_total{reason="pinned"} 1
dynamic_config_admission_refusals_total{reason="conflict"} 0
dynamic_config_admission_refusals_total{reason="malformed"} 0
dynamic_config_admission_refusals_total{reason="other"} 0
# TYPE dynamic_config_admission_duration_seconds histogram
dynamic_config_admission_duration_seconds_bucket{le="0.0001"} 39
dynamic_config_admission_duration_seconds_bucket{le="0.001"} 55
dynamic_config_admission_duration_seconds_bucket{le="+Inf"} 56
dynamic_config_admission_duration_seconds_sum 0.031
dynamic_config_admission_duration_seconds_count 56
# TYPE dynamic_config_admission_patch_bytes_total counter
dynamic_config_admission_patch_bytes_total 24576
```

`skipped` is a pod that did not ask; `patched` asked and got the
agent; `refused` asked wrongly. **The refusals are labelled by kind
because they are three different pages:**

| reason | what happened | who fixes it |
|---|---|---|
| `policy` | a store, or an `agent-env` name, that this installation does not allow here | whoever wrote the pod, or whoever set the gate |
| `pinned` | a value the installation fixed, overridden by a pod | whoever is working around the installation |
| `malformed` | an annotation that is not the shape it has to be | whoever wrote the pod — a typo |
| `conflict` | the pod already has a container by a name the injection needs | whoever wrote the pod — rename it |

The kind is a `status.reason` the refusal carries, not something a
scrape works out by reading the message, so rewording an error does not
silently re-label a metric.

In `selfRotate` mode two more say whether rotation is still happening:

```text
# TYPE dynamic_config_certificate_rotations_total counter
dynamic_config_certificate_rotations_total 7
# TYPE dynamic_config_certificate_expires_at_seconds gauge
dynamic_config_certificate_expires_at_seconds 1755698600
```

A counter that should climb on a schedule, and a wall-clock second that
should always be in the future. **Alert on the gauge**: a webhook whose
certificate expires takes every pod creation in the cluster with it.

```yaml
- alert: DynamicConfigWebhookCertificateExpiring
  expr: dynamic_config_certificate_expires_at_seconds - time() < 3600
- alert: DynamicConfigAdmissionSlow
  expr: histogram_quantile(0.99, rate(dynamic_config_admission_duration_seconds_bucket[5m])) > 1
```

### Where to scrape it

**A port of its own, in plain HTTP** — `webhook.metrics.port`, 9091 by
default. The admission port is mutual TLS against a CA the webhook
mints for itself, so scraping *that* meant handing Prometheus a client
certificate from that CA, and a deployment that did not simply had no
metrics. The admission port still answers `/metrics`, so a scrape
already configured against it keeps working.

With the Prometheus Operator, one toggle:

```yaml
webhook:
  metrics:
    serviceMonitor:
      enabled: true      # needs the ServiceMonitor CRD
      interval: 30s
```

Without it, the Service carries a named `metrics` port:

```yaml
- job_name: dynamic-config-webhook
  kubernetes_sd_configs:
    - role: endpoints
      namespaces: { names: [dynamic-config] }
  relabel_configs:
    - source_labels: [__meta_kubernetes_endpoint_port_name]
      action: keep
      regex: metrics
```

## Watching

**The sidecar watches; it does not poll.** Each store says how it learns
that its document changed, and the agent uses it — so a change in etcd,
Consul, NATS, Redis or a config server arrives as it happens, and a store
that must be asked is asked the cheapest question it offers (an S3 object
is a `HEAD`, not a download).

`watch-seconds` keeps its spelling and means two things depending on the
store:

| the store | what a watch is | what `watch-seconds` means |
|---|---|---|
| etcd, Consul, NATS, Redis, config-server | the store's own push | a **resync** — how often to re-read anyway |
| Vault, S3, git, Firestore | a cheap "has it changed?" | how often to ask |

[`examples/watch-driven.yaml`](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/examples/watch-driven.yaml)
is a pod on the push path with both numbers set: a five-minute resync
under etcd's stream, and the metrics port to scrape it from.

The resync is not belt and braces. **The failure mode of a stream is
silence**: a subscription the broker forgot, a connection that dropped
without an error, a blocking query answering an index that will never
move again. All three look exactly like a store where nothing has
changed, and the only way to tell them apart is to go and ask.

## The agent's metrics

A watching agent with a `metrics-port` serves twenty-seven series; the port
also lands on the container spec as a named port (`metrics`), so
selector-based discovery finds it without configuration. Since 0.3.0 the
chart gives every installation a default port (9110), so this is on unless
somebody turns it off with `metrics-port: "0"`:

```text
# TYPE dynamic_config_agent_renders_total counter
dynamic_config_agent_renders_total 128
# TYPE dynamic_config_agent_render_failures_total counter
dynamic_config_agent_render_failures_total 2
# TYPE dynamic_config_agent_last_render_timestamp_seconds gauge
dynamic_config_agent_last_render_timestamp_seconds 1755612200
# TYPE dynamic_config_agent_deliveries_total counter
dynamic_config_agent_deliveries_total 12
# TYPE dynamic_config_agent_resyncs_total counter
dynamic_config_agent_resyncs_total 480
# TYPE dynamic_config_agent_watch_connected gauge
dynamic_config_agent_watch_connected 1
# TYPE dynamic_config_agent_watch_reconnects_total counter
dynamic_config_agent_watch_reconnects_total 3
# TYPE dynamic_config_agent_staleness_seconds gauge
dynamic_config_agent_staleness_seconds 4
# TYPE dynamic_config_agent_generation gauge
dynamic_config_agent_generation 41
# TYPE dynamic_config_agent_lease_renewals_total counter
dynamic_config_agent_lease_renewals_total 6
# TYPE dynamic_config_agent_lease_renewal_failures_total counter
dynamic_config_agent_lease_renewal_failures_total 0
# TYPE dynamic_config_agent_lease_revocations_total counter
dynamic_config_agent_lease_revocations_total 0
# TYPE dynamic_config_agent_lease_ttl_seconds gauge
dynamic_config_agent_lease_ttl_seconds 3600
# TYPE dynamic_config_agent_absent gauge
dynamic_config_agent_absent 0
# TYPE dynamic_config_agent_absent_total counter
dynamic_config_agent_absent_total 0
# TYPE dynamic_config_agent_notifications_total counter
dynamic_config_agent_notifications_total 12
# TYPE dynamic_config_agent_notification_failures_total counter
dynamic_config_agent_notification_failures_total 0
# TYPE dynamic_config_agent_drift gauge
dynamic_config_agent_drift 0
# TYPE dynamic_config_agent_drift_total counter
dynamic_config_agent_drift_total 0
# TYPE dynamic_config_agent_canary_holding gauge
dynamic_config_agent_canary_holding 0
# TYPE dynamic_config_agent_canary_percent gauge
dynamic_config_agent_canary_percent 100
# TYPE dynamic_config_agent_acks_total counter
dynamic_config_agent_acks_total 41
# TYPE dynamic_config_agent_ack_mismatches_total counter
dynamic_config_agent_ack_mismatches_total 0
# TYPE dynamic_config_agent_applied gauge
dynamic_config_agent_applied 1
# TYPE dynamic_config_agent_unapplied_seconds gauge
dynamic_config_agent_unapplied_seconds 0
# TYPE dynamic_config_agent_tls_reloads_total counter
dynamic_config_agent_tls_reloads_total 0
# TYPE dynamic_config_agent_tls_verification_skipped gauge
dynamic_config_agent_tls_verification_skipped 0
```

`generation` is the store's own revision of what was last rendered, where
the store counts them — a Vault KV version, a Consul index, an etcd
revision. Zero for a store whose revision is opaque, because an ETag has no
number to report and inventing one would make a dashboard compare things
that do not compare.

The four `lease_*` series are zero unless the source is a dynamic-secret
engine. `lease_ttl_seconds` is what the store last *granted*, not what was
asked for.

`lease_renewal_failures_total` is worth alerting on precisely because it
should stay at zero. A lease the store marked `renewable: false` — every
`pki/issue`, and a database credential past its role's maximum — is never
sent a renewal in the first place; it is re-issued instead, and
`lease_renewals_total` stays at zero while the credential keeps arriving.
So a rising failure count is always a lease that was *expected* to renew
and did not, which is a real thing to page on rather than the background
noise it would be if every non-renewable lease were asked anyway.

`tls_verification_skipped` is 1 when the agent runs with
[`tls-skip-verify`](annotations.md#reaching-the-store-over-tls). A gauge
rather than a log line repeated per fetch, because the question it answers
is a fleet question — *which of these five thousand pods are doing this?* —
and one alert over a gauge answers it where five thousand log streams do
not. It should stay at zero.

`canary_holding` is 1 while this pod is outside a
[canary cohort](annotations.md#some-of-the-fleet-before-all-of-it) and is
holding a document it fetched — without it, a held document looks exactly
like a store with nothing new to say. `canary_percent` is the number it last
read, and 100 when no canary is configured.

### The only end-to-end answer

`renders_total` says a document reached disk. `applied` says the
application is **running** it — the four `ack` series are the difference
between "we published" and "it converged", and an application still running
the previous document while every other series reports success is the
outage they close.

They stay at zero unless the application acknowledges, which is not a
failure: an application that never acknowledges is not penalised, it simply
leaves `applied` at zero. `unapplied_seconds` is the one to alert on —
`ack_mismatches_total` climbing steadily means acknowledgements and renders
are talking past each other, which is a different problem from being behind.

[The annotations page](annotations.md#knowing-the-application-actually-applied-it)
has the two-line contract, and `require-ack` turns it into readiness.

`tls_reloads_total` counts the times the store's client was rebuilt for
rotated trust material — a counter rather than a gauge, because the question
worth asking is whether a rotation was picked up at all.

`drift` is 1 while a rendered file is something other than what this agent
wrote. `notification_failures_total` counts post-render calls that were
refused or unanswered; neither is fatal, because the file is already
published by the time either can happen.

`absent` is 1 while the store says the document is not there, and
`absent_total` counts how many times it has gone missing. The distinction
they encode is the one a stale file cannot make on its own: a store that
does not answer is an outage, which waiting cures, and a store that answers
*gone* is a deletion, which waiting does not. Before 0.3.0 a deleted Vault
secret moved nothing at all and the last render went on being served with
every health check reporting fine. What the agent *does* about it is
[`on-delete`](annotations.md#freshness-and-what-happens-when-a-store-is-not-there).

### `/readyz` means there is a document

The agent's readiness is stricter than the operator's, and since 0.3.0 the
webhook attaches a probe to the injected container: `/readyz` answers 503
until a document has been rendered, and pod readiness is already AND-ed
across containers — so a Service sends no traffic to a pod whose
configuration does not exist yet.

The trade, said plainly: with the store unreachable at start and nothing
cached, the pod never becomes ready. That is correct — the application has
no configuration — but it turns a store outage into a visibly stalled
rollout rather than pods that come up and misbehave. Two ways out, and they
answer different questions:

- `dynamic-config.rs/startup-policy: allow-cached` (the default) serves the
  file already on disk when the first fetch fails. The volume survives a
  *container* restart, so an agent coming back after a crash usually finds
  its own last render there.
- `dynamic-config.rs/readiness: "false"` detaches the probe entirely, for a
  deployment that would rather start than wait.

And `dynamic-config.rs/max-staleness: "6h"` puts a ceiling on the first of
those: last-known-good answers *is there a document* and leaves *is it too
old to trust* open. A credential may be worthless after five minutes and a
feature flag fine after a week, so there is no default — the ceiling is off
unless somebody sets one.

**`staleness_seconds` is the one to alert on.** It is seconds since the
store was last read successfully, delivered or resynced — the number a
pager asks for. Everything else says what happened; this says how long
ago it stopped happening. Zero until the first success, so a pod that has
never reached its store does not look fresh.

```yaml
# The document went stale: nothing read from the store for 10 minutes.
- alert: DynamicConfigStoreStale
  expr: dynamic_config_agent_staleness_seconds > 600
- alert: DynamicConfigRenderFailing
  expr: increase(dynamic_config_agent_render_failures_total[10m]) > 0
# A watch that keeps reopening is a store or a network that is not well.
- alert: DynamicConfigWatchFlapping
  expr: increase(dynamic_config_agent_watch_reconnects_total[15m]) > 5
```

**Deliveries and resyncs are told apart because that pair is a
diagnosis.** Deliveries flat while resyncs climb *is* the stalled stream
the resync exists to cover: the store is being read, changes are landing,
and the push half is doing nothing. Nothing else here would show it.

`watch_connected` is `0` between a watch ending and the next one opening;
on a store that is polled rather than pushed it stays `1` for as long as
the loop runs.

A one-shot init agent serves nothing — it renders once and exits, and
a scrape target that lives for two seconds is noise. Only the watching
half carries the port, which is also why `metrics-port` pairs with
`mode: sidecar` or `both`.

With a PodMonitor (Prometheus Operator), the named port makes
discovery one selector:

```yaml
apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: dynamic-config-agents
spec:
  selector:
    matchExpressions:
      - { key: app, operator: Exists }   # your workload labels
  podMetricsEndpoints:
    - port: metrics
```

Fleet-wide, set `agent.defaults.metricsPort: "9102"` once and every
injected watching agent serves on 9102; a pod that must not (a
port collision, say) opts out with `metrics-port: "0"`.

## The operator's metrics

The agent's render series, `dynamic_config_operator_`-prefixed —
`_renders_total` counts reconciles that produced or refreshed a target,
`_render_failures_total` the ones that ended in a `RenderFailed` event,
and the gauge the last success — plus a reconcile histogram:

```text
# TYPE dynamic_config_operator_reconcile_duration_seconds histogram
dynamic_config_operator_reconcile_duration_seconds_bucket{le="0.5"} 88
dynamic_config_operator_reconcile_duration_seconds_bucket{le="+Inf"} 91
dynamic_config_operator_reconcile_duration_seconds_sum 12.4
dynamic_config_operator_reconcile_duration_seconds_count 91
# TYPE dynamic_config_operator_reconcile_failures_total counter
dynamic_config_operator_reconcile_failures_total 3
```

A reconcile is a store fetch and an API write, so a tail that moves is
usually the store rather than the operator. On by default at
`0.0.0.0:9090`; point `DYNAMIC_CONFIG_OPERATOR_METRICS_ADDR` elsewhere
or set it empty to turn it off. The same port answers `/healthz` and
`/readyz`, which is what the chart's probes use.

The operator's `/readyz` means *the process is up*, and that is not an
oversight: an operator with no `DynamicConfigRender` resources has rendered
nothing and is working perfectly. The **agent** means something stricter by
the same path — see below.

**Only one replica reconciles.** The operator elects a leader over a
Lease, so `operator.replicas: 2` is spare capacity rather than twice the
work — and the metrics of a follower stay flat by design. A leader that
dies is replaced within the lease's fifteen-second term.

Beside the metrics, the operator reports per-object: a `Ready`
condition on every `DynamicConfigRender`'s status and Kubernetes
Events (`Rendered` / `RenderFailed`) on the object — `kubectl describe
dynamicconfigrender` is the first debugging stop, before any
dashboard.

## Logs

Every component writes JSON to stdout through `tracing`, one line per
event, `RUST_LOG` grammar via the environment. The webhook logs one
audit line per non-skipped admission — namespace, pod, source, outcome,
and NO annotation values, because store addresses and role names
belong in the cluster, not in every log aggregator. The agent logs
each render with byte counts, and failures with the store's error.

Turning up an agent's verbosity is the canonical `agent-env` use:

```yaml
# the installation allows it:   webhook.agentEnvAllow: "*: RUST_LOG"
dynamic-config.rs/agent-env: "RUST_LOG=debug"
```

— or fleet-wide for a day with `agent.defaults.env: "RUST_LOG=debug"`,
no allowlist needed, [the installer owns
both](installation-defaults.md#the-fleet-environment--no-gate-on-purpose).

## The engine's own metrics

The series above measure the DELIVERY machinery: admissions, renders,
staleness. The configuration engine inside your application has its
own richer contract — `dynamic_config_reload_total`, generation and
snapshot-age gauges, per-source health — defined once in
[the metrics contract](https://dynamic-config-rs.github.io/metrics-contract.html)
of the engine book and exported by the language bindings. When an app
consumes the rendered file through the engine's own watcher, alert on
the engine's series (closest to the truth the app sees) and keep the
agent's gauge as the delivery-side backstop.

## A dashboard and the rules under it

`deploy/observability/` carries both, over the series named above:

```sh
kubectl apply -f deploy/observability/rules.yaml    # recording rules, then alerts
# Grafana → Dashboards → Import → dashboard.json
```

The rules come first — four of the dashboard's fleet counters read recording
rules, and without them those panels stay empty.

Two things worth knowing before tuning the thresholds. **Staleness is the
gauge to page on**, not a failure counter: an agent that fails one fetch and
recovers is doing what it was built to do, while one whose store has not
answered in half an hour is serving a document nobody can vouch for. And
**`lease_renewal_failures_total` should sit at zero**, which is what makes a
low threshold on it reasonable — a lease the store marked `renewable: false`
is never sent a renewal at all, so anything counted there was expected to
renew and did not.

The recording rules aggregate `pod` away. A thousand agents cost four series
through them and four thousand without; the alerts that do keep `pod` are
the ones whose whole purpose is naming which pod.

## OpenTelemetry

Since 0.3.0 the webhook, the agent and the operator export OTLP traces — and only when a
collector is configured. Setting `OTEL_EXPORTER_OTLP_ENDPOINT` is the whole
opt-in; leave it unset and nothing is exported, nothing is allocated, and
the process behaves exactly as it did. The variable is OpenTelemetry's own,
so a collector that already injects it into pods needs no annotation from
this chart.

Resource attributes come from the downward API — `service.name`,
`service.version`, `k8s.pod.name`, `k8s.namespace.name`, `k8s.node.name` —
and are **absent rather than invented** when nobody wires them: a made-up
pod name is worse than none.

### Where the pipeline is built

The engine offers an `otel` feature that records into a meter somebody else
configured, and refuses to build the pipeline. That is the right split: a
pipeline owns a runtime, a batch processor and a shutdown, and a *library*
that takes those over is one that has to be fought. These three are
programs, and programs own `main` — so the exporter, and the flush before
exit, are theirs.

If a collector cannot be reached, the process logs a warning and carries on
without traces. A configuration agent that refused to start because its
*telemetry* was down would be a worse outage than the one it was reporting.

### The Prometheus text is untouched

Nothing above replaces the scrape. Many deployments have one already, and
the collector reads it directly — neither way out is the price of the
other:

- **Metrics**: the OTel Collector's `prometheus` receiver scrapes all
  three endpoints and exports OTLP wherever you aggregate:

  ```yaml
  receivers:
    prometheus:
      config:
        scrape_configs:
          - job_name: dynamic-config-agents
            kubernetes_sd_configs: [{ role: pod }]
            relabel_configs:
              - source_labels: [__meta_kubernetes_pod_container_port_name]
                regex: metrics
                action: keep
  ```

- **Logs**: the JSON lines on stdout are structured input for the
  `filelog` receiver, no parsing regexes required.

Traces are spans over the work — an admission, a fetch, a render — and the
scrape is the state of it. A deployment that wants only one of the two is
not missing anything by taking only one.
