#!/usr/bin/env bash
# What this costs at a size somebody would actually run.
#
# The webhook has had an admission-latency histogram since 0.2.0 and
# nothing has ever driven load through it, so the claim in its own source
# — "tens of microseconds when it is well" — has been an assertion rather
# than a measurement. This drives it.
#
# Three numbers, because they are the three an operator is deciding on:
#
#   admission p50/p95/p99   how much of the API server's ten-second
#                           budget one webhook takes
#   agent RSS               multiplied by every sidecar in the cluster
#   webhook RSS             one deployment, but on the path of every pod
#
# Output, not a gate. A shared CI runner cannot produce a number worth
# publishing as a guarantee, and a threshold tuned on one would fail on
# another — so this prints, records, and only fails on the things that are
# *qualitatively* wrong: an admission that took seconds, or a refusal.
#
# Needs: docker, kind, kubectl, helm.
set -euo pipefail
cd "$(dirname "$0")/.."

cluster=dynamic-config-scale

# How many pods to admit. Small enough for CI, large enough that the
# histogram has a distribution rather than a handful of samples.
pods="${SCALE_PODS:-200}"

# The one thing worth failing on: an admission that ate a meaningful part
# of the API server's budget. Ten seconds is the default timeout; a p99
# past a second means something is wrong in kind rather than slow.
ceiling_ms="${SCALE_CEILING_MS:-1000}"

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

diagnose() {
  echo "════ webhook logs:"
  kubectl -n dynamic-config logs deploy/dynamic-config-webhook --tail=30 || true
}
trap 'diagnose; kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

# ── Admission under load ──────────────────────────────────────────────
#
# Created as one manifest rather than N `kubectl` calls: the thing being
# measured is the webhook, and a per-pod process start would swamp it.

echo "════ admitting $pods pods"

{
  for index in $(seq 1 "$pods"); do
    cat <<POD
---
apiVersion: v1
kind: Pod
metadata:
  name: scale-$index
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: consul
    dynamic-config.rs/endpoint: http://consul.default.svc:8500
    dynamic-config.rs/key: myapp/config.json
    dynamic-config.rs/path: /config/rendered.toml
spec:
  restartPolicy: Never
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
POD
  done
} > /tmp/scale-pods.yaml

started=$(date +%s)
kubectl apply -f /tmp/scale-pods.yaml > /dev/null
elapsed=$(( $(date +%s) - started ))

admitted=$(kubectl get pods -o name | grep -c '^pod/scale-' || true)

echo "  $admitted of $pods admitted in ${elapsed}s"

if [[ "$admitted" -ne "$pods" ]]; then
  echo "the webhook refused $((pods - admitted)) pods under load" >&2
  exit 1
fi

# ── The histogram the webhook has been keeping ────────────────────────

metrics=$(kubectl -n dynamic-config exec deploy/dynamic-config-webhook -- \
  wget -qO- http://127.0.0.1:9091/metrics 2>/dev/null || true)

if [[ -z "$metrics" ]]; then
  # The metrics port is a chart value; a deployment that moved it is not a
  # failure of this leg.
  echo "  (the webhook's metrics port did not answer; skipping the histogram)"
else
  echo
  echo "════ admission duration, as the webhook measured it"
  echo "$metrics" | grep '^dynamic_config_admission_duration_seconds' | sed 's/^/  /'

  # The p99 bucket the samples actually reached: the smallest bucket whose
  # cumulative count covers 99% of them.
  total=$(echo "$metrics" | sed -n 's/^dynamic_config_admission_duration_seconds_count \(.*\)/\1/p')

  if [[ -n "$total" && "$total" -gt 0 ]]; then
    target=$(( (total * 99 + 99) / 100 ))
    reached=$(echo "$metrics" \
      | sed -n 's/^dynamic_config_admission_duration_seconds_bucket{le="\([^"]*\)"} \(.*\)/\1 \2/p' \
      | awk -v t="$target" '$2 >= t { print $1; exit }')

    echo
    echo "  p99 falls in the ≤${reached}s bucket, over $total admissions"

    # Through `awk` because the bucket boundary is a float and the shell
    # only does integers; `print` rather than an exit status, so a false
    # comparison is an answer rather than a failure under `set -e`.
    over=$(awk -v r="${reached:-0}" -v c="$ceiling_ms" 'BEGIN { print (r * 1000 > c) ? 1 : 0 }')

    if [[ "$over" == "1" ]]; then
      echo "p99 is past the ${ceiling_ms}ms ceiling" >&2
      exit 1
    fi
  fi
fi

# ── What a sidecar costs, times every pod in the cluster ──────────────

kubectl wait --for=condition=Ready pod/scale-1 --timeout=180s

echo
echo "════ resident memory"

# `kubectl top` needs metrics-server, which kind does not ship — and the
# agent's image is distroless, so there is no shell in it to read a cgroup
# with either. The kubelet's own summary API answers both problems: it
# reports per *container*, and it needs nothing inside the container.
node=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
summary=$(kubectl get --raw "/api/v1/nodes/${node}/proxy/stats/summary" 2>/dev/null || echo '{}')

# Working set rather than RSS: it is what the kubelet compares against the
# limit when it decides whether to evict, which is the number that decides
# whether 25,000 sidecars fit.
rss_of() {
  echo "$summary" | python3 -c '
import json, sys
pod, container = sys.argv[1], sys.argv[2]
summary = json.load(sys.stdin)
for entry in summary.get("pods", []):
    if entry.get("podRef", {}).get("name") != pod:
        continue
    for held in entry.get("containers", []):
        if held.get("name") == container:
            print(held.get("memory", {}).get("workingSetBytes", 0))
            raise SystemExit
print(0)
' "$1" "$2"
}

agent_rss=$(rss_of scale-1 dynamic-config-agent)
webhook_pod=$(kubectl -n dynamic-config get pods -l app.kubernetes.io/component=webhook \
  -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
webhook_rss=$(rss_of "${webhook_pod:-none}" dynamic-config-webhook)

echo "  agent    $(( agent_rss / 1024 ))KiB per sidecar × $pods = $(( agent_rss * pods / 1024 / 1024 ))MiB"
echo "  webhook  $(( webhook_rss / 1024 ))KiB, once"

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### scale"
    echo
    echo "| pods admitted | in | agent working set | × $pods | webhook |"
    echo "|---|---|---|---|---|"
    echo "| $admitted | ${elapsed}s | $(( agent_rss / 1024 ))KiB | $(( agent_rss * pods / 1024 / 1024 ))MiB | $(( webhook_rss / 1024 ))KiB |"
  } >> "$GITHUB_STEP_SUMMARY"
fi

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

echo
echo "SCALE OK: $admitted pods admitted, none refused"
