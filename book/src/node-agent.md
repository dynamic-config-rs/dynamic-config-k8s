# One Agent per Node

Every other shape here puts an agent **beside** the application: one
container per render, one fetch per pod. That is the right default — the
credential is scoped to the workload that needs it, and a compromised agent
reaches one application's configuration.

It is also 25,000 containers at 10,000 pods and 2.5 renders each, and that
number is what this exists for.

```yaml
volumes:
  - name: config
    csi:
      driver: config.dynamic-config.rs
      readOnly: true
      volumeAttributes:
        source: consul
        endpoint: http://consul.default.svc:8500
        key: myapp/config.json
        path: rendered.toml        # a name inside the volume
containers:
  - name: app
    volumeMounts:
      - name: config
        mountPath: /config
```

No annotations, no injected container, no webhook involved at all. The
kubelet asks the node's agent for the volume, and the agent renders into it.

## Why this is one component and not two

"A node-level agent" and "a CSI driver" were two entries on the same list,
and building them separately would have been building a thing and its only
delivery mechanism as though they were unrelated.

A DaemonSet that fetches for a whole node has to get bytes into a pod, and
there are two ways: a `hostPath` the pod also mounts — which restricted Pod
Security forbids, for the reason it forbids it — or a **CSI volume**, which
is the interface Kubernetes added for exactly this and which the kubelet
already knows how to mount, unmount and clean up after an eviction.

So this is a CSI node plugin whose backing store is the same engine the
sidecar runs. Not a second implementation of fetch-resolve-render-watch: the
sidecar's own crate, called as a library.

## What it shares

Two pods on one node that want the same document from the same store under
the same credential share **one fetch and one watch**. A node running a
hundred pods that read one Consul key opens one connection to Consul.

The credential is part of that identity, not metadata beside it. Two pods
reading one key under different tokens are two reads, and sharing them would
hand one namespace's document to another under a credential it was never
granted.

What they do not share is the rendered file: each pod gets its own bytes at
its own path, in its own format, with its own mode. A `.properties` reader
and a YAML reader on one node share the fetch and share nothing else.

Two series say whether any of it is working:

```text
dynamic_config_node_agent_documents 12
dynamic_config_node_agent_readers   96
```

On a node where those two numbers are equal, nothing is being shared and a
sidecar would have cost the same.

## One property a sidecar cannot offer

**The first render happens before the pod starts.** The kubelet does not
start a pod's containers until every volume is published, and publishing is
what does the first fetch — so an application cannot observe a missing file,
and there is no init container here because there is nothing for one to do.

## What it costs

Said plainly, because it is the reason this is off by default and the
sidecar is not.

- **Credentials for many workloads in one process.** A compromised node
  agent reaches every store credential every pod on that node uses. The
  sidecar's isolation is exactly what is traded away.
- **It runs as root.** The kubelet creates a pod's volume directories owned
  by root, and a plugin that cannot write into them cannot publish. Every
  other binary here runs nonroot; this one cannot.
- **It mounts host paths.** The kubelet's plugin and pod directories, which
  is what a CSI driver is.

Not a better shape. A different trade, for a scale that makes the first one
untenable — and a decision to make with a measurement rather than a
preference.

## Installing it

```sh
helm upgrade dynamic-config … --set nodeAgent.enabled=true
```

`nodeAgent.kubeletPath` is `/var/lib/kubelet` everywhere except a few
distributions — k0s and some managed offerings move it, and a wrong value is
a driver the kubelet registers and never calls.

The DaemonSet carries upstream's `node-driver-registrar` beside the agent.
Its whole job is telling the kubelet this driver exists; writing that here
would be reimplementing a thing Kubernetes ships.

## The attributes a volume takes

The same vocabulary as the annotations, for the same reason the agent's
flags are: one contract, learned once, whichever shape delivers it.

| attribute | |
|---|---|
| `source` | required — `consul`, `vault`, `etcd`, … |
| `key` | required — the document's key, path or object |
| `path` | required — a **name inside the volume**; the extension picks the format |
| `endpoint` | the store's address |
| `section`, `auth`, `auth-mount`, `auth-role`, `auth-username`, `namespace`, `ref`, `api-url`, `file-mode`, `watch-seconds` | as the annotations of those names |

`path` is checked rather than trusted: absolute paths and `..` are refused,
because the kubelet owns the directory and a volume that could write outside
it could write into every other pod's volume on the node.

There is no `mode`: a CSI volume is published before the containers start
and stays. There is no `env-inject`: there is no command here to wrap. And
there is no `metrics-port`: the metrics are the node agent's, one endpoint
for the whole node.
