# Observability

Three components, three Prometheus endpoints, one honest sentence
about OpenTelemetry. Everything here is hand-rolled Prometheus text —
a handful of counters do not earn a metrics crate — and everything is
scrapeable by anything that speaks the exposition format, which
includes the OTel Collector.

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

A watching agent with a `metrics-port` serves eight series; the port also
lands on the container spec as a named port (`metrics`), so
selector-based discovery finds it without configuration:

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
```

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

## OpenTelemetry, honestly

There is no OTel SDK inside any of these binaries, no traces and no
OTLP exporter — an admission decision and a render loop are single
hops with nothing worth a span, and a config agent should not carry a
telemetry stack an order of magnitude heavier than itself. What you
get instead composes with OTel cleanly:

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

If a future release grows OTLP, it will be because a real deployment
needed it — not because the acronym was missing from this page.
