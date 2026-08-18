# Firestore

A Google Cloud document read as configuration. The endpoint is not a
url: it is `<project>` or `<project>/<database>`, and the key is
`collection/document` (nest deeper as
`environments/prod/config/db`).

On GKE the right method is the first one, and it involves no secret at
all.

## Metadata-server (Workload Identity)

The workload's own identity, from the metadata server — reachable from
GKE, Cloud Run, GCE, and nowhere else, which is the security property
that makes it the default. The agent asks for a token, gets a
short-lived one, and renews it as it approaches expiry.

`auth` can be omitted entirely — metadata-server is what the agent does
for firestore when nothing else is asked:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: billing
  annotations:
    dynamic-config.rs/inject: "true"
    dynamic-config.rs/source: "firestore"
    dynamic-config.rs/endpoint: "acme-prod"
    dynamic-config.rs/key: "config/billing"
    dynamic-config.rs/path: "/config/rendered.json"
spec:
  serviceAccountName: billing
  containers:
    - name: app
      image: myapp:1
```

GKE side, the Workload Identity pairing (the [GKE
docs](https://cloud.google.com/kubernetes-engine/docs/how-to/workload-identity)
own the full ceremony):

```sh
gcloud iam service-accounts create billing-reader
gcloud projects add-iam-policy-binding acme-prod \
  --member serviceAccount:billing-reader@acme-prod.iam.gserviceaccount.com \
  --role roles/datastore.viewer

gcloud iam service-accounts add-iam-policy-binding \
  billing-reader@acme-prod.iam.gserviceaccount.com \
  --role roles/iam.workloadIdentityUser \
  --member "serviceAccount:acme-prod.svc.id.goog[default/billing]"

kubectl annotate serviceaccount billing \
  iam.gke.io/gcp-service-account=billing-reader@acme-prod.iam.gserviceaccount.com
```

## Access token

A token somebody already obtained — `gcloud auth print-access-token`
produces one. It expires within the hour and the agent cannot renew it,
so this is a debugging method, not a deployment method; the honest use
is a one-shot init container in a test cluster:

```yaml
    dynamic-config.rs/mode: "init"
    dynamic-config.rs/auth: "access-token"
    dynamic-config.rs/token-secret: "gcp-token/token"
```

## Emulator

The Firestore emulator wants no credential and a different endpoint;
`api-url` points the API somewhere other than Google's:

```yaml
    dynamic-config.rs/auth: "emulator"
    dynamic-config.rs/api-url: "http://firestore-emulator.test.svc:8080"
    dynamic-config.rs/endpoint: "demo-project"
```

## The named database

The second database in a project is the endpoint's second segment:

```yaml
    dynamic-config.rs/endpoint: "acme-prod/eu-config"
```

## When it fails

| symptom | look at | usual cause |
|---|---|---|
| `403 PERMISSION_DENIED` | `gcloud projects get-iam-policy acme-prod` | the GSA lacks `roles/datastore.viewer`, or the WI binding names the wrong namespace/KSA pair |
| metadata server unreachable | pod events, GKE node pool | Workload Identity not enabled on the pool |
| `404` | `gcloud firestore databases list` | the document path or the named database is wrong |
| works locally, fails in-cluster | — | local `gcloud` credentials are not the pod's; the pod has only the metadata server |
