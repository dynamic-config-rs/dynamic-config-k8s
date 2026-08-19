#!/usr/bin/env bash
# Every feasible live store, ONE kind cluster and ONE image build:
# etcd (both of its auth methods — password against one server, a
# required client certificate against another), vault, redis, NATS and
# S3 (MinIO) each seed a document and an annotated pod renders it —
# the annotation → webhook → agent → file wiring, per store.
#
# Deliberately NOT here, with reasons: consul rides the PR smoke (and
# the operator leg); git, firestore and config-server's CLIENTS are
# exercised against real servers by the remote repository's container
# suites, and their agent arms are flag plumbing identical to the six
# below — a gitea/emulator orchestration in kind would re-buy that
# coverage at ten times the price.
set -euo pipefail
cd "$(dirname "$0")/.."

cluster=dynamic-config-stores-e2e

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

# ── etcd's certificates: a CA of the test's own, v3 leaves ─────────────
work=$(mktemp -d)

openssl req -x509 -newkey rsa:2048 -nodes -days 2 \
  -keyout "$work/ca.key" -out "$work/ca.crt" -subj "/CN=e2e-ca" 2>/dev/null
openssl req -newkey rsa:2048 -nodes \
  -keyout "$work/server.key" -out "$work/server.csr" -subj "/CN=etcd" 2>/dev/null
openssl x509 -req -in "$work/server.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
  -CAcreateserial -days 2 -out "$work/server.crt" \
  -extfile <(printf 'extendedKeyUsage=serverAuth\nsubjectAltName=DNS:etcd-password,DNS:etcd-password.default.svc,DNS:etcd-mtls,DNS:etcd-mtls.default.svc,IP:127.0.0.1\n') 2>/dev/null
openssl req -newkey rsa:2048 -nodes \
  -keyout "$work/client.key" -out "$work/client.csr" -subj "/CN=myapp" 2>/dev/null
# v3 with clientAuth EKU on purpose: an extension-less `x509 -req` mints
# a v1 certificate, and Go's TLS stack (etcd's) is entitled to refuse it.
openssl x509 -req -in "$work/client.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
  -CAcreateserial -days 2 -out "$work/client.crt" \
  -extfile <(printf 'extendedKeyUsage=clientAuth\nsubjectAltName=DNS:myapp\n') 2>/dev/null

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

# ── The other four stores ──────────────────────────────────────────────
kubectl apply -f - <<'STORES'
apiVersion: apps/v1
kind: Deployment
metadata: { name: vault }
spec:
  replicas: 1
  selector: { matchLabels: { app: vault } }
  template:
    metadata: { labels: { app: vault } }
    spec:
      containers:
        - name: vault
          image: hashicorp/vault:1.20
          env:
            - { name: VAULT_DEV_ROOT_TOKEN_ID, value: e2e-root }
          ports: [{ containerPort: 8200 }]
---
apiVersion: v1
kind: Service
metadata: { name: vault }
spec: { selector: { app: vault }, ports: [{ port: 8200 }] }
---
apiVersion: apps/v1
kind: Deployment
metadata: { name: redis }
spec:
  replicas: 1
  selector: { matchLabels: { app: redis } }
  template:
    metadata: { labels: { app: redis } }
    spec:
      containers:
        - name: redis
          image: redis:7-alpine
          ports: [{ containerPort: 6379 }]
---
apiVersion: v1
kind: Service
metadata: { name: redis }
spec: { selector: { app: redis }, ports: [{ port: 6379 }] }
---
apiVersion: apps/v1
kind: Deployment
metadata: { name: nats }
spec:
  replicas: 1
  selector: { matchLabels: { app: nats } }
  template:
    metadata: { labels: { app: nats } }
    spec:
      containers:
        - name: nats
          image: nats:2.10-alpine
          args: ["-js"]
          ports: [{ containerPort: 4222 }]
---
apiVersion: v1
kind: Service
metadata: { name: nats }
spec: { selector: { app: nats }, ports: [{ port: 4222 }] }
---
apiVersion: apps/v1
kind: Deployment
metadata: { name: minio }
spec:
  replicas: 1
  selector: { matchLabels: { app: minio } }
  template:
    metadata: { labels: { app: minio } }
    spec:
      containers:
        - name: minio
          image: minio/minio:RELEASE.2024-12-18T13-15-44Z
          args: ["server", "/data"]
          env:
            - { name: MINIO_ROOT_USER, value: e2e-access }
            - { name: MINIO_ROOT_PASSWORD, value: e2e-secret-key }
          ports: [{ containerPort: 9000 }]
---
apiVersion: v1
kind: Service
metadata: { name: minio }
spec: { selector: { app: minio }, ports: [{ port: 9000 }] }
STORES

for d in etcd-password etcd-mtls vault redis nats minio; do
  kubectl rollout status "deploy/$d" --timeout=180s
done

# etcd v3.6's image has etcdctl and nothing else — no shell, no env.
pwexec() {
  kubectl exec deploy/etcd-password -- etcdctl \
    --endpoints=https://127.0.0.1:2379 --insecure-skip-tls-verify=true "$@"
}
mtlsexec() {
  kubectl exec deploy/etcd-mtls -- etcdctl \
    --endpoints=https://127.0.0.1:2379 --insecure-skip-tls-verify=true \
    --cert=/etc/etcd-client/tls.crt --key=/etc/etcd-client/tls.key "$@"
}

