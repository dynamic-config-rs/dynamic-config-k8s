//! The mutating admission webhook: an AdmissionReview in, a JSONPatch
//! out, and the patch injects the agent into annotated pods.
//!
//! The patch generation is a pure function, golden-file tested with
//! recorded AdmissionReview fixtures — the server here is only
//! transport. TLS is terminated in-process (`tls.rs`): the API server
//! speaks HTTPS to webhooks and nothing else, so a webhook without TLS
//! is a webhook that is never called. The chart mounts the pair at
//! `/tls` from cert-manager or from its own generated Secret; both look
//! the same from here.

#![forbid(unsafe_code)]

mod selfrotate;
mod tls;

use std::sync::atomic::{AtomicU64, Ordering};

use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;
use tracing::info;

/// One counter per outcome. `skipped` is a pod that did not ask;
/// `patched` asked and got the agent; `refused` asked wrongly.
static SKIPPED: AtomicU64 = AtomicU64::new(0);
static PATCHED: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);

async fn mutate(Json(review): Json<Value>) -> Json<Value> {
    let response = dynamic_config_webhook::admission_response(&review);

    // The audit line: who, what, and which way it went — and no values,
    // because annotation values include store addresses and role names
    // that belong in the cluster, not in every log aggregator.
    let text = |pointer: &str| {
        review
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let namespace = text("/request/namespace");
    let name = review
        .pointer("/request/object/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or_else(|| text("/request/object/metadata/generateName"));
    let source = text("/request/object/metadata/annotations/dynamic-config.rs~1source");

    let outcome = if response.pointer("/response/patch").is_some() {
        PATCHED.fetch_add(1, Ordering::Relaxed);
        "patched"
    } else if response.pointer("/response/allowed") == Some(&Value::Bool(false)) {
        REFUSED.fetch_add(1, Ordering::Relaxed);
        "refused"
    } else {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
        "skipped"
    };

    if outcome != "skipped" {
        info!(namespace, name, source, outcome, "admission");
    }

    Json(response)
}

async fn healthz() -> &'static str {
    "ok"
}

/// Prometheus text format, hand-rolled: three counters do not earn a
/// metrics crate. Scrape over the same TLS the API server uses.
async fn metrics() -> String {
    format!(
        "# TYPE dynamic_config_admissions_total counter\n\
         dynamic_config_admissions_total{{outcome=\"skipped\"}} {}\n\
         dynamic_config_admissions_total{{outcome=\"patched\"}} {}\n\
         dynamic_config_admissions_total{{outcome=\"refused\"}} {}\n",
        SKIPPED.load(Ordering::Relaxed),
        PATCHED.load(Ordering::Relaxed),
        REFUSED.load(Ordering::Relaxed),
    )
}

/// SIGTERM is how Kubernetes asks; ctrl-c is how a terminal does.
async fn shutdown_signal() {
    let interrupt = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler installs");

        tokio::select! {
            _ = interrupt => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    interrupt.await.ok();

    info!("shutting down");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // The serving path names its provider per-config, but the selfRotate
    // mode's kube client uses the process default — which, with ring and
    // aws-lc both reachable, must be stated or rustls panics mid-handshake.
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| "a rustls CryptoProvider was already installed")?;

    let app = Router::new()
        .route("/mutate", post(mutate))
        .route("/healthz", axum::routing::get(healthz))
        .route("/metrics", axum::routing::get(metrics));

    let address =
        std::env::var("DYNAMIC_CONFIG_WEBHOOK_ADDR").unwrap_or_else(|_| "0.0.0.0:8443".to_owned());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    let material = tls::Material::from_environment();

    // The selfRotate mode: the rotation loop runs beside the server and
    // fills the mounted Secret; serving below waits for the kubelet to
    // deliver the first pair.
    if let Some(settings) = selfrotate::Settings::from_environment() {
        info!("selfRotate: this webhook mints and rotates its own certificate");
        tokio::spawn(selfrotate::run(settings));

        let patience = std::time::Duration::from_secs(5);

        // `loadable`, not `present`: the placeholder Secret mounts empty
        // files, and serving must not start (and fail, and take the
        // rotation task down with the process) until a real pair landed.
        while !material.loadable() {
            info!("waiting for the first minted pair to arrive at /tls");
            tokio::time::sleep(patience).await;
        }
    }

    if material.present() {
        info!(%address, "webhook listening");
        tls::serve(listener, app, material, shutdown_signal()).await?;
    } else if std::env::var("DYNAMIC_CONFIG_WEBHOOK_PLAINTEXT").as_deref() == Ok("1") {
        // For harnesses that terminate TLS in front or call the pure
        // function over localhost. Never a production shape, hence the
        // name it must be asked for by.
        tracing::warn!("PLAINTEXT mode: no API server will call this");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
    } else {
        return Err(format!(
            "no TLS material at {} / {}: an admission webhook cannot serve \
             plain HTTP (set DYNAMIC_CONFIG_WEBHOOK_PLAINTEXT=1 only for a \
             local harness)",
            material.certificate.display(),
            material.key.display()
        )
        .into());
    }

    Ok(())
}
