---
name: change-an-annotation
description: Use when adding, renaming or changing the meaning of a dynamic-config.rs/* annotation — the contract is this repository's API, and the change has to travel.
---

# Changing the annotation contract

`dynamic-config.rs/*` is the API of this repository. A pod's author writes
it, a cluster's operator pins it, and neither can see the code. So the rule
is stricter than semver's: **additive is a minor, anything else is
breaking**, and both land with the same commit that regenerates the golden
file.

## The order that works

1. **`dynamic-config-webhook/src/annotations.rs`** — the parse and the
   refusal. A wrong value fails the admission with a sentence a pod's
   author reads; it never carries the value it refused.
2. **The refusal's *reason*, not its English.** Refusal kinds are a
   `status.reason` slug written at the refusal site (`POLICY`, `PINNED`,
   `MALFORMED`, `CONFLICT`). Nothing downstream may sniff the message —
   that was tried, and none of the three real messages matched the
   patterns somebody wrote for them.
3. **`cargo test -p dynamic-config-webhook regenerate -- --ignored`**, then
   **read the diff**. It is the injected pod, byte for byte.
4. **`book/src/annotations.md`** — the table, and the sentence that says
   what a refusal looks like.
5. **Installation defaults**, if the knob has one: `deploy/helm/values.yaml`,
   `values.schema.json`, and the kustomize `installation.yaml` ConfigMap.
   Both spellings — the structured document and the env-var grammar —
   go through one parser, so a knob added to one side and not the other is
   a knob half the deployments cannot set.
6. **`examples/`** if it deserves a manifest, and `examples/README.md`'s
   table plus the count in `book/src/sources.md`.
7. **`CHANGELOG.md`** under `Unreleased`, naming the old spelling if
   something moved.

## What reviewers here have caught

- An annotation parsed but never plumbed to the agent's argv.
- A default that the webhook applied and the schema refused.
- A pin that refused a *matching* value, because the comparison was on the
  raw string rather than the parsed one.
- A golden regenerated in a later commit than the change — green on both
  sides, and the fixture describing neither.
