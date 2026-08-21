#!/usr/bin/env bash
# The PR-sized end-to-end: a kind cluster, the images built locally, one
# annotated pod, and the rendered file appearing inside it.
#
# Needs: docker, kind, kubectl, helm. CI's e2e workflow calls exactly
# this; running it locally is the same test.
set -euo pipefail
cd "$(dirname "$0")/.."

cluster=dynamic-config-e2e

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

# The default path is the zero-dependency one: the chart mints its own
# certificate. CERT_MANAGER=1 runs the other mode — the one that renews.
if [[ "${CERT_MANAGER:-0}" == "1" ]]; then
  kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.16.2/cert-manager.yaml
  kubectl -n cert-manager rollout status deploy/cert-manager-webhook --timeout=180s
  kubectl apply -f e2e/issuer.yaml
  extra=(--set webhook.certManager.enabled=true
         --set webhook.certManager.issuerRef.name=selfsigned)
else
  extra=()
fi

# Into its OWN namespace, as production does: the webhook configuration
# excludes the release namespace to keep the webhook from selecting its
# own pods — install into `default` and that exclusion swallows every
# workload beside it.
helm install dynamic-config deploy/helm \
  --namespace dynamic-config --create-namespace \
  --set webhook.image=dynamic-config-webhook --set webhook.tag=dev \
  --set agent.image=dynamic-config-agent --set agent.tag=dev \
  --set webhook.agentEnvAllow="default: RUST_LOG" \
  --set webhook.sourceDeny="default: git" \
  --set-string agent.defaults.perStore.consul='watch-seconds=7\,endpoint=http://consul.default.svc:8500!\,key=myapp/config.json' \
  --set agent.defaults.source="consul" \
  --set agent.defaults.path="/config/rendered.toml" \
  "${extra[@]}"
kubectl -n dynamic-config rollout status deploy/dynamic-config-webhook --timeout=180s

# A consul to render from, and a document in it.
kubectl apply -f e2e/consul.yaml
kubectl rollout status deploy/consul --timeout=180s
kubectl exec deploy/consul -- consul kv put myapp/config.json '{"host": "db.internal", "port": 9000}'

# The pod that asks.
kubectl apply -f e2e/annotated-pod.yaml

# On failure, say WHY before the trap tears the evidence down.
diagnose() {
  echo "════ the pod's containers:"
  kubectl get pod annotated -o jsonpath='{.spec.initContainers[*].name} | {.spec.containers[*].name}'; echo
  echo "════ webhook logs:"
  kubectl -n dynamic-config logs deploy/dynamic-config-webhook --tail=30 || true
  echo "════ recent events:"
  kubectl get events --sort-by=.lastTimestamp | tail -12
}
trap 'diagnose; kind delete cluster --name "$cluster"' EXIT

kubectl wait --for=condition=Ready pod/annotated --timeout=180s

# The claim: the sidecar rendered the file where the annotation said —
# and the app container (uid 1000, per the pod) can read it even at
# 0640, because the run-as annotations made the agent write as 1000.
kubectl exec annotated -c app -- cat /config/rendered.toml | grep 'port = 9000'
kubectl exec annotated -c app -- stat -c '%a %u %g' /config/rendered.toml | grep '640 1000 1000'

# And the posture claims hold on the running pod, not only in a fixture:
# the agent is the restricted-PSS shape and the volume never touches disk.
POD_JSON="$(kubectl get pod annotated -o json)" python3 <<'CHECK'
import json, os
pod = json.loads(os.environ["POD_JSON"])
agent = next(c for c in pod["spec"]["containers"] if c["name"] == "dynamic-config-agent")
sc = agent["securityContext"]
assert sc["runAsNonRoot"] and not sc["allowPrivilegeEscalation"], sc
assert sc["runAsUser"] == 1000 and sc["runAsGroup"] == 1000, sc
assert sc["capabilities"]["drop"] == ["ALL"], sc
volume = next(v for v in pod["spec"]["volumes"] if v["name"] == "dynamic-config")
assert volume["emptyDir"].get("medium") == "Memory", volume
print("POSTURE OK: restricted agent, memory-backed volume")
CHECK

