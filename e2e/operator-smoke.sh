#!/usr/bin/env bash
# The 0.3.x operator gate: apply a Class and a Render, the ConfigMap
# appears with rendered content, a source change propagates on the next
# interval, and deleting the Render garbage-collects the ConfigMap
# through its owner reference.
set -euo pipefail
cd "$(dirname "$0")/.."

cluster=dynamic-config-operator-e2e

# An isolated kubeconfig: two e2e legs on one machine must not trade
# current-contexts under each other.
export KUBECONFIG
KUBECONFIG=$(mktemp)

kind create cluster --name "$cluster" --wait 120s

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

just images
kind load docker-image --name "$cluster" \
  dynamic-config-agent:dev dynamic-config-webhook:dev dynamic-config-operator:dev

helm install dynamic-config deploy/helm \
  --namespace dynamic-config --create-namespace \
  --set webhook.image=dynamic-config-webhook --set webhook.tag=dev \
  --set agent.image=dynamic-config-agent --set agent.tag=dev \
  --set operator.enabled=true \
  --set operator.image=dynamic-config-operator --set operator.tag=dev
kubectl -n dynamic-config rollout status deploy/dynamic-config-webhook --timeout=180s
kubectl -n dynamic-config rollout status deploy/dynamic-config-operator --timeout=180s

kubectl apply -f e2e/consul.yaml
kubectl rollout status deploy/consul --timeout=180s
kubectl exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9000}'

diagnose() {
  echo "════ operator logs:"
  kubectl -n dynamic-config logs deploy/dynamic-config-operator --tail=40 || true
  echo "════ the render's status:"
  kubectl get dynamicconfigrender myapp -o jsonpath='{.status}' 2>/dev/null || true; echo
  kubectl get events --sort-by=.lastTimestamp | tail -10
}
trap 'diagnose; kind delete cluster --name "$cluster"' EXIT

kubectl apply -f - <<'CRS'
apiVersion: dynamic-config.rs/v1alpha1
kind: DynamicConfigClass
metadata:
  name: consul-main
spec:
  source: consul
  endpoint: http://consul.default.svc:8500
---
apiVersion: dynamic-config.rs/v1alpha1
kind: DynamicConfigRender
metadata:
  name: myapp
spec:
  class: consul-main
  key: myapp/config.json
  target:
    configMap: myapp-rendered
    file: config.toml
  intervalSeconds: 5
CRS

# 1. The ConfigMap appears, rendered.
for _ in $(seq 1 24); do
  kubectl get configmap myapp-rendered >/dev/null 2>&1 && break
  sleep 5
done
kubectl get configmap myapp-rendered -o jsonpath='{.data.config\.toml}' | grep 'port = 9000'
echo "OPERATOR OK: the Render became a ConfigMap"

# 2. The status says so, and says when.
kubectl get dynamicconfigrender myapp -o jsonpath='{.status.renderedAt}' | grep -E '20[0-9]{2}-'
echo "OPERATOR OK: status.rendered_at is a timestamp"

# 3. A source change propagates without anyone touching the Render.
kubectl exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9100}'
for _ in $(seq 1 24); do
  kubectl get configmap myapp-rendered -o jsonpath='{.data.config\.toml}' | grep -q 'port = 9100' && break
  sleep 5
done
kubectl get configmap myapp-rendered -o jsonpath='{.data.config\.toml}' | grep 'port = 9100'
echo "OPERATOR OK: the source change propagated"

# 4. The Secret target, envEntries shape: the document's leaves land as
#    UPPER_SNAKE entries in a Secret the operator owns — the shape a
#    Secret-watching consumer (or an envFrom block) reads.
kubectl apply -f - <<'CRS'
apiVersion: dynamic-config.rs/v1alpha1
kind: DynamicConfigRender
metadata:
  name: myapp-env
spec:
  class: consul-main
  key: myapp/config.json
  target:
    secret: myapp-env
    shape: envEntries
  intervalSeconds: 5
CRS

for _ in $(seq 1 24); do
  kubectl get secret myapp-env >/dev/null 2>&1 && break
  sleep 5
done
kubectl get secret myapp-env -o jsonpath='{.data.PORT}' | base64 -d | grep '9100'
kubectl get secret myapp-env -o jsonpath='{.data.HOST}' | base64 -d | grep 'db.internal'
echo "OPERATOR OK: the Secret target carries envEntries"

kubectl delete dynamicconfigrender myapp-env
for _ in $(seq 1 24); do
  kubectl get secret myapp-env >/dev/null 2>&1 || break
  sleep 5
done
if kubectl get secret myapp-env >/dev/null 2>&1; then
  echo "the Secret survived its owner" >&2
  exit 1
fi
echo "OPERATOR OK: the Secret target is owner-collected too"

# 5. The cluster class: platform-owned store, tenant namespace allowed
#    by the list, credential nowhere near the tenant.
kubectl create namespace tenant
kubectl apply -f - <<'CRS'
apiVersion: dynamic-config.rs/v1alpha1
kind: ClusterDynamicConfigClass
metadata:
  name: platform-consul
spec:
  source: consul
  endpoint: http://consul.default.svc:8500
  namespaces: [tenant]
---
apiVersion: dynamic-config.rs/v1alpha1
kind: DynamicConfigRender
metadata:
  name: tenant-app
  namespace: tenant
spec:
  class: platform-consul
  classKind: ClusterDynamicConfigClass
  key: myapp/config.json
  target:
    configMap: tenant-app
    file: config.toml
  intervalSeconds: 5
CRS

for _ in $(seq 1 24); do
  kubectl -n tenant get configmap tenant-app >/dev/null 2>&1 && break
  sleep 5
done
kubectl -n tenant get configmap tenant-app -o jsonpath='{.data.config\.toml}' | grep 'port = 9100'
echo "OPERATOR OK: the cluster class rendered into an allowed tenant"

# …and the allowlist refuses a namespace it does not name.
kubectl apply -f - <<'CRS'
apiVersion: dynamic-config.rs/v1alpha1
kind: DynamicConfigRender
metadata:
  name: trespasser
  namespace: default
spec:
  class: platform-consul
  classKind: ClusterDynamicConfigClass
  key: myapp/config.json
  target:
    configMap: trespasser
    file: config.toml
  intervalSeconds: 5
CRS

for _ in $(seq 1 24); do
  kubectl get dynamicconfigrender trespasser -o jsonpath='{.status.lastError}' 2>/dev/null | grep -q 'does not allow' && break
  sleep 5
done
kubectl get dynamicconfigrender trespasser -o jsonpath='{.status.lastError}' | grep 'does not allow namespace'
if kubectl get configmap trespasser >/dev/null 2>&1; then
  echo "the allowlist did not hold" >&2
  exit 1
fi
echo "OPERATOR OK: the allowlist held, and the refusal names the class"

# 6. Deleting the Render garbage-collects the ConfigMap: the owner
#    reference is the cleanup, no finalizer to get wrong.
kubectl delete dynamicconfigrender myapp
for _ in $(seq 1 24); do
  kubectl get configmap myapp-rendered >/dev/null 2>&1 || break
  sleep 5
done
if kubectl get configmap myapp-rendered >/dev/null 2>&1; then
  echo "the ConfigMap survived its owner" >&2
  exit 1
fi
echo "OPERATOR OK: the owner reference cleaned up"

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT
echo "OPERATOR SMOKE OK: Class + Render → ConfigMap, propagation, GC"
