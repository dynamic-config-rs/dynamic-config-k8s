# Security

## Reporting

Use the repository's private advisory form
(<https://github.com/dynamic-config-rs/dynamic-config-k8s/security/advisories/new>).
First response within days, not weeks. Please do not open a public
issue for anything that could be a vulnerability.

## Scope

This repository ships **images and a chart**, not crates. What it is
responsible for:

| Property | Where it is enforced |
|---|---|
| The injected agent carries the restricted-PSS posture and cannot relax a pod | golden files + the e2e posture assertion |
| Secret material never rides annotations or arguments | the annotation contract; refusals are admission errors |
| The webhook holds no API credential and serves only TLS | `automountServiceAccountToken: false`; the TLS server refuses plaintext |
| Unknown `dynamic-config.rs/*` annotations fail the admission | contract tests — a typo cannot silently downgrade a pod's auth |
| Images are distroless, non-root, signed, with SBOMs | the release workflow; `cosign verify` against either registry — and see *Which digest carries what* below |
| No `unsafe` anywhere in any binary here | `#![forbid(unsafe_code)]` + the Security workflow's check |

### Which digest carries what

A tag resolves to a **manifest list**, and the list is what `cosign` signs:
`cosign verify ghcr.io/dynamic-config-rs/dynamic-config-agent:v0.3.0` is the
command, against either registry, and both registries carry the same index
digest.

The SBOM and the provenance are attached to the **per-architecture images
underneath it**, because that is what they describe — one is a build of one
architecture, and an attestation on the list would be claiming something
about bytes it does not cover. To verify one, resolve the architecture
first:

```sh
digest=$(docker buildx imagetools inspect \
  ghcr.io/dynamic-config-rs/dynamic-config-agent:v0.3.0 \
  --format '{{range .Manifest.Manifests}}{{if eq .Platform.Architecture "amd64"}}{{.Digest}}{{end}}{{end}}')

cosign verify-attestation --type spdxjson \
  "ghcr.io/dynamic-config-rs/dynamic-config-agent@${digest}" …
```

This split arrived with 0.3.0, when the arm64 half stopped being built
under emulation: each architecture is built on its own runner and pushed by
digest, and the list is assembled from the two.

The engine and store crates inside the images carry the engine
repository's guarantees; see its SECURITY.md and the book's
[Compatibility Contract](https://dynamic-config-rs.github.io/compatibility.html).

## What is not a vulnerability here

- Anything reachable only by editing the pod spec directly — pod
  writers are trusted by Kubernetes' own model; this webhook adds to
  pods, it does not police them.
- A store that serves hostile configuration: the agent renders what
  the store returns; validating content is the application's schema's
  job, and last-known-good bounds the blast radius.
- `failurePolicy: Ignore` letting a pod start un-injected during a
  webhook outage — that is the documented trade, flip to `Fail` per
  the book's security page.

## What falls with what

[`THREAT_MODEL.md`](THREAT_MODEL.md) answers the other half of the question
this file starts: the assets, the trust boundaries, and what an attacker
reaches from a compromised application container, agent, webhook, operator
or network path. The confused-deputy risk in a cluster-scoped
`DynamicConfigClass` is named there rather than left implicit.

## Supported versions

| Version | Supported |
|---|---|
| 0.3.x images/chart | ✅ the latest |
| ≤ 0.2 | — end of life |

Security fixes land on the **latest release** and nothing is
backported before 1.0: when a release ships, every prior release is
end-of-life the same day.

## Standing rule

Every open Dependabot alert is triaged before a release ships, and the
three security ledgers (deny.toml, the workflow's allow-ghsas, the OSV
config where present) never carry an entry without a reason and an
expiry condition.
