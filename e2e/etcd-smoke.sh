#!/usr/bin/env bash
# The 0.2.0 gate: etcd through the async agent, BOTH of etcd's methods —
# username/password against the running server, and a TLS client
# certificate presented on the channel. One kind cluster, one etcd, two
# annotated pods.
#
# Needs: docker, kind, kubectl, helm, openssl. CI's e2e workflow calls
# exactly this; running it locally is the same test.
set -euo pipefail
cd "$(dirname "$0")/.."

cluster=dynamic-config-etcd-e2e

# An isolated kubeconfig: two e2e legs on one machine must not trade
# current-contexts under each other.
export KUBECONFIG
KUBECONFIG=$(mktemp)

kind create cluster --name "$cluster" --wait 120s

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

just images
kind load docker-image --name "$cluster" \
  dynamic-config-agent:dev dynamic-config-webhook:dev

helm install dynamic-config deploy/helm \
  --namespace dynamic-config --create-namespace \
  --set webhook.image=dynamic-config-webhook --set webhook.tag=dev \
  --set agent.image=dynamic-config-agent --set agent.tag=dev
kubectl -n dynamic-config rollout status deploy/dynamic-config-webhook --timeout=180s

# ── A CA of the test's own, a server cert, and a client cert ──────────
work=$(mktemp -d)

openssl req -x509 -newkey rsa:2048 -nodes -days 2 \
  -keyout "$work/ca.key" -out "$work/ca.crt" -subj "/CN=e2e-ca" 2>/dev/null
openssl req -newkey rsa:2048 -nodes \
  -keyout "$work/server.key" -out "$work/server.csr" -subj "/CN=etcd" 2>/dev/null
openssl x509 -req -in "$work/server.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
  -CAcreateserial -days 2 -out "$work/server.crt" \
  -extfile <(echo "subjectAltName=DNS:etcd-password,DNS:etcd-password.default.svc,DNS:etcd-mtls,DNS:etcd-mtls.default.svc,IP:127.0.0.1") 2>/dev/null
openssl req -newkey rsa:2048 -nodes \
  -keyout "$work/client.key" -out "$work/client.csr" -subj "/CN=myapp" 2>/dev/null
openssl x509 -req -in "$work/client.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
  -CAcreateserial -days 2 -out "$work/client.crt" 2>/dev/null

kubectl create secret tls etcd-server-tls \
  --cert="$work/server.crt" --key="$work/server.key"
kubectl get secret etcd-server-tls -o json | python3 -c '
import base64, json, sys
secret = json.load(sys.stdin)
with open(sys.argv[1], "rb") as ca:
    secret["data"]["ca.crt"] = base64.b64encode(ca.read()).decode()
secret["metadata"] = {"name": "etcd-server-tls"}
json.dump(secret, sys.stdout)
' "$work/ca.crt" | kubectl replace -f -
kubectl create secret tls etcd-client-tls \
  --cert="$work/client.crt" --key="$work/client.key"
kubectl create configmap etcd-ca --from-file=ca.crt="$work/ca.crt"

kubectl apply -f e2e/etcd.yaml
kubectl rollout status deploy/etcd-password --timeout=180s
kubectl rollout status deploy/etcd-mtls --timeout=180s

# etcd v3.6's image has etcdctl and nothing else — no shell, no env.
# The password server takes plain TLS; the mTLS one requires the client
# certificate, and the mounted server pair (same CA, no EKU restriction)
# serves as one for seeding.
pwexec() {
  kubectl exec deploy/etcd-password -- etcdctl \
    --endpoints=https://127.0.0.1:2379 --insecure-skip-tls-verify=true "$@"
}
mtlsexec() {
  kubectl exec deploy/etcd-mtls -- etcdctl \
    --endpoints=https://127.0.0.1:2379 --insecure-skip-tls-verify=true \
    --cert=/etc/etcd-tls/tls.crt --key=/etc/etcd-tls/tls.key "$@"
}

# The password server: a user, a role, a document, auth on.
pwexec put myapp/config.json '{"host": "db.internal", "port": 9200}'
pwexec user add root:rootpw
pwexec role add root
pwexec user grant-role root root
pwexec user add myapp:hunter2
pwexec role add reader
pwexec role grant-permission reader read myapp/config.json
pwexec user grant-role myapp reader
pwexec auth enable

# The mTLS server: the document; the certificate is the credential.
mtlsexec put myapp/config.json '{"host": "db.internal", "port": 9200}'

kubectl create secret generic etcd-password --from-literal=password=hunter2

diagnose() {
  echo "════ webhook logs:"
  kubectl -n dynamic-config logs deploy/dynamic-config-webhook --tail=30 || true
  for pod in etcd-password-pod etcd-tls-pod; do
    echo "════ $pod agent logs:"
    kubectl logs "$pod" -c dynamic-config-agent --tail=30 2>/dev/null || true
  done
  kubectl get events --sort-by=.lastTimestamp | tail -12
}
trap 'diagnose; kind delete cluster --name "$cluster"' EXIT

# ── Method one: username/password ──────────────────────────────────────
kubectl apply -f - <<'POD'
apiVersion: v1
kind: Pod
metadata:
  name: etcd-password-pod
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "etcd"
    dynamic-config.rs/endpoint: "https://etcd-password.default.svc:2379"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    dynamic-config.rs/auth-username: "myapp"
    dynamic-config.rs/password-secret: "etcd-password/password"
    dynamic-config.rs/ca-configmap: "etcd-ca"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "600"]
POD

# ── Method two: the client certificate IS the credential ──────────────
# The mTLS server refuses any client without a certificate this CA
# signed, so a successful render proves the agent presented the pair.
kubectl apply -f - <<'POD'
apiVersion: v1
kind: Pod
metadata:
  name: etcd-tls-pod
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "etcd"
    dynamic-config.rs/endpoint: "https://etcd-mtls.default.svc:2379"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    dynamic-config.rs/tls-secret: "etcd-client-tls"
    dynamic-config.rs/ca-configmap: "etcd-ca"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "600"]
POD

for pod in etcd-password-pod etcd-tls-pod; do
  kubectl wait --for=condition=Ready "pod/$pod" --timeout=180s
  kubectl exec "$pod" -c app -- cat /config/rendered.toml | grep 'port = 9200'
  echo "ETCD OK: $pod rendered through its method"
done

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT
echo "ETCD SMOKE OK: both of etcd's methods, through the async agent"
