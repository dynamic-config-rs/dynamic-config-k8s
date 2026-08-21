---
name: review-for-release
description: Use before cutting a k8s release, or when asked to review the whole repository — the checks that have actually caught things here, in the order that finds problems fastest.
---

# Reviewing before a release

Run `just check` first, then `just e2e-smoke` if docker is there. Everything
below is what those do not catch.

## The checks that have caught real bugs here

**Read the golden diff, do not just regenerate it.** `cargo test regenerate
-- --ignored` rewrites the injected-pod fixture to agree with whatever the
code now does. A golden regenerated without being read is a test that has
stopped testing. The `status: injected` annotation and the `status.reason`
slugs both landed this way and both needed the diff read.

**Ask what a *stopped* watch does, not just a running one.** A loop that
only notices its token between frames takes as long as its idle deadline to
let go — fifteen seconds on a keep-alive, fifty on a half-open connection.
The book says a quarter second.

**Ask when the first file is written.** A watch delivers a *change*; the
current value is not delivered at startup, by contract. Anything that
renders only on delivery leaves a pod whose app opens a file that is not
there. The kind smoke's `MINIMAL OK` line is the one that catches it.

**Every refusal must be a refusal.** Silently not injecting is how a pod
starts without the configuration it declared. Grep the admission path for
an `Ok(())` that should have been an error, and for a message that names
the value it refused — no log line here carries a configuration value.

**Three copies of every CRD, one source.** `deploy/crds.json`,
`deploy/helm/crds/`, `deploy/kustomize/base/crds/` — all generated from
`dynamic-config-operator/src/crds.rs`. `just crds` is the drift gate; a
hand-edit passes review and fails the gate.

**Render both deployment paths.** `helm template dc deploy/helm` and
`kubectl kustomize deploy/kustomize/overlays/with-operator`. A values
change that renders on one side and not the other is a user's first
five minutes.

**Chase the version through every file that names it.** Chart version,
appVersion, three `artifacthub.io/images` tags, kustomize image tags,
kustomize README `?ref=` pins, the README cosign example, the book's
`--version`. A release that moves the chart and not the images ships a
chart pointing at images that do not exist.

**`SECURITY.md`'s supported-versions table.** A security job fails the
build when its top row is not the line being released. It has drifted
before.

## What a release needs beyond green

- Each crate's own `CHANGELOG.md` under `## [Unreleased]`, not only the
  root's: a user reading one crate's history should see what it gained.
- The examples' count in `book/src/sources.md` and `examples/README.md`
  if a manifest was added.
- `just images` builds three images from one cargo-chef recipe; the
  release builds them once and shares them. A Dockerfile change that only
  works because a layer was cached locally is a red release.
