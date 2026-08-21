# Git

Configuration that lives where its reviews live. Git has nothing to push,
so the agent asks — but it asks the *cheap* question: the ref's
advertisement is one handshake per tick, and a transfer happens only when
the ref actually moved, so a 15-second watch does not hammer the host.

The endpoint is anything git understands (`https://…`, `ssh://…`,
`git@host:org/repo.git`); the key is the file's path inside the
repository; the ref defaults to branch `main`.

## Anonymous

A public repository over HTTPS — no auth annotation:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: billing
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "git"
    dynamic-config.rs/endpoint: "https://github.com/acme/config.git"
    dynamic-config.rs/key: "billing/prod.yaml"
    dynamic-config.rs/path: "/config/rendered.yaml"
    dynamic-config.rs/ref: "main"
spec:
  containers:
    - name: app
      image: myapp:1
```

## A token over HTTPS

How every host takes a token: HTTP basic auth with the token in the
password half. GitHub PATs and App installation tokens, GitLab deploy
and project tokens, Azure DevOps PATs — all the same shape. The
username half is filler that these hosts ignore; the agent sends
`x-access-token`, the value GitHub documents:

```sh
kubectl create secret generic config-repo-token --from-literal=token=ghp_…
```

```yaml
    dynamic-config.rs/auth: "token"
    dynamic-config.rs/token-secret: "config-repo-token/token"
```

For the rare host that does read the username, name it:

```yaml
    dynamic-config.rs/auth-username: "deploy"
```

A GitLab *deploy token* is the least-privilege pick on that platform:
scope `read_repository`, one repository, its own expiry.

## An SSH deploy key

One `kubernetes.io/ssh-auth` Secret; its conventional key name is
`ssh-privatekey`, and the webhook mounts it `0400` because ssh refuses
group-readable keys:

```sh
ssh-keygen -t ed25519 -f deploy_key -N ""
# register deploy_key.pub as a read-only deploy key on the host
kubectl create secret generic config-deploy-key \
  --type=kubernetes.io/ssh-auth --from-file=ssh-privatekey=deploy_key
```

```yaml
    dynamic-config.rs/endpoint: "git@github.com:acme/config.git"
    dynamic-config.rs/ssh-secret: "config-deploy-key"
```

`auth: ssh-key` is implied by `ssh-secret` when no auth is named. The
key is offered with `IdentitiesOnly=yes`, so an agent holding other
keys cannot exhaust the server's auth tries before the right one.

**And the permission caveat, out loud:** the kubelet writes secret
files as root, `0400` means owner-read only, and the agent runs
nonroot — so the mounted key is readable only when the pod sets a
`securityContext.fsGroup` (the kubelet then group-owns the files) and
the custom image's ssh client tolerates a group-readable key, or when
the pod runs the agent's uid. This combination is honest-but-untested:
the e2e suite covers HTTPS and token auth; ssh in-pod is documented,
not gated.

**The stock image caveat, out loud:** git-over-SSH is carried by the
`ssh` *program*, exactly as git itself does it — and the distroless
agent image does not contain one. HTTPS works from the stock image;
SSH needs an image with an ssh client:

```dockerfile
FROM ghcr.io/dynamic-config-rs/dynamic-config-agent:0.1.0 AS agent
FROM alpine:3.20
RUN apk add --no-cache openssh-client ca-certificates \
 && printf 'github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl\n' \
    >> /etc/ssh/ssh_known_hosts
COPY --from=agent /dynamic-config-agent /dynamic-config-agent
ENTRYPOINT ["/dynamic-config-agent"]
```

…and the chart's `agent.image` value points at it. The `known_hosts`
line is the second half ssh insists on; pin your own host's key, not a
copy of this one.

## Branch, tag, commit

`ref` takes four spellings:

```yaml
    dynamic-config.rs/ref: "main"           # a branch, plainly
    dynamic-config.rs/ref: "branch:release" # the same, spelled out
    dynamic-config.rs/ref: "tag:v1.4"       # a tag
    dynamic-config.rs/ref: "commit:8f3a…"   # one exact tree, forever
```

A tag or commit still ticks the watch loop, and still never transfers —
useful with `mode: init` for a pinned, reproducible render.

## A self-hosted host with a private CA

The same annotation as every other store:

```yaml
    dynamic-config.rs/endpoint: "https://git.internal.acme/config.git"
    dynamic-config.rs/ca-configmap: "internal-ca"
```

## When it fails

| symptom | look at | usual cause |
|---|---|---|
| auth failed over HTTPS | try the token in a `git ls-remote` by hand | token expired, or lacks read scope on the repo |
| `Host key verification failed` | the image's `/etc/ssh/ssh_known_hosts` | the custom image pinned no host key for this host |
| ssh: command not found | — | the stock distroless image; see the caveat above |
| file not found | `git ls-tree <ref> -- <path>` | the path is spelled from the repository root, and the ref matters |
