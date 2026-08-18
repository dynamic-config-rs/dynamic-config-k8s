# Troubleshooting

Each symptom, its first command, and the usual cause. The agent's logs
are JSON; `kubectl logs <pod> -c dynamic-config-agent | jq .` is the
readable form.

## The file never appears

```sh
kubectl get pod <name> -o jsonpath='{.spec.containers[*].name}'
```

**No `dynamic-config-agent` in the list → the webhook never mutated.**
In order of likelihood:

1. The pod predates the webhook install — admission only sees CREATE.
2. `failurePolicy: Ignore` swallowed a webhook outage:
   `kubectl get events -n <ns> | grep dynamic-config` and the webhook
   deployment's own logs say which.
3. The annotation said `inject: "false"` or misspelled the prefix —
   `dynamic-config.rs/`, with the dot and the slash.

**Agent present, file absent → the agent is failing.** Its log carries
the store error verbatim minus values:

```sh
kubectl logs <pod> -c dynamic-config-agent | jq -r '.fields.error // .fields.message'
```

## The admission was denied

That is the contract working: `inject: "true"` with a missing or
malformed companion annotation **fails the pod's creation**, and the
reason names the annotation:

```text
Error creating: admission webhook "inject.dynamic-config.rs" denied the
request: dynamic-config.rs/inject is true, so dynamic-config.rs/path is required
```

Silently starting without configuration is the failure mode this
refusal exists to prevent; add the named annotation.

## 401/403 from the store

The token travels in `DYNAMIC_CONFIG_AGENT_TOKEN`, not in annotations.

```sh
kubectl exec <pod> -c dynamic-config-agent -- env | grep -c DYNAMIC_CONFIG
```

`0` means the Secret was never mounted onto the agent container.
Config-server 401s specifically: the bearer must belong to a
`[[server.clients]]` block whose `applications` list names the
application in your `--key` — the server's audit log line for the
refusal names the client it matched.

## The rendered file is stale

The sidecar keeps the last good render on fetch failure, on purpose —
staleness with a warning beats an empty file. The log says so at
`warn` level. `--watch-seconds` too high is the boring cause; a store
ACL that started refusing is the interesting one, and the 401 section
above applies.

`mode: init` never refreshes by design: rotation there is a pod
restart, which is the trade the [Vault page](vault.md) states.

## `--out` refused at startup

The extension picks the format, and only five are legal: `.json`
`.toml` `.yaml` `.ini` `.properties`. The error lists them; `.conf` and
`.cfg` are nobody's format and stay refused.

## Arrays in the document, flat output requested

```text
`hosts` is an array, and neither flat format has one; render to json,
toml or yaml instead
```

Exactly what it says: pick a structured output, or reshape the
document. The [Rendering](rendering.md) page owns the reasoning.

## Reading what the webhook actually did

The golden test's fixture is the contract, and a live pod can be
compared against it:

```sh
kubectl get pod <name> -o json | jq '.spec.containers[].volumeMounts'
kubectl get pod <name> -o json | jq '.spec.volumes[] | select(.name == "dynamic-config")'
```

## The pod was created, nothing was injected

In order of likelihood:

1. **The namespace is excluded.** kube-system, kube-node-lease and the
   chart's own namespace never get injection, plus anything in
   `webhook.excludeNamespaces`:

   ```sh
   kubectl get mutatingwebhookconfiguration dynamic-config \
     -o jsonpath='{.webhooks[0].namespaceSelector}'
   ```

2. **The webhook was down and `failurePolicy: Ignore` let the pod
   through.** The API server records exactly that:

   ```sh
   kubectl get events --field-selector reason=FailedAdmissionWebhook -A
   kubectl -n <chart-namespace> get pods -l app.kubernetes.io/component=webhook
   ```

3. **TLS trust is broken** — the caBundle does not match what the
   webhook serves. With the self-signed default this happens when the
   Secret was deleted but the webhook configuration was not re-rendered;
   `helm upgrade` heals both sides. The API server's opinion:

   ```sh
   kubectl logs -n <chart-namespace> deploy/dynamic-config-webhook | tail
   ```

## The webhook pod refuses to start: "no TLS material"

The exact message names the two paths it looked at. The Secret
`dynamic-config-webhook-tls` is missing or empty — with cert-manager
enabled, check the Certificate:

```sh
kubectl describe certificate dynamic-config-webhook
```

## An injected pod is rejected by Pod Security admission

It should not be: the injected container carries the full restricted
posture. If a namespace enforces something stricter than restricted
(an OPA/Kyverno policy), read the denial message — the
[security page](security.md) lists every field the injection sets, so
the diff is one screen.

## The template refuses to render

Two shapes, two places:

- **At startup** (a parse error, an undefined key on the first render):
  the agent exits and the reason is in the injected container's log —
  strict on purpose, so a typo'd `{{ db.hots }}` cannot ship an empty
  string with a clean exit code.
- **During a watch** (the ConfigMap was edited into an error): the pod
  keeps its **last good file** and the agent logs the render error every
  tick until the template is fixed. Check with:

  ```sh
  kubectl logs <pod> -c dynamic-config-agent | tail
  kubectl get configmap billing-template -o jsonpath='{.data.template}'
  ```
