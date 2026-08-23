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
mod election;

use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Structured logs as before, plus OTLP traces when a collector is
    // configured. Nothing is exported unless `OTEL_EXPORTER_OTLP_ENDPOINT`
    // is set, so no existing deployment changes behaviour.
    let telemetry = dynamic_config_telemetry::install("dynamic-config-operator");

    // `--crds` prints the CRD manifests and exits: what `deploy/` embeds,
    // generated from the same types the reconciler compiles against, so
    // the two cannot drift.
    if std::env::args().nth(1).as_deref() == Some("--crds") {
        print!("{}", crds::manifests()?);
        telemetry.shutdown();

        return Ok(());
    }

    let outcome = reconcile_forever().await;

    // Explicitly, before the process goes: a batch exporter holds spans for
    // up to its scheduled delay, and a controller that has just lost its
    // lease has the spans somebody will want.
    telemetry.shutdown();

    outcome
}

/// The operator proper, so `main` can own the telemetry either side of it.
async fn reconcile_forever() -> Result<(), Box<dyn std::error::Error>> {
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
            // Up is ready, here. An operator with no `Render` resources has
            // rendered nothing and is working perfectly; keying its
            // readiness on a render would leave a healthy install
            // permanently unready.
            dynamic_config_agent::metrics::always,
        ));
    }

    let client = kube::Client::try_default().await?;

    // One replica reconciles; the rest wait for its term to end. Two
    // writing the same ConfigMap is harmless on a good day and two
    // different documents landing in an arbitrary order on a bad one.
    //
    // The namespace and the identity come from the downward API, which the
    // chart wires. Without them there is nothing to contend over, so a
    // single replica runs unelected — which is what a `kubectl run` of this
    // image is, and refusing that would make the image untestable.
    let (namespace, identity) = (
        std::env::var("POD_NAMESPACE").ok(),
        std::env::var("POD_NAME").ok(),
    );

    let Some((namespace, identity)) = namespace.zip(identity) else {
        tracing::warn!(
            "POD_NAMESPACE and POD_NAME are not set, so there is no leader election; \
             run one replica"
        );

        return crds::run(client).await;
    };

    let lost = election::lead(&client, &namespace, &identity).await?;

    tokio::select! {
        result = crds::run(client) => result,
        () = lost => {
            // Ending the process rather than pausing: a controller that
            // has been running holds caches and in-flight work, and the
            // simplest correct answer to "somebody else leads now" is to
            // let the pod restart and contend again from nothing.
            Err("the leader lease was lost".into())
        }
    }
}
