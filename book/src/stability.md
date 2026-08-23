# Stability & Versioning

Experimental, stated plainly: this is the youngest repository in the
organisation and the annotation contract is v1 — and since 0.3.0 that
contract is a **registry** rather than a list somebody keeps in step: every
key is one row carrying whether it takes a `.name` suffix and whether it has
been retired, a test checks this book against it, and a retired key will be
admitted with a warning naming its replacement rather than refused. Nothing
is retired yet. The operator's
reconcilers shipped in 0.1.1 and it stays 0.x until they have soak
history, whatever the rest of the family does.

- The **annotation contract is the API**; a breaking change to it bumps
  the minor and regenerates the golden file in the same commit. It grew
  additively in 0.2.0: `dynamic-config.rs/status`, which the webhook
  writes on a pod it has patched so that a second admission does not
  patch it again.
- **The installation is one contract with three spellings** — chart
  values, a mounted YAML document, or environment variables. A map is
  rendered to the same grammar the variables carry and goes through the
  same parser, so adding a spelling is not adding a semantics.
- The three components version together; images are the artefacts.
- The agent's store list grew additively (etcd, nats and s3 landed in 0.1.1) and is complete at nine; a tenth would follow the same rule.
- The engine dependency is a caret: an engine patch reaches the images
  on rebuild.

The repository's [ROADMAP](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/ROADMAP.md)
carries the ladder in full — the async stores and etcd's
two-methods-forever answer, the self-rotating webhook TLS mode and the
one narrow RBAC it will cost, the operator's reconcilers.
