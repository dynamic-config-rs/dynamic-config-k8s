//! The operator: two CRDs over the same agent machinery.
//!
//! - `DynamicConfigClass` names a store + auth bundle once, so pod
//!   annotations shrink to a class reference.
//! - `DynamicConfigRender` reconciles a store document into a ConfigMap
//!   — the mode for workloads that cannot take a sidecar.
//!
//! 6c of the staged plan: this binary is the *scaffold* — CRD types,
//! their generated schemas, and the reconcile loop's shape — and the
//! book's Operator chapter tracks what is wired against what is planned.

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

    info!("operator starting (reconcilers land in 0.3.0 — see the book)");

    let client = kube::Client::try_default().await?;
    crds::run(client).await
}
