# Changelog

All notable changes to the `dynamic-config-k8s` components are documented
here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

The three components version together and ship as images, not crates.
Pre-1.0, a breaking change to the **annotation contract** bumps the minor
version — the contract is the API here.

## [Unreleased]

### Added

- **Output templating.** `dynamic-config.rs/template` (inline) and
  `template-configmap` (mounted, re-read every render) put a minijinja
  template between the resolved document and the file — env files,
  framework-shaped nesting, headers. Strict undefined (a typo is an
  error, not an empty string), lowercase booleans, the trailing newline
  survives, keep-last-good covers a bad edit, and the template's
  context is exactly the resolved document — no files, no environment,
  no network. The agent grew `--template`/`--template-inline`, and a
  templated `--out` frees its extension (`.env` is legal there).
- **Namespace gating.** `webhook.namespaceGating=true` selects only
  namespaces labeled `dynamic-config.rs/injection: enabled` —
  Istio-style opt-in that bounds the blast radius and makes
  `failurePolicy: Fail` a per-namespace promise.

- **Enterprise round.** Unknown `dynamic-config.rs/*` annotations now
  FAIL the admission (a typo'd `token-secret` silently ignored is a pod
  without the auth it declared); `template`/`template-configmap` are
  reserved for 0.2.0's output templating and refused by version;
  `source: etcd|nats|s3` is refused at admission instead of at
  container crashloop. One structured audit line per patched/refused
  admission (names, never values) and `GET /metrics` with
  `dynamic_config_admissions_total{outcome}`. Hardcoded values moved to
  the chart: injected-agent resource defaults (`agent.defaults.*` →
  env), the webhook's bind address/port (`webhook.port`,
  `webhook.hostNetwork` for API-server-off-pod-network CNIs),
  `imagePullSecrets`, `webhook.podAnnotations`. Twelve ready-to-apply
  manifests landed in `examples/` — every store, every auth method,
  and the native-sidecar Job.

- **Production posture, end to end.** The webhook terminates TLS
  in-process (tokio-rustls over `ring`, the config server's own accept
  loop shape) with hot certificate reload and a graceful SIGTERM drain
  — an admission webhook is only ever called over HTTPS, and the
  previous plain-HTTP listener could never have been. cert-manager is
  now **optional**: the chart's default mints its own CA and ten-year
  certificate at install (Secret reused across upgrades, caBundle
  embedded), `webhook.certManager.enabled=true` keeps the renewing
  mode. Injected agents carry the full restricted-PSS security context,
  requests/limits (annotation-tunable: `agent-cpu-request`,
  `agent-memory-request`, `agent-cpu-limit`, `agent-memory-limit`), a
  memory-backed emptyDir by default (`volume-medium: disk` opts out),
  and `native-sidecar: "true"` injects the watcher as a
  `restartPolicy: Always` init container (Kubernetes 1.29+; Jobs
  finish). The chart grew the standard label set, a ServiceAccount with
  `automountServiceAccountToken: false`, topology spread, a
  PodDisruptionBudget, an optional deny-all-egress NetworkPolicy,
  namespace self-exclusion in the webhook configuration
  (kube-system, kube-node-lease, the release namespace,
  `webhook.excludeNamespaces`), `failurePolicy`/`timeoutSeconds`
  values, digest pinning, and a render-time refusal of `tag: latest`.
  The agent image reaches the webhook through
  `DYNAMIC_CONFIG_AGENT_IMAGE` — the chart's `agent.image` value now
  actually governs what gets injected. The e2e smoke runs the
  zero-dependency default (`CERT_MANAGER=1` covers the other mode) and
  asserts the injected posture on the live pod.

- **`dynamic-config-agent`** (0.1.0's piece): `--source
  consul|vault|config-server|firestore|git|redis`, the full
  per-store auth surface (`--auth` with each store's methods — vault's
  seven, consul's login flows, Workload Identity on firestore, tokens
  and ssh keys on git), secrets through `DYNAMIC_CONFIG_AGENT_TOKEN` /
  `_PASSWORD` / `_ENDPOINT` only (never flags), a private CA and a
  client certificate as data (`--ca`, `--tls-cert`/`--tls-key`),
  `--key`, `--out` (extension picks the format,
  `.properties` and `.ini` included — the agent owns flat emitters the
  engine's `save` refuses, and the round trip through the engine is a
  test), `--one-shot` for init containers, `--watch <seconds>` for
  sidecars, atomic write-then-rename, keep-last-good on fetch failure,
  JSON logs. etcd, nats and s3 join with the async path in 0.2.0 and
  are refused by name until then.
- **`dynamic-config-webhook`** (0.2.0's piece, golden-tested today): the
  `dynamic-config.rs/*` annotation contract v1, pure JSONPatch
  generation — a wrong ask *fails the admission* rather than silently
  not injecting — and an axum server over the organisation's own
  request-scope crate. The contract carries authentication end to end:
  pass-through `auth*` annotations, `token-secret`/`password-secret`/
  `endpoint-secret` wired as Secret-backed environment variables,
  `ca-configmap`, `tls-secret` (the `kubernetes.io/tls` pair) and
  `ssh-secret` (the `kubernetes.io/ssh-auth` convention, mounted
  `0400`, `auth: ssh-key` implied) as read-only mounts into the agent
  alone. Two golden files lock the shape. cert-manager is a declared
  dependency.
- **`dynamic-config-operator`** (0.3.0's piece, scaffolded): both CRDs
  as Rust types, `--crds` printing the manifests `deploy/` embeds (a
  `just crds` gate keeps them from drifting), and a watch-and-log loop
  the reconcilers land into.
- **`deploy/`**: the helm chart (webhook + Certificate + mutating
  configuration), kustomize base, generated CRDs. **`e2e/`**: the kind
  smoke — inject, render from a live Consul, read the file back out of
  the pod.