# The password server: a user, a role, a document, auth on.
pwexec put myapp/config.json '{"host": "etcd-db", "port": 9201}'
pwexec user add root:rootpw
pwexec role add root
pwexec user grant-role root root
pwexec user add myapp:hunter2
pwexec role add reader
pwexec role grant-permission reader read myapp/config.json
pwexec user grant-role myapp reader
pwexec auth enable

# The mTLS server: the document; the certificate IS the credential.
mtlsexec put myapp/config.json '{"host": "etcd-db", "port": 9202}'

kubectl create secret generic etcd-password --from-literal=password=hunter2

# ── Seed one document per store ────────────────────────────────────────
# Typed JSON through stdin: `kv put k=v` would store "9301" as a STRING.
# The store wraps the KV map under its section key ("db" by default),
# which is why the pod below reads --section db.
kubectl exec -i deploy/vault -- env VAULT_ADDR=http://127.0.0.1:8200 VAULT_TOKEN=e2e-root \
  vault kv put secret/myapp - <<'DOC'
{"host": "vault-db", "port": 9301}
DOC

kubectl exec deploy/redis -- redis-cli set myapp/config.json '{"host": "redis-db", "port": 9302}'

kubectl run nats-seed --image=natsio/nats-box:latest --restart=Never --command -- \
  sh -c "nats -s nats://nats.default.svc:4222 kv add config && nats -s nats://nats.default.svc:4222 kv put config db.json '{\"host\": \"nats-db\", \"port\": 9303}'"
kubectl wait --for=jsonpath='{.status.phase}'=Succeeded pod/nats-seed --timeout=120s

kubectl run minio-seed --image=minio/mc:RELEASE.2024-11-21T17-21-54Z --restart=Never --command -- \
  sh -c "mc alias set e2e http://minio.default.svc:9000 e2e-access e2e-secret-key && mc mb e2e/myapp-config && echo '{\"host\": \"s3-db\", \"port\": 9304}' | mc pipe e2e/myapp-config/prod/db.json"
kubectl wait --for=jsonpath='{.status.phase}'=Succeeded pod/minio-seed --timeout=120s

kubectl create secret generic vault-token --from-literal=token=e2e-root
kubectl create secret generic minio-cred \
  --from-literal=AWS_ACCESS_KEY_ID=e2e-access \
  --from-literal=AWS_SECRET_ACCESS_KEY=e2e-secret-key

diagnose() {
  echo "════ webhook logs:"
  kubectl -n dynamic-config logs deploy/dynamic-config-webhook --tail=20 || true
  for pod in etcd-password-pod etcd-tls-pod vault-pod redis-pod nats-pod s3-pod; do
    echo "════ $pod agent logs:"
    kubectl logs "$pod" -c dynamic-config-agent --tail=20 2>/dev/null || true
  done
  for server in etcd-password etcd-mtls; do
    echo "════ $server server logs:"
    kubectl logs "deploy/$server" --tail=20 2>/dev/null || true
  done
  kubectl get events --sort-by=.lastTimestamp | tail -12
}
trap 'diagnose; kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT

# ── One annotated pod per store ────────────────────────────────────────
pod() {
  local name="$1"; shift
  local annotations="$1"; shift

  kubectl apply -f - <<POD
apiVersion: v1
kind: Pod
metadata:
  name: $name
  annotations:
    dynamic-config.rs/inject: "true"
$annotations
    dynamic-config.rs/path: "/config/rendered.toml"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sh", "-c", "sleep 3600"]
POD
}

pod etcd-password-pod '    dynamic-config.rs/source: "etcd"
    dynamic-config.rs/endpoint: "https://etcd-password.default.svc:2379"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/auth-username: "myapp"
    dynamic-config.rs/password-secret: "etcd-password/password"
    dynamic-config.rs/ca-configmap: "etcd-ca"'

pod etcd-tls-pod '    dynamic-config.rs/source: "etcd"
    dynamic-config.rs/endpoint: "https://etcd-mtls.default.svc:2379"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/tls-secret: "etcd-client-tls"
    dynamic-config.rs/ca-configmap: "etcd-ca"'

pod vault-pod '    dynamic-config.rs/source: "vault"
    dynamic-config.rs/endpoint: "http://vault.default.svc:8200"
    dynamic-config.rs/key: "secret/myapp"
    dynamic-config.rs/section: "db"
    dynamic-config.rs/token-secret: "vault-token/token"'

pod redis-pod '    dynamic-config.rs/source: "redis"
    dynamic-config.rs/endpoint: "redis://redis.default.svc:6379"
    dynamic-config.rs/key: "myapp/config.json"'

pod nats-pod '    dynamic-config.rs/source: "nats"
    dynamic-config.rs/endpoint: "nats://nats.default.svc:4222"
    dynamic-config.rs/key: "config/db.json"'

pod s3-pod '    dynamic-config.rs/source: "s3"
    dynamic-config.rs/endpoint: "myapp-config"
    dynamic-config.rs/key: "prod/db.json"
    dynamic-config.rs/api-url: "http://minio.default.svc:9000"
    dynamic-config.rs/aws-secret: "minio-cred"'

for name in etcd-password-pod:9201 etcd-tls-pod:9202 vault-pod:9301 redis-pod:9302 nats-pod:9303 s3-pod:9304 ; do
  pod="${name%%:*}"; port="${name##*:}"
  kubectl wait --for=condition=Ready "pod/$pod" --timeout=180s
  kubectl exec "$pod" -c app -- cat /config/rendered.toml | grep "port = $port"
  echo "STORE OK: $pod rendered its document"
done

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT
echo "STORES SMOKE OK: etcd both auths, vault, redis, nats and s3 — annotation to file"
