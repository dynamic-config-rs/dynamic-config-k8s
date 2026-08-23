#!/usr/bin/env bash
# The CSI node plugin, against a real kubelet.
#
# The node agent's central claim is that two pods on one node wanting the
# same document share one fetch and one watch. Unit tests cover the sharing
# registry and the volume parsing; nothing until this had published a volume
# to a kubelet at all — and a CSI driver that has never been mounted is a
# gRPC server with an opinion.
#
# What this proves, in order:
#
#   the kubelet registers the driver
#   a pod with a CSI volume starts, and its file is there BEFORE it does
#   a second pod on the same key shares the watch — one document, two readers
#   a change in the store reaches both files
#   deleting one pod leaves the other's watch alone
#
# Needs: docker, kind, kubectl, helm. Same as every other leg here.
set -euo pipefail
cd "$(dirname "$0")/.."

cluster=dynamic-config-node-agent

export KUBECONFIG
KUBECONFIG=$(mktemp)

kind create cluster --name "$cluster" --wait 120s

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

if [[ "${IMAGES_PRELOADED:-0}" != "1" ]]; then
  just images
fi
kind load docker-image --name "$cluster" \
  dynamic-config-node-agent:dev dynamic-config-webhook:dev

# The webhook comes along because the chart installs it; this leg does not
# annotate anything, and that is part of the point — a CSI volume never
# passes through admission.
helm install dynamic-config deploy/helm \
  --namespace dynamic-config --create-namespace \
  --set webhook.image=dynamic-config-webhook --set webhook.tag=dev \
  --set nodeAgent.enabled=true \
  --set nodeAgent.image=dynamic-config-node-agent --set nodeAgent.tag=dev

kubectl -n dynamic-config rollout status daemonset/dynamic-config-node-agent --timeout=180s

diagnose() {
  echo "════ the node agent's logs:"
  kubectl -n dynamic-config logs daemonset/dynamic-config-node-agent -c node-agent --tail=40 || true
  echo "════ the registrar's logs:"
  kubectl -n dynamic-config logs daemonset/dynamic-config-node-agent -c node-driver-registrar --tail=20 || true
  echo "════ events:"
  kubectl get events --sort-by=.lastTimestamp | tail -15 || true
}
trap 'diagnose; kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

# ── The kubelet knows the driver ──────────────────────────────────────
#
# `CSINode` is the kubelet's own record of what registered on it. If this
# is empty the registrar never got through, and everything below would
# fail with a mount error that says nothing about why.

for _ in $(seq 1 24); do
  kubectl get csinode -o jsonpath='{.items[*].spec.drivers[*].name}' \
    | grep -q 'config.dynamic-config.rs' && break
  sleep 5
done

kubectl get csinode -o jsonpath='{.items[*].spec.drivers[*].name}' \
  | grep -q 'config.dynamic-config.rs' \
  || { echo "the kubelet never registered the driver" >&2; exit 1; }

echo "NODE-AGENT OK: the kubelet registered the driver"

kubectl apply -f e2e/consul.yaml
kubectl rollout status deploy/consul --timeout=180s
kubectl exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9000}'

# ── Two pods, one document ────────────────────────────────────────────
#
# The same source, endpoint and key, which is what makes them one read.

for name in reader-a reader-b; do
  kubectl apply -f - <<POD
apiVersion: v1
kind: Pod
metadata:
  name: $name
spec:
  restartPolicy: Never
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
      volumeMounts:
        - name: config
          mountPath: /config
  volumes:
    - name: config
      csi:
        driver: config.dynamic-config.rs
        readOnly: true
        volumeAttributes:
          source: consul
          endpoint: http://consul.default.svc:8500
          key: myapp/config.json
          path: rendered.toml
POD
done

kubectl wait --for=condition=Ready pod/reader-a pod/reader-b --timeout=180s

# The property a sidecar cannot offer: the kubelet does not start a pod's
# containers until every volume is published, and publishing is what does
# the first fetch. So the file is there the instant the container is, with
# no init container and no window in which it is missing.
for name in reader-a reader-b; do
  kubectl exec "$name" -- cat /config/rendered.toml | grep 'port = 9000'
done

echo "NODE-AGENT OK: both pods had their document before they started"

# ── One fetch, two readers ────────────────────────────────────────────
#
# The whole reason this component exists. `documents` counts the watches;
# `readers` counts the pod volumes reading them. Equal numbers would mean
# nothing was shared and a sidecar would have cost the same.

# The plugin's image is distroless, so there is no `wget` inside it to
# exec — this used to try, get nothing, and skip itself with a `|| true`,
# which is an assertion that has never once run. The consul pod is alpine
# and is already here, so it is the probe.

agent=$(kubectl -n dynamic-config get pods -l app.kubernetes.io/component=node-agent \
  -o jsonpath='{.items[0].metadata.name}')

agent_ip=$(kubectl -n dynamic-config get pod "$agent" -o jsonpath='{.status.podIP}')

metrics=$(kubectl exec deploy/consul -- \
  wget -qO- "http://${agent_ip}:9111/metrics") \
  || { echo "the node agent's metrics port did not answer" >&2; exit 1; }

echo "$metrics" | sed 's/^/  /'

documents=$(echo "$metrics" | sed -n 's/^dynamic_config_node_agent_documents \(.*\)/\1/p')
readers=$(echo "$metrics" | sed -n 's/^dynamic_config_node_agent_readers \(.*\)/\1/p')

[[ "$documents" == "1" ]] \
  || { echo "two pods on one key should be one watch, not $documents" >&2; exit 1; }
[[ "$readers" == "2" ]] \
  || { echo "one watch should have two readers, not $readers" >&2; exit 1; }

echo "NODE-AGENT OK: one document, two readers"

# ── A change reaches both ─────────────────────────────────────────────

kubectl exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9100}'

for name in reader-a reader-b; do
  if ! timeout 60 kubectl exec "$name" -- sh -c \
    "until grep -q 9100 /config/rendered.toml 2>/dev/null; do sleep 1; done"; then
    echo "$name never saw the change" >&2
    exit 1
  fi
done

echo "NODE-AGENT OK: one watch delivered to both pods"

# ── One reader leaving does not take the other's watch ────────────────

kubectl delete pod reader-a --wait=true

kubectl exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9200}'

if ! timeout 60 kubectl exec reader-b -- sh -c \
  "until grep -q 9200 /config/rendered.toml 2>/dev/null; do sleep 1; done"; then
  echo "reader-b stopped receiving when reader-a left" >&2
  exit 1
fi

echo "NODE-AGENT OK: the surviving reader kept its watch"

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

echo "NODE-AGENT OK: registered, published, shared, delivered and released"
