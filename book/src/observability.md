# Observability

Three components, three Prometheus endpoints, one honest sentence
about OpenTelemetry. Everything here is hand-rolled Prometheus text —
a handful of counters do not earn a metrics crate — and everything is
scrapeable by anything that speaks the exposition format, which
includes the OTel Collector.

## What each component exposes

| component | where | how it turns on |
|---|---|---|
| webhook | `GET /metrics` on the serving (TLS) port | always on |
| agent | its own port, plain HTTP | `metrics-port` annotation, or the `agent.defaults.metricsPort` fleet default |
| operator | `0.0.0.0:9090`, plain HTTP | on by default; `DYNAMIC_CONFIG_OPERATOR_METRICS_ADDR=""` turns it off |

## The webhook's metrics

One counter, three outcomes — the webhook's whole job is a decision,
so its whole telemetry is which way decisions went:

```text
# TYPE dynamic_config_admissions_total counter
dynamic_config_admissions_total{outcome="skipped"} 41
dynamic_config_admissions_total{outcome="patched"} 12
dynamic_config_admissions_total{outcome="refused"} 3
```

`skipped` is a pod that did not ask; `patched` asked and got the
agent; `refused` asked wrongly — a contract violation or a
[gate](installation-defaults.md#the-gates-in-depth) holding. A rising
`refused` after an installation change usually means a gate is doing
its job on workloads that have not caught up.

The endpoint shares the webhook's serving port, so the scrape goes
over the same TLS the API server uses. Prometheus needs the scheme
and, unless it trusts the webhook's CA, permission to skip
verification:

```yaml
# a scrape_config for the webhook Service
- job_name: dynamic-config-webhook
  scheme: https
  tls_config:
    insecure_skip_verify: true   # or ca_file from the caBundle
  kubernetes_sd_configs:
    - role: endpoints
      namespaces: { names: [dynamic-config] }
```

## The agent's metrics

A watching agent with a `metrics-port` serves three series; the port
also lands on the container spec as a named port (`metrics`), so
selector-based discovery finds it without configuration:

```text
# TYPE dynamic_config_agent_renders_total counter
dynamic_config_agent_renders_total 128
# TYPE dynamic_config_agent_render_failures_total counter
dynamic_config_agent_render_failures_total 2
# TYPE dynamic_config_agent_last_render_timestamp_seconds gauge
dynamic_config_agent_last_render_timestamp_seconds 1755612200
```

The gauge is the one to alert on — a counter that stops moving looks
identical to a quiet store, but a timestamp that stops moving while
`watch-seconds` says it should not is a stuck render:

```yaml
# The document went stale: no successful render for 10 minutes.
- alert: DynamicConfigRenderStale
  expr: time() - dynamic_config_agent_last_render_timestamp_seconds > 600
- alert: DynamicConfigRenderFailing
  expr: increase(dynamic_config_agent_render_failures_total[10m]) > 0
```

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

The same three series, `dynamic_config_operator_`-prefixed:
`_renders_total` counts reconciles that produced or refreshed a target,
`_render_failures_total` the ones that ended in a `RenderFailed`
event, and the gauge the last success. On by default at
`0.0.0.0:9090`; point `DYNAMIC_CONFIG_OPERATOR_METRICS_ADDR` elsewhere
or set it empty to turn it off.

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
