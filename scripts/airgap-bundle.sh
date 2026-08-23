#!/usr/bin/env bash
# Everything needed to install this on a cluster with no internet, in one
# tarball — and everything needed to *verify* it once it is there.
#
# The supply-chain work this repository already does is worth nothing in an
# air-gapped environment if the signatures and attestations are left behind
# with the registry. So the bundle carries them, and `verify.sh` inside it
# checks them against the images as they landed rather than against the
# ones that were published.
#
# Needs: helm, crane (or skopeo), cosign, tar. All on the connected side.
#
#   ./scripts/airgap-bundle.sh 0.3.0
#   # move dynamic-config-0.3.0-airgap.tar.gz across
#   tar xzf dynamic-config-0.3.0-airgap.tar.gz && cd dynamic-config-0.3.0-airgap
#   ./load.sh registry.internal:5000
#   ./verify.sh
#   helm install dynamic-config ./chart --values values-airgap.yaml
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:?usage: airgap-bundle.sh <version>, e.g. 0.3.0}"
registry="${SOURCE_REGISTRY:-ghcr.io/dynamic-config-rs}"
out="dynamic-config-${version}-airgap"

for tool in helm crane cosign; do
  command -v "$tool" >/dev/null || { echo "$tool is not installed" >&2; exit 1; }
done

rm -rf "$out" && mkdir -p "$out/images"

echo "════ the chart"
helm pull "oci://${registry}/charts/dynamic-config" --version "$version" \
  --untar --untardir "$out"
mv "$out/dynamic-config" "$out/chart"

# Helm never upgrades `crds/`, so an air-gapped operator needs them as their
# own files — the same reason the upgrade leg applies them by hand.
cp -r deploy/crds "$out/crds"

echo "════ the images, by digest"
: > "$out/images.txt"

for component in agent webhook operator; do
  image="${registry}/dynamic-config-${component}:v${version}"
  digest=$(crane digest "$image")

  echo "${component} ${image} ${digest}" >> "$out/images.txt"

  # The index, not one architecture: whoever loads this may run either.
  crane pull --format oci "${registry}/dynamic-config-${component}@${digest}" \
    "$out/images/${component}"

  # The signature and the attestations travel as their own tags, which is
  # how cosign finds them offline too.
  tag="${digest/:/-}"

  crane pull --format oci "${registry}/dynamic-config-${component}:${tag}.sig" \
    "$out/images/${component}.sig" 2>/dev/null || echo "  (no signature tag for ${component})"
  crane pull --format oci "${registry}/dynamic-config-${component}:${tag}.att" \
    "$out/images/${component}.att" 2>/dev/null || echo "  (no attestation tag for ${component})"
done

echo "════ the loader"
cat > "$out/load.sh" <<'LOAD'
#!/usr/bin/env bash
# Pushes the bundled images into an internal registry, keeping their
# digests — which is what lets the signatures still verify afterwards.
set -euo pipefail
cd "$(dirname "$0")"

target="${1:?usage: load.sh <registry>, e.g. registry.internal:5000}"

while read -r component image digest; do
  crane push --index "images/${component}" "${target}/dynamic-config-${component}:${digest#sha256:}" 2>/dev/null \
    || crane copy --format oci "images/${component}" "${target}/dynamic-config-${component}@${digest}"

  echo "${component} → ${target}/dynamic-config-${component}@${digest}"
done < images.txt

echo
echo "Set these in values-airgap.yaml, by digest — a tag in an air-gapped"
echo "registry is a mutable name nobody is watching."
LOAD

cat > "$out/verify.sh" <<'VERIFY'
#!/usr/bin/env bash
# The signature and the attestations, checked against what actually landed.
#
# `--offline` and a bundled trust root: nothing here reaches Fulcio or
# Rekor, which is the whole point of an air-gapped verification.
set -euo pipefail
cd "$(dirname "$0")"

identity="${IDENTITY_REGEXP:-github.com/dynamic-config-rs/dynamic-config-k8s}"

while read -r component image digest; do
  echo "════ ${component} ${digest}"

  cosign verify --offline \
    --certificate-identity-regexp "$identity" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --local-image "images/${component}" \
    || echo "  UNVERIFIED — do not install this"
done < images.txt

echo
echo "The SBOM and the provenance are attached to the PER-ARCHITECTURE"
echo "images rather than to the index; see SECURITY.md for which digest"
echo "carries what, and use cosign verify-attestation against those."
VERIFY

chmod +x "$out/load.sh" "$out/verify.sh"

echo "════ values"
cat > "$out/values-airgap.yaml" <<'VALUES'
# Point every image at the internal registry, BY DIGEST. A tag inside an
# air-gapped registry is a mutable name nobody is watching, and the digests
# are what the bundled signatures cover.
#
# `load.sh` prints the four lines to paste here.
webhook:
  image: registry.internal:5000/dynamic-config-webhook
  tag: ""            # leave empty and use digest instead
  digest: ""         # sha256:...
agent:
  image: registry.internal:5000/dynamic-config-agent
  tag: ""
  digest: ""
operator:
  enabled: false
  image: registry.internal:5000/dynamic-config-operator
  tag: ""
  digest: ""
VALUES

cat > "$out/README.md" <<READ
# dynamic-config ${version}, offline

    ./load.sh registry.internal:5000    # push the images, keeping digests
    ./verify.sh                         # signatures, without reaching Rekor
    kubectl apply --server-side -f crds/
    helm install dynamic-config ./chart --values values-airgap.yaml

CRDs are applied by hand because Helm installs \`crds/\` once and never
upgrades it — the same step an upgrade needs on a connected cluster.

\`images.txt\` is the manifest: component, source image, digest.
READ

echo "════ packing"
tar czf "${out}.tar.gz" "$out"
rm -rf "$out"

echo
echo "${out}.tar.gz"
echo "  $(du -h "${out}.tar.gz" | cut -f1)"
