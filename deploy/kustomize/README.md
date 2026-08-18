# Deploying without helm

The same resources the chart renders, as a kustomize base plus two TLS
overlays. The base alone is NOT deployable — an admission webhook must
serve TLS the API server trusts, and the trust is the overlay's job.

```text
base/                   namespace, SA, deployment, service, PDB,
                        webhook configuration (no caBundle), CRDs
overlays/cert-manager/  Certificate + cainjector annotation — TLS that renews
overlays/own-cert/      bring-your-own PEMs via secretGenerator
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
