#!/usr/bin/env bash
# The previous chart, upgraded to this one, with workloads in between.
#
# Every other leg here installs a fresh chart. That proves the install and
# says nothing about the thing an operator actually does on a Tuesday —
# and the failure modes of an upgrade are not the failure modes of an
# install:
#
#   - `deploy/helm/crds/` is where Helm puts CRDs it installs and **never
#     upgrades**. A schema field added in a new version is simply not
#     there after `helm upgrade`, and the operator's writes fail on a
#     field the API server prunes.
#   - The webhook's `caBundle` is patched into the MutatingWebhookConfiguration
#     by the webhook itself, and an upgrade re-renders that object from the
#     chart. If the rendered value wins, every admission in the cluster
#     fails until the rotator catches up.
#   - Pods admitted by the previous version go on running with the previous
#     agent image, and their annotations have to still mean what they meant.
#
# Nightly rather than per-PR: it installs a released chart from a registry,
# so it is the one leg here that is not hermetic.
#
# Needs: docker, kind, kubectl, helm. `PREVIOUS` overrides which version is
# upgraded from.
set -euo pipefail
cd "$(dirname "$0")/.."

cluster=dynamic-config-upgrade

# The version an operator would be coming from. Empty means "resolve the
# latest published", which is what somebody upgrading actually has.
previous="${PREVIOUS:-}"
chart_registry="${CHART_REGISTRY:-oci://ghcr.io/dynamic-config-rs/charts/dynamic-config}"

export KUBECONFIG
KUBECONFIG=$(mktemp)

kind create cluster --name "$cluster" --wait 120s

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

if [[ "${IMAGES_PRELOADED:-0}" != "1" ]]; then
  just images
fi
kind load docker-image --name "$cluster" \
  dynamic-config-agent:dev dynamic-config-webhook:dev dynamic-config-operator:dev

# ── The previous version, as published ────────────────────────────────

if [[ -n "$previous" ]]; then
  version=(--version "$previous")
else
  version=()
fi

echo "════ installing the published chart"
helm install dynamic-config "$chart_registry" "${version[@]}" \
  --namespace dynamic-config --create-namespace \
  --set operator.enabled=true
kubectl -n dynamic-config rollout status deploy/dynamic-config-webhook --timeout=180s

installed=$(helm -n dynamic-config get metadata dynamic-config -o json | \
  sed -n 's/.*"version":"\([^"]*\)".*/\1/p')
echo "════ installed chart version: ${installed:-unknown}"

# A store, and a workload admitted by the OLD webhook.
kubectl apply -f e2e/consul.yaml
kubectl rollout status deploy/consul --timeout=180s
kubectl exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9000}'

kubectl apply -f e2e/annotated-pod.yaml
kubectl wait --for=condition=Ready pod/annotated --timeout=180s

# And a CR, so the operator's objects are in the picture too.
kubectl apply -f - <<'CR'
apiVersion: dynamic-config.rs/v1alpha1
kind: DynamicConfigClass
metadata:
  name: local-consul
spec:
  consul:
    endpoint: http://consul.default.svc:8500
---
apiVersion: dynamic-config.rs/v1alpha1
kind: DynamicConfigRender
metadata:
  name: myapp
spec:
  class: local-consul
  key: myapp/config.json
  target:
    configMap: myapp-rendered
    file: config.toml
CR

for _ in $(seq 1 24); do
  kubectl get configmap myapp-rendered > /dev/null 2>&1 && break
  sleep 5
done
kubectl get configmap myapp-rendered -o jsonpath='{.data.config\.toml}' | grep 'port = 9000'

before=$(kubectl get mutatingwebhookconfiguration \
  -l app.kubernetes.io/name=dynamic-config \
  -o jsonpath='{.items[0].webhooks[0].clientConfig.caBundle}')

[ -n "$before" ] || { echo "the old install has no caBundle to preserve" >&2; exit 1; }

diagnose() {
  echo "════ webhook logs:"
  kubectl -n dynamic-config logs deploy/dynamic-config-webhook --tail=40 || true
  echo "════ operator logs:"
  kubectl -n dynamic-config logs deploy/dynamic-config-operator --tail=40 || true
  echo "════ events:"
  kubectl get events --sort-by=.lastTimestamp | tail -12 || true
}
trap 'diagnose; kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

# ── The upgrade ───────────────────────────────────────────────────────

# CRDs first, by hand. **This is the step the trap is about**: Helm
# installs `crds/` and never upgrades it, so a chart that added a schema
# field ships one the cluster does not have. Applying them here is what a
# release note has to tell an operator to do, and running it in CI is how
# we find out it is still true.
echo "════ applying CRDs by hand, as Helm will not"
kubectl apply --server-side -f deploy/helm/crds/

echo "════ upgrading to the working tree"
helm upgrade dynamic-config deploy/helm \
  --namespace dynamic-config \
  --set webhook.image=dynamic-config-webhook --set webhook.tag=dev \
  --set agent.image=dynamic-config-agent --set agent.tag=dev \
  --set operator.enabled=true \
  --set operator.image=dynamic-config-operator --set operator.tag=dev
kubectl -n dynamic-config rollout status deploy/dynamic-config-webhook --timeout=180s
kubectl -n dynamic-config rollout status deploy/dynamic-config-operator --timeout=180s

# ── What must still be true ───────────────────────────────────────────

# 1. The pod admitted by the old webhook is still running, still holding
#    its file. An upgrade must not disturb a workload it did not touch.
kubectl get pod annotated -o jsonpath='{.status.phase}' | grep -q Running
kubectl exec annotated -c app -- cat /config/rendered.toml | grep 'port = 9000'
echo "UPGRADE OK: the pod admitted before the upgrade still serves its document"

# 2. The CRs survived, and the operator went on reconciling them. A new
#    value proves the controller is alive rather than the ConfigMap merely
#    still existing.
kubectl exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9100}'

for _ in $(seq 1 24); do
  kubectl get configmap myapp-rendered -o jsonpath='{.data.config\.toml}' | grep -q 'port = 9100' && break
  sleep 5
done
kubectl get configmap myapp-rendered -o jsonpath='{.data.config\.toml}' | grep 'port = 9100'
echo "UPGRADE OK: the operator reconciles the CRs that predate the upgrade"

# 3. The caBundle survived. If the chart's rendered (empty) value won, this
#    is where the whole cluster's admissions would already be failing.
after=$(kubectl get mutatingwebhookconfiguration \
  -l app.kubernetes.io/name=dynamic-config \
  -o jsonpath='{.items[0].webhooks[0].clientConfig.caBundle}')

[ -n "$after" ] || { echo "the upgrade emptied the caBundle" >&2; exit 1; }
echo "UPGRADE OK: the caBundle survived the upgrade"

# 4. Admission still works — with the NEW webhook, and a pod that uses a
#    field this version added. A contract that admits the old shape and
#    refuses the new one is an upgrade nobody can use.
kubectl apply -f - <<'POD'
apiVersion: v1
kind: Pod
metadata:
  name: after-upgrade
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: consul
    dynamic-config.rs/endpoint: http://consul.default.svc:8500
    dynamic-config.rs/key: myapp/config.json
    dynamic-config.rs/path: /config/rendered.toml
    dynamic-config.rs/startup-policy: allow-cached
spec:
  restartPolicy: Never
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
POD

kubectl wait --for=condition=Ready pod/after-upgrade --timeout=180s
kubectl exec after-upgrade -c app -- cat /config/rendered.toml | grep 'port = 9100'
echo "UPGRADE OK: the new webhook admits a pod using a field this version added"

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

echo "UPGRADE OK: ${installed:-published} → working tree"
