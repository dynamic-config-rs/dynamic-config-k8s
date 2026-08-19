//! Serving the router over TLS, because there is no other way to be an
//! admission webhook: the API server speaks HTTPS to `clientConfig`
//! endpoints and nothing else.
//!
//! The accept loop is this crate's own for the same reasons the config
//! server's is (its `serve.rs` carries the full argument): a failed
//! handshake must cost one connection and not the listener, a socket
//! somebody squats on must have a deadline, and the provider must be
//! `ring` — already in the workspace graph — rather than a vendored
//! AWS-LC.
//!
//! One thing the config server does not need and this server does:
//! **certificate reload**. cert-manager renews the mounted Secret well
//! before expiry, and a webhook that only reads its certificate at
//! startup serves a stale one until the next rollout. The files are
//! polled for a changed modification time and the rustls configuration
//! swapped in place; connections in flight keep the config they
//! handshook with.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use arc_swap::ArcSwap;
use axum::Router;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

/// How long a client has to complete a TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a connection has, after the handshake, to send headers.
const HEADER_TIMEOUT: Duration = Duration::from_secs(15);

/// How long shutdown waits for in-flight admissions before dropping them.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the accept loop waits after an accept error. Out of file
/// descriptors fails again immediately; retrying instantly turns a
/// resource limit into a spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(10);

/// How often the certificate files are checked for renewal.
const RELOAD_INTERVAL: Duration = Duration::from_secs(60);

pub struct Material {
    pub certificate: PathBuf,
    pub key: PathBuf,
}

impl Material {
    /// Where the chart mounts the pair, unless the environment moved it.
    pub fn from_environment() -> Self {
        let path = |name: &str, fallback: &str| {
            PathBuf::from(std::env::var(name).unwrap_or_else(|_| fallback.to_owned()))
        };

        Self {
            certificate: path("DYNAMIC_CONFIG_WEBHOOK_TLS_CERT", "/tls/tls.crt"),
            key: path("DYNAMIC_CONFIG_WEBHOOK_TLS_KEY", "/tls/tls.key"),
        }
    }

    pub fn present(&self) -> bool {
        self.certificate.exists() && self.key.exists()
    }

    /// Whether the pair on disk actually parses into a server config —
    /// the selfRotate placeholder Secret mounts EMPTY files, so
    /// existence alone says nothing while the first mint is in flight.
    pub fn loadable(&self) -> bool {
        self.load().is_ok()
    }

    fn load(&self) -> Result<ServerConfig, Box<dyn std::error::Error + Send + Sync>> {
        let chain: Vec<CertificateDer<'static>> =
            CertificateDer::pem_file_iter(&self.certificate)?.collect::<Result<_, _>>()?;
        let key = PrivateKeyDer::from_pem_file(&self.key)?;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(chain, key)?;

        config.alpn_protocols = vec![b"http/1.1".to_vec()];

        Ok(config)
    }

    fn modified(&self) -> Option<(SystemTime, SystemTime)> {
        let stamp = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();

        Some((stamp(&self.certificate)?, stamp(&self.key)?))
    }
}

/// Serves `router` over TLS until `shutdown` completes, reloading the
/// certificate when cert-manager (or a chart upgrade) renews it.
pub async fn serve<S>(
    listener: TcpListener,
    router: Router,
    material: Material,
    shutdown: S,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: std::future::Future<Output = ()> + Send,
{
    let current = Arc::new(ArcSwap::from_pointee(material.load()?));
    let mut seen = material.modified();

    let (closing, closed) = watch::channel(false);
    let mut connections = JoinSet::new();
    let mut reload = tokio::time::interval(RELOAD_INTERVAL);
    reload.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tokio::pin!(shutdown);

    info!(certificate = %material.certificate.display(), "serving TLS");

    loop {
        tokio::select! {
            () = &mut shutdown => break,
            _ = reload.tick() => {
                let now = material.modified();

                if now.is_some() && now != seen {
                    match material.load() {
                        Ok(fresh) => {
                            current.store(Arc::new(fresh));
                            seen = now;
                            info!("certificate reloaded");
                        }
                        // Renewal writes the pair one file at a time;
                        // a half-written pair stays on the old one and
                        // the next tick sees the whole renewal.
                        Err(error) => warn!(%error, "certificate reload failed; serving the previous one"),
                    }
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        warn!(%error, "accept failed");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                        continue;
                    }
                };

                let acceptor = TlsAcceptor::from(current.load_full());
                let service = TowerToHyperService::new(router.clone());
                let mut closed = closed.clone();

                connections.spawn(async move {
                    let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream));

                    let Ok(Ok(stream)) = handshake.await else {
                        // Port scanners and health checks that do not
                        // speak TLS are normal traffic; one line, no peer.
                        return;
                    };

                    let mut builder = http1::Builder::new();
                    builder
                        .timer(TokioTimer::new())
                        .header_read_timeout(HEADER_TIMEOUT);

                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    tokio::pin!(connection);

                    tokio::select! {
                        _ = connection.as_mut() => {}
                        _ = closed.changed() => {
                            connection.as_mut().graceful_shutdown();
                            connection.await.ok();
                        }
                    }
                });
            }
        }
    }

    // Stop accepting, tell every open connection to finish its request,
    // and bound the wait — a rollout must not hang on a slow client.
    drop(listener);
    closing.send(true).ok();

    if tokio::time::timeout(DRAIN_TIMEOUT, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        warn!("drain timed out; dropping remaining connections");
    }

    Ok(())
}
