#!/usr/bin/env bash
# The selfRotate gate: rotations under admission load, zero refused
# handshakes across every rotation. `failurePolicy: Fail` on purpose —
# with Ignore, a refused handshake would admit the pod un-injected and
# hide exactly the failure this soak exists to catch.
#
# SOAK_HOURS picks the length (default 1); the pair's validity is
# shortened so a rotation lands roughly every 20-25 minutes.
set -euo pipefail
cd "$(dirname "$0")/.."

hours="${SOAK_HOURS:-1}"
cluster=dynamic-config-rotation-soak

# An isolated kubeconfig: two e2e legs on one machine must not trade
# current-contexts under each other.
export KUBECONFIG
KUBECONFIG=$(mktemp)

kind create cluster --name "$cluster" --wait 120s

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

# CI builds the three images once for the whole run and hands them over
# as a tarball; a local run builds them here. Either way what follows
# loads `:dev` tags into kind.
if [[ "${IMAGES_PRELOADED:-0}" != "1" ]]; then
  just images
fi
kind load docker-image --name "$cluster" \
  dynamic-config-agent:dev dynamic-config-webhook:dev

helm install dynamic-config deploy/helm \
  --namespace dynamic-config --create-namespace \
  --set webhook.image=dynamic-config-webhook --set webhook.tag=dev \
  --set agent.image=dynamic-config-agent --set agent.tag=dev \
  --set webhook.selfRotate.enabled=true \
  --set webhook.replicas=2 \
  --set webhook.failurePolicy=Fail \
  --set webhook.namespaceGating=true \
  --set-json 'webhook.extraEnv=[{"name":"DYNAMIC_CONFIG_WEBHOOK_VALIDITY_SECONDS","value":"3600"}]'
kubectl -n dynamic-config rollout status deploy/dynamic-config-webhook --timeout=180s

# Gating on, so Fail is a promise scoped to THIS namespace.
kubectl create namespace soak
kubectl label namespace soak dynamic-config.rs/injection=enabled

kubectl apply -n soak -f e2e/consul.yaml
kubectl -n soak rollout status deploy/consul --timeout=180s
kubectl -n soak exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9000}'

# Wait for the first minted pair to reach the MWC.
for _ in $(seq 1 36); do
  bundle=$(kubectl get mutatingwebhookconfiguration dynamic-config \
    -o jsonpath='{.webhooks[0].clientConfig.caBundle}' 2>/dev/null || true)
  [ -n "$bundle" ] && break
  sleep 5
done
[ -n "$bundle" ] || { echo "the first rotation never landed"; exit 1; }

deadline=$(( $(date +%s) + hours * 3600 ))
created=0
failures=0
bundles=1
last_bundle="$bundle"

while [ "$(date +%s)" -lt "$deadline" ]; do
  name="load-$created"

  if kubectl run -n soak "$name" --image=busybox:1.36 --restart=Never \
       --annotations=dynamic-config.rs/inject=true \
       --annotations=dynamic-config.rs/source=consul \
       --annotations=dynamic-config.rs/endpoint=http://consul.soak.svc:8500 \
       --annotations=dynamic-config.rs/key=myapp/config.json \
       --annotations=dynamic-config.rs/path=/config/rendered.toml \
       --overrides='{"spec":{"containers":[{"name":"load","image":"busybox:1.36","command":["sleep","30"]}]}}' \
       >/dev/null 2>&1; then
    :
  else
    failures=$((failures + 1))
    echo "REFUSED at $(date -u +%T): admission failed with failurePolicy=Fail"
  fi

  created=$((created + 1))
  kubectl delete pod -n soak "$name" --wait=false >/dev/null 2>&1 || true

  now_bundle=$(kubectl get mutatingwebhookconfiguration dynamic-config \
    -o jsonpath='{.webhooks[0].clientConfig.caBundle}')

  if [ "$now_bundle" != "$last_bundle" ]; then
    bundles=$((bundles + 1))
    last_bundle="$now_bundle"
    echo "ROTATION $bundles observed at $(date -u +%T), after $created admissions"
  fi

  sleep 20
done

echo "soak over: $created admissions, $failures refused, $bundles CA generations"

[ "$failures" -eq 0 ] || { echo "refused handshakes under rotation"; exit 1; }

expected=$(( hours * 3600 / 1500 ))
if [ "$bundles" -le 1 ] && [ "$expected" -gt 1 ]; then
  echo "no rotation happened in $hours hour(s) with 1h validity"
  exit 1
fi

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT
echo "ROTATION SOAK OK: $bundles generations, zero refusals, $created admissions"