# The agent-env gate, both ways round: an allowlisted name lands as
# environment on the agent; a name outside the list is REFUSED at
# admission, and the refusal names the chart value that opens it.
kubectl get pod annotated -o json \
  | python3 -c 'import json,sys; pod=json.load(sys.stdin); \
agent=[c for c in pod["spec"]["containers"] if c["name"]=="dynamic-config-agent"][0]; \
assert {"name":"RUST_LOG","value":"debug"} in agent.get("env",[]), agent.get("env")'

refusal="$(kubectl apply -f - 2>&1 <<'POD' && echo UNEXPECTED-ADMIT || true
apiVersion: v1
kind: Pod
metadata:
  name: gated
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "consul"
    dynamic-config.rs/endpoint: "http://consul.default.svc:8500"
    dynamic-config.rs/key: "myapp/config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
    dynamic-config.rs/agent-env: "HTTPS_PROXY=http://mitm:3128"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sh", "-c", "sleep 3600"]
POD
)"
echo "$refusal" | grep -q "agentEnvAllow" || { echo "the gate did not hold: $refusal"; exit 1; }
echo "GATE OK: agent-env passes allowlisted names and refuses the rest"

# The source gate: git is turned off in this namespace by the install.
denied="$(kubectl apply -f - 2>&1 <<'POD' && echo UNEXPECTED-ADMIT || true
apiVersion: v1
kind: Pod
metadata:
  name: denied-store
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "git"
    dynamic-config.rs/endpoint: "https://github.com/example/config.git"
    dynamic-config.rs/key: "config.json"
    dynamic-config.rs/path: "/config/rendered.toml"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sh", "-c", "sleep 3600"]
POD
)"
echo "$denied" | grep -q "sourceDeny" || { echo "the source gate did not hold: $denied"; exit 1; }
echo "SOURCE GATE OK: a denied store never reaches scheduling"

# The per-store default tier: the pod names no watch-seconds, the
# install says consul watches every 7 — the sidecar's args carry it.
kubectl get pod annotated -o json \
  | python3 -c 'import json,sys; pod=json.load(sys.stdin); \
agent=[c for c in pod["spec"]["containers"] if c["name"]=="dynamic-config-agent"][0]; \
args=agent["args"]; assert args[args.index("--watch")+1]=="7", args'
echo "STORE DEFAULTS OK: the consul tier set the watch interval"

# The whole-installation tier: a pod that says NOTHING but "inject me"
# gets source, endpoint, key and path from the installation.
kubectl apply -f - <<'POD'
apiVersion: v1
kind: Pod
metadata:
  name: minimal
  annotations:
    dynamic-config.rs/inject: "true"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sh", "-c", "sleep 3600"]
POD
kubectl wait --for=condition=Ready pod/minimal --timeout=180s
kubectl exec minimal -c app -- cat /config/rendered.toml | grep 'port = 9000'
echo "MINIMAL OK: inject-only pod rendered from installation defaults"

# And the pinned endpoint refuses a pod that names another address.
pinned="$(kubectl apply -f - 2>&1 <<'POD' && echo UNEXPECTED-ADMIT || true
apiVersion: v1
kind: Pod
metadata:
  name: pinned-endpoint
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/endpoint: "http://rogue.default.svc:8500"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sh", "-c", "sleep 3600"]
POD
)"
echo "$pinned" | grep -q "pins it" || { echo "the pin did not hold: $pinned"; exit 1; }
echo "PIN OK: a pinned endpoint refuses a differing annotation"

trap 'kind delete cluster --name "$cluster"; rm -f "$KUBECONFIG"' EXIT
echo "SMOKE OK: the annotation became a file inside the pod"
