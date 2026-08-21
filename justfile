# Everything CI runs, in the order that fails fastest.

default: check

check: fmt lint test crds

fmt:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

# The CRD manifests in deploy/ are generated from the operator's types;
# this proves they have not drifted.
crds:
    cargo run -p dynamic-config-operator -- --crds > /tmp/crds-now.json
    diff -u deploy/crds.json /tmp/crds-now.json
    # The split copies (each file one VALID json doc — which is also
    # valid YAML) feed helm's crds/ dir and the kustomize base. Drift
    # in either is drift.
    python3 scripts/split-crds.py /tmp/crds-now.json /tmp/crds-split
    diff -ur deploy/crds /tmp/crds-split
    diff -ur deploy/helm/crds /tmp/crds-split
    diff -ur deploy/kustomize/base/crds /tmp/crds-split

# Regenerate after a deliberate CRD change.
crds-write:
    cargo run -p dynamic-config-operator -- --crds > deploy/crds.json
    python3 scripts/split-crds.py deploy/crds.json deploy/crds
    python3 scripts/split-crds.py deploy/crds.json deploy/helm/crds
    python3 scripts/split-crds.py deploy/crds.json deploy/kustomize/base/crds

# One file, one dependency build, three targets out of it. The three
# images share the layer that holds the gRPC stack, the AWS SDK and
# rustls, which is where the minutes were going.
#
# **Blocked until the engine's 0.9 is published.** While the `[patch]`
# table in Cargo.toml points at sibling working trees, those paths are
# outside the build context and `cargo chef` cannot resolve them. Delete
# the patch table with the 0.9 pin and this works again — which is the
# same step the release already has to take.
images:
    docker build -f docker/Dockerfile --target agent -t dynamic-config-agent:dev .
    docker build -f docker/Dockerfile --target webhook -t dynamic-config-webhook:dev .
    docker build -f docker/Dockerfile --target operator -t dynamic-config-operator:dev .

# The three images as one tarball, for a CI run that builds once and
# hands the result to every e2e leg instead of rebuilding per leg.
images-save out="images.tar": images
    docker save -o {{out}} \
      dynamic-config-agent:dev dynamic-config-webhook:dev dynamic-config-operator:dev

# The kind end-to-end smoke: needs docker + kind + kubectl.
e2e-smoke:
    e2e/smoke.sh
