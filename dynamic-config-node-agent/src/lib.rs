//! One agent per node, delivering configuration as a volume.
//!
//! Every other shape in this repository puts an agent **beside** the
//! application: one container per render, one fetch per pod. That is the
//! right default — the credential is scoped to the workload that needs it,
//! and a compromised agent reaches one application's configuration.
//!
//! It is also 25,000 containers at 10,000 pods and 2.5 renders each, and
//! that is the number this exists for.
//!
//! # Why this is one component and not two
//!
//! "A node-level agent" and "a CSI driver" were two entries on the same
//! list, and building them separately would have been building a thing and
//! its only delivery mechanism as though they were unrelated.
//!
//! A DaemonSet that fetches for a whole node has to get bytes into a pod,
//! and there are two ways: a `hostPath` the pod also mounts — which
//! restricted Pod Security forbids, for the reason it forbids it — or a
//! **CSI volume**, which is the interface Kubernetes added for exactly this
//! and which the kubelet already knows how to mount, unmount and clean up
//! after an eviction.
//!
//! So this is a CSI node plugin whose backing store is the same engine the
//! sidecar runs.
//!
//! # What it shares, and what it does not
//!
//! Two pods on one node that want the same document from the same store
//! share **one fetch and one watch**. That is the whole saving, and it is
//! the reason a node with a hundred pods reading one Consul key opens one
//! connection to Consul rather than a hundred.
//!
//! What they do not share is the rendered file: each pod gets its own,
//! written into the directory the kubelet gave it, with that pod's own
//! format and mode.
//!
//! # What this costs, said plainly
//!
//! **Credentials for many workloads in one process.** The sidecar's
//! isolation is the property this trades away: a compromised node agent
//! reaches every store credential every pod on that node uses. That is why
//! the sidecar stays the default and this is the escape hatch for a scale
//! that makes 25,000 containers untenable — not a better shape, a different
//! trade.

pub mod csi;
pub mod publish;
pub mod shared;

/// The CSI wire types, generated from the vendored spec.
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod proto {
    tonic::include_proto!("csi.v1");
}

/// The name the kubelet knows this driver by.
///
/// It appears in three places that must agree: here, the `CSIDriver`
/// object's name, and the registration socket's path. A mismatch is a
/// driver the kubelet registers and never calls.
pub const DRIVER: &str = "config.dynamic-config.rs";

/// The node's own numbers, in the format everything else here answers.
///
/// Two series, and they are the two that say whether this component is
/// earning its keep: on a node where `documents` equals `readers`, nothing
/// is being shared and a sidecar would have cost the same.
pub async fn serve_metrics(address: String, registry: shared::Registry) {
    let listener = match tokio::net::TcpListener::bind(&address).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, %address, "the metrics endpoint could not bind");
            return;
        }
    };

    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            continue;
        };

        let (documents, readers) = registry.counts();

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;

            let body = format!(
                "# TYPE dynamic_config_node_agent_documents gauge\n\
                 dynamic_config_node_agent_documents {documents}\n\
                 # TYPE dynamic_config_node_agent_readers gauge\n\
                 dynamic_config_node_agent_readers {readers}\n"
            );

            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );

            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}
