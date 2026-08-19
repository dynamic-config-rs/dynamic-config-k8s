//! A Prometheus text endpoint with no metrics crate: a handful of
//! counters do not earn a dependency, and the format is one content
//! type and some lines. Shared by the agent and (through the library)
//! the operator.

use std::sync::atomic::{AtomicU64, Ordering};

/// Installs and refusals, agent-side; reconciles, operator-side — the
/// caller picks which of these it moves.
pub static RENDERS_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static RENDER_FAILURES_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static LAST_RENDER_TIMESTAMP: AtomicU64 = AtomicU64::new(0);

pub fn rendered() {
    RENDERS_TOTAL.fetch_add(1, Ordering::Relaxed);
    LAST_RENDER_TIMESTAMP.store(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        Ordering::Relaxed,
    );
}

pub fn failed() {
    RENDER_FAILURES_TOTAL.fetch_add(1, Ordering::Relaxed);
}

fn exposition(prefix: &str) -> String {
    format!(
        "# TYPE {prefix}_renders_total counter\n\
         {prefix}_renders_total {}\n\
         # TYPE {prefix}_render_failures_total counter\n\
         {prefix}_render_failures_total {}\n\
         # TYPE {prefix}_last_render_timestamp_seconds gauge\n\
         {prefix}_last_render_timestamp_seconds {}\n",
        RENDERS_TOTAL.load(Ordering::Relaxed),
        RENDER_FAILURES_TOTAL.load(Ordering::Relaxed),
        LAST_RENDER_TIMESTAMP.load(Ordering::Relaxed),
    )
}

/// Serves the exposition on every connection — GET is the only verb a
/// scraper sends, and answering it unconditionally keeps this at forty
/// lines instead of an HTTP framework.
pub async fn serve(address: String, prefix: &'static str) {
    let listener = match tokio::net::TcpListener::bind(&address).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, %address, "metrics endpoint could not bind");
            return;
        }
    };

    tracing::info!(%address, "metrics endpoint listening");

    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            continue;
        };

        let body = exposition(prefix);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            // Read whatever request line arrived (and ignore it), answer,
            // close. A scraper needs nothing more.
            let mut scratch = [0u8; 1024];
            let _ = stream.read(&mut scratch).await;
            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}
