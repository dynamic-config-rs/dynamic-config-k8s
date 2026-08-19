# Stability & Versioning

Experimental, stated plainly: this is the youngest repository in the
organisation, the annotation contract is v1, and the operator's
reconcilers have not shipped. It stays 0.x until the operator has soak
history, whatever the rest of the family does.

- The **annotation contract is the API**; a breaking change to it bumps
  the minor and regenerates the golden file in the same commit.
- The three components version together; images are the artefacts.
- The agent's store list grew additively (etcd, nats and s3 landed in 0.2.0) and is complete at nine; a tenth would follow the same rule.
- The engine dependency is a caret: an engine patch reaches the images
  on rebuild.

The repository's [ROADMAP](https://github.com/dynamic-config-rs/dynamic-config-k8s/blob/main/ROADMAP.md)
carries the ladder in full — the async stores and etcd's
two-methods-forever answer, the self-rotating webhook TLS mode and the
one narrow RBAC it will cost, the operator's reconcilers.
