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
| Images are distroless, non-root, signed, with SBOMs | the release workflow; verify with `cosign verify` against either registry |
| No `unsafe` anywhere in the three binaries | `#![forbid(unsafe_code)]` + the Security workflow's check |

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

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x images/chart | ✅ the latest |
| — | nothing older exists |

Security fixes land on the **latest release** and nothing is
backported before 1.0: when a release ships, every prior release is
end-of-life the same day.

## Standing rule

Every open Dependabot alert is triaged before a release ships, and the
three security ledgers (deny.toml, the workflow's allow-ghsas, the OSV
config where present) never carry an entry without a reason and an
expiry condition.
