//! The node plugin: a gRPC server on a Unix socket the kubelet dials.
//!
//! Two sockets are involved and it is worth being clear which is which.
//! **This one** is the CSI socket, under the kubelet's plugins directory,
//! and it is where volumes are published. The *other* one — the
//! registration socket — belongs to the `node-driver-registrar` sidecar in
//! the DaemonSet, which is the upstream component whose whole job is to
//! tell the kubelet this driver exists. Reimplementing that here would be
//! reimplementing a thing Kubernetes ships.

use std::path::Path;

use dynamic_config_node_agent::csi::Plugin;
use dynamic_config_node_agent::proto::{identity_server::IdentityServer, node_server::NodeServer};
use tracing::info;

/// Where the kubelet expects to find this driver's socket.
///
/// Under the driver's own directory, which the DaemonSet mounts from the
/// host — the kubelet and this process have to agree on the path, and the
/// `CSIDriver` object's name is what makes them.
const SOCKET: &str = "/csi/csi.sock";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _telemetry = dynamic_config_telemetry::install("dynamic-config-node-agent");

    // The node this pod is on, from the downward API. Without it the
    // kubelet's `NodeGetInfo` gets an empty id and the driver is registered
    // against nothing.
    let node = std::env::var("KUBE_NODE_NAME")
        .map_err(|_| "KUBE_NODE_NAME is not set; the DaemonSet sets it from spec.nodeName")?;

    let socket = Path::new(SOCKET);

    // A socket left behind by a previous container — an eviction, an OOM,
    // a rollout — makes `bind` fail with "address in use" on a path nobody
    // is listening to. Removing it is what every CSI driver does and what
    // makes a restart a restart rather than a permanent failure.
    if socket.exists() {
        std::fs::remove_file(socket)?;
    }

    if let Some(directory) = socket.parent() {
        std::fs::create_dir_all(directory)?;
    }

    let plugin = Plugin::new(node.clone());

    if let Ok(address) = std::env::var("DYNAMIC_CONFIG_METRICS_ADDR") {
        let registry = plugin.registry().clone();

        tokio::spawn(async move {
            dynamic_config_node_agent::serve_metrics(address, registry).await;
        });
    }

    let listener = tokio::net::UnixListener::bind(socket)?;

    info!(%node, socket = SOCKET, "the node plugin is listening");

    tonic::transport::Server::builder()
        .add_service(IdentityServer::new(plugin.clone()))
        .add_service(NodeServer::new(plugin))
        .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
        .await?;

    Ok(())
}
