#!/usr/bin/env bash
# How long a change takes to become visible inside a pod.
#
# The claim this repository makes is that a change reaches an application's
# file quickly. Every other leg in this directory proves *correctness* —
# the document arrives, and it is the right one — and none of them has ever
# produced a number. A claim with no measurement is the thing the parity
# document says not to make.
#
# What is measured, end to end:
#
#   consul kv put ──▶ watch delivery ──▶ resolve ──▶ render ──▶ rename
#                                                                 │
#                                     the application's read ◀────┘
#
# The clock starts before the write and stops when a shell inside the
# application's container can read the new value. Nothing here inspects the
# agent's own metrics: the number worth publishing is the one a consumer
# experiences, and an internal timestamp would flatter it by the width of
# the rename.
#
# This is a **regression test**, not a benchmark for a README. It runs on a
# shared CI runner beside two other kind clusters, so the ceilings below are
# generous and the interesting output is the distribution, not the pass.
#
# Needs: docker, kind, kubectl, helm. Same as every other leg here.
set -euo pipefail
cd "$(dirname "$0")/.."

cluster=dynamic-config-latency

# How many changes to time. Ten is enough for a median and a p95 that mean
# something, and short enough that the leg is not the slowest thing in CI.
rounds="${LATENCY_ROUNDS:-10}"

# The ceiling, in milliseconds. Consul is a `Native` store — a blocking
# query returns the moment the value moves — so this is not an interval, it
# is a round trip plus a parse plus a write. Three seconds is far past that
# and still catches the regression this exists for: a delivery path that
# quietly went back to polling.
ceiling="${LATENCY_CEILING_MS:-3000}"

export KUBECONFIG
KUBECONFIG=$(mktemp)

kind create cluster --name "$cluster" --wait 120s

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

if [[ "${IMAGES_PRELOADED:-0}" != "1" ]]; then
  just images
fi
kind load docker-image --name "$cluster" \
  dynamic-config-agent:dev dynamic-config-webhook:dev

helm install dynamic-config deploy/helm \
  --namespace dynamic-config --create-namespace \
  --set webhook.image=dynamic-config-webhook --set webhook.tag=dev \
  --set agent.image=dynamic-config-agent --set agent.tag=dev
kubectl -n dynamic-config rollout status deploy/dynamic-config-webhook --timeout=180s

kubectl apply -f e2e/consul.yaml
kubectl rollout status deploy/consul --timeout=180s
kubectl exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9000}'

# The watching agent, and nothing else: no init container, so the number is
# a *change* being delivered rather than a pod starting.
kubectl apply -f - <<'POD'
apiVersion: v1
kind: Pod
metadata:
  name: timed
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: consul
    dynamic-config.rs/endpoint: http://consul.default.svc:8500
    dynamic-config.rs/key: myapp/config.json
    dynamic-config.rs/path: /config/rendered.toml
    dynamic-config.rs/mode: sidecar
    # Long, deliberately. A short interval would mean the resync could
    # deliver the change and the number would be measuring a *poll*, which
    # is exactly the regression this leg exists to catch.
    dynamic-config.rs/watch-seconds: "300"
spec:
  restartPolicy: Never
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
POD

kubectl wait --for=condition=Ready pod/timed --timeout=180s

diagnose() {
  echo "════ the agent's logs:"
  kubectl logs timed -c dynamic-config-agent --tail=40 || true
  echo "════ events:"
  kubectl get events --sort-by=.lastTimestamp | tail -12 || true
}
trap 'diagnose; kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

# The first render has already happened — the pod is Ready — so every
# round below times a *change*.
measurements=()

for round in $(seq 1 "$rounds"); do
  # A value nothing else can produce, so "the file changed" cannot be
  # satisfied by a leftover.
  marker=$((10000 + round))

  started=$(date +%s%3N)
  kubectl exec deploy/consul -- \
    consul kv put myapp/config.json "{\"host\": \"db.internal\", \"port\": $marker}" > /dev/null

  # Busy-poll from inside the application's container: this is the read an
  # application does, and its cost is part of what is being measured.
  # `timeout` bounds a round that never arrives, so a broken delivery path
  # fails the leg instead of hanging CI.
  if ! timeout 30 kubectl exec timed -c app -- sh -c \
    "until grep -q '$marker' /config/rendered.toml 2>/dev/null; do :; done"; then
    echo "round $round: the change never became visible" >&2
    exit 1
  fi

  ended=$(date +%s%3N)
  elapsed=$((ended - started))

  measurements+=("$elapsed")
  echo "round $round: ${elapsed}ms"
done

# Sorted, so the percentiles below are percentiles.
readarray -t sorted < <(printf '%s\n' "${measurements[@]}" | sort -n)

count=${#sorted[@]}
p50=${sorted[$((count / 2))]}
p95=${sorted[$(((count * 95 - 1) / 100))]}
worst=${sorted[$((count - 1))]}

echo
echo "════ change → visible, over $count rounds"
echo "  p50   ${p50}ms"
echo "  p95   ${p95}ms"
echo "  max   ${worst}ms"

# Into the job summary, so a release can quote a number somebody measured
# rather than one somebody remembered.
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### change → visible"
    echo
    echo "Consul (\`Native\`), $count rounds, one watching agent."
    echo
    echo "| p50 | p95 | max |"
    echo "|---|---|---|"
    echo "| ${p50}ms | ${p95}ms | ${worst}ms |"
  } >> "$GITHUB_STEP_SUMMARY"
fi

# The p95 rather than the max: one scheduling hiccup on a shared runner is
# not a regression, and a leg that fails on it is a leg people learn to
# ignore.
if (( p95 > ceiling )); then
  echo "p95 ${p95}ms is past the ${ceiling}ms ceiling" >&2
  exit 1
fi

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

echo "LATENCY OK: p95 ${p95}ms, under the ${ceiling}ms ceiling"
