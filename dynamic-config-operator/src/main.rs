//! The operator: two CRDs over the same agent machinery.
//!
//! - `DynamicConfigClass` names a store + auth bundle once, so pod
//!   annotations shrink to a class reference.
//! - `DynamicConfigRender` reconciles a store document into a ConfigMap
//!   — the mode for workloads that cannot take a sidecar.
//!
//! Deliberately THIN, per the recorded tripwire: two CRDs, a
//! reconcile-to-ConfigMap loop through the agent's own machinery, and
//! nothing more.

#![forbid(unsafe_code)]

mod crds;

use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // `--crds` prints the CRD manifests and exits: what `deploy/` embeds,
    // generated from the same types the reconciler compiles against, so
    // the two cannot drift.
    if std::env::args().nth(1).as_deref() == Some("--crds") {
        print!("{}", crds::manifests()?);
        return Ok(());
    }

    // One provider, stated: with ring and aws-lc both reachable in the
    // dependency graph, rustls refuses to pick — and panics inside the
    // first TLS handshake instead of here, where the message can say
    // what to do.
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| "a rustls CryptoProvider was already installed")?;

    info!("operator starting: Render → ConfigMap/Secret reconciler, Class watch wired");

    // The metrics contract's operator slice, opt-out by unsetting.
    let metrics = std::env::var("DYNAMIC_CONFIG_OPERATOR_METRICS_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:9090".to_owned());

    if !metrics.is_empty() {
        tokio::spawn(dynamic_config_agent::metrics::serve(
            metrics,
            "dynamic_config_operator",
        ));
    }

    let client = kube::Client::try_default().await?;
    crds::run(client).await
}
