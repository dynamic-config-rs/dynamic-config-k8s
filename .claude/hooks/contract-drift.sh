#!/usr/bin/env bash
# Names the files a change has to travel to, the moment it is made.
#
# Half of what ships from this repository is not code: an annotation is a
# contract, a CRD is three generated copies, a chart value is a schema and a
# README. Each of those has been forgotten here at least once, and each is
# cheap to name at the moment somebody is still holding the change.
#
# Advisory by design: it exits 0 and never blocks a tool call.
set -euo pipefail

input=$(cat)
path=$(printf '%s' "$input" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input", {}).get("file_path", ""))' 2>/dev/null || true)

[ -z "$path" ] && exit 0

case "$path" in
  */dynamic-config-webhook/src/annotations.rs)
    cat <<'NOTE'
The annotation contract moved — which is this repository's API, so the
change is breaking unless it is purely additive. What has to follow:
  · book/src/annotations.md            the table AND the refusal wording
  · dynamic-config-webhook/tests/golden.rs
        `cargo test regenerate -- --ignored` in the SAME commit
  · deploy/helm/values.yaml + values.schema.json
        if the knob has an installation default
  · examples/README.md                 if it wants a manifest of its own
  · CHANGELOG.md under Unreleased      naming the old spelling
NOTE
    ;;
  */dynamic-config-webhook/src/patch.rs|*/dynamic-config-webhook/src/installation_file.rs)
    cat <<'NOTE'
The injected pod's shape moved. The golden file is the contract:
  · cargo test -p dynamic-config-webhook regenerate -- --ignored
  · read the diff before committing it — a golden regenerated without
    being read is a test that agrees with whatever it was given.
NOTE
    ;;
  */dynamic-config-agent/src/*)
    cat <<'NOTE'
The agent moved. What a reader of the book expects to still be true:
  · book/src/observability.md   the metric names and the resync table
  · book/src/sources.md         if a store's capability or interval changed
  · e2e/smoke.sh                the contract a pod depends on — a render
    that now happens later than it did is exactly what it catches
NOTE
    ;;
  */dynamic-config-operator/src/crds.rs)
    cat <<'NOTE'
The CRD types moved. The manifests are GENERATED — never edit the JSON:
  · just crds-write   regenerates deploy/crds.json and both copies
  · just crds         the drift gate `just check` runs
  · book/src/operator.md if a field's meaning changed
NOTE
    ;;
  */deploy/helm/values.yaml)
    cat <<'NOTE'
A chart value moved. Three files describe the same thing here:
  · deploy/helm/values.schema.json   or `helm lint` passes a typo through
  · deploy/helm/README.md            the values reference a user reads
  · deploy/kustomize/                if the knob exists on that side too
Render both before believing it: `helm template dc deploy/helm` and
`kubectl kustomize deploy/kustomize/overlays/with-operator`.
NOTE
    ;;
  */deploy/crds.json|*/deploy/helm/crds/*|*/deploy/kustomize/base/crds/*)
    cat <<'NOTE'
This file is generated. Edit `dynamic-config-operator/src/crds.rs` and run
`just crds-write`; `just check` fails on a hand-edited copy.
NOTE
    ;;
  */Chart.yaml)
    cat <<'NOTE'
The chart's own version moved. Everything that names it:
  · appVersion AND the three artifacthub.io/images tags
  · deploy/kustomize/{base,overlays}/*/deployment.yaml image tags
  · deploy/kustomize/README.md ?ref= pins
  · README.md's cosign example, book/src/install.md's --version
NOTE
    ;;
esac

exit 0
