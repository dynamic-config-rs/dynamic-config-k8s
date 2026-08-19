# Deploying without helm

The same resources the chart renders, as a kustomize base plus two TLS
overlays. The base alone is NOT deployable — an admission webhook must
serve TLS the API server trusts, and the trust is the overlay's job.

```text
base/                    namespace, SA, deployment, service, PDB,
                         webhook configuration (no caBundle), CRDs ×3
overlays/cert-manager/   Certificate + cainjector annotation — TLS that renews
overlays/own-cert/       bring-your-own PEMs via secretGenerator
overlays/selfrotate/     the webhook mints and rotates its own pair —
                         env + the name-scoped RBAC + the empty Secret vessel
overlays/with-operator/  the Render → ConfigMap/Secret reconciler + its RBAC;
                         composes with any TLS overlay
```

Composing (a Flux Kustomization or an Argo Application points here):

```yaml
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - github.com/dynamic-config-rs/dynamic-config-k8s//deploy/kustomize/overlays/selfrotate?ref=v0.1.1
  - github.com/dynamic-config-rs/dynamic-config-k8s//deploy/kustomize/overlays/with-operator?ref=v0.1.1
```

## cert-manager

```sh
# once: put your issuer's name into overlays/cert-manager/certificate.yaml
kubectl apply -k deploy/kustomize/overlays/cert-manager
```

## Bring your own certificate

1. Mint a pair whose SAN is the in-cluster name:

   ```sh
   openssl req -x509 -newkey ed25519 -nodes -days 3650 \
     -keyout tls.key -out tls.crt \
     -subj "/CN=dynamic-config-webhook.dynamic-config.svc" \
     -addext "subjectAltName=DNS:dynamic-config-webhook.dynamic-config.svc"
   cp tls.crt ca.crt      # self-signed: the cert is its own CA
   ```

2. Drop `tls.crt`, `tls.key`, `ca.crt` into `overlays/own-cert/` and:

   ```sh
   kubectl apply -k deploy/kustomize/overlays/own-cert
   ```

3. Hand the CA to the webhook configuration (kustomize cannot read a
   file into a patch):

   ```sh
   kubectl patch mutatingwebhookconfiguration dynamic-config --type=json \
     -p "[{\"op\":\"add\",\"path\":\"/webhooks/0/clientConfig/caBundle\",\"value\":\"$(base64 -w0 ca.crt)\"}]"
   ```

## What the chart gives that this cannot

Chart-minted TLS with Secret reuse, fleet-wide agent defaults as
values, namespace gating, NetworkPolicy, the operator toggle — the
[chart README](../helm/README.md) is the full surface. This path
exists for shops whose policy is plain manifests; it stays deliberately
small.

## selfRotate

```sh
kubectl apply -k deploy/kustomize/overlays/selfrotate
```

No PEMs, no cert-manager: the webhook mints a CA and leaf in memory,
fills the (initially empty) `dynamic-config-webhook-tls` Secret, patches
its own `caBundle` with a two-CA transition window, and rotates every
24h behind a Lease. The overlay is exactly the helm mode's price sheet:
a service-account token and three name-scoped grants, spelled out in
`rbac.yaml`.

## The operator

```sh
kubectl apply -k deploy/kustomize/overlays/with-operator   # plus a TLS overlay
```

Adds the `DynamicConfigRender` reconciler and its least-privilege RBAC —
including Secret writes, because a Render's `secret:` target is a Secret
the operator owns. GitOps note: under ArgoCD prefer the selfrotate or
cert-manager overlay over own-cert, for the same render-stability
reasons the book's [GitOps page](https://dynamic-config-rs.github.io/k8s/gitops.html) walks.
