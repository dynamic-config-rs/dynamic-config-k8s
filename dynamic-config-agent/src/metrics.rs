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

/// Documents the store's own watch delivered, and rounds the resync ran.
///
/// Told apart because they answer different questions: deliveries near
/// zero beside resyncs climbing is a stream that is not delivering, which
/// is the failure a resync exists to cover and the one nothing else here
/// would show.
pub static DELIVERIES_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static RESYNCS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Whether a watch is open, and how many times one has been reopened.
pub static WATCH_CONNECTED: AtomicU64 = AtomicU64::new(0);
pub static WATCH_RECONNECTS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// When the last *successful* read of the store happened, delivered or
/// resynced. What the staleness gauge is measured from.
pub static LAST_SUCCESS_TIMESTAMP: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// A document arrived from the store's own watch.
pub fn delivered() {
    DELIVERIES_TOTAL.fetch_add(1, Ordering::Relaxed);
    LAST_SUCCESS_TIMESTAMP.store(now(), Ordering::Relaxed);
}

/// The periodic read that covers a stream going silent.
pub fn resynced() {
    RESYNCS_TOTAL.fetch_add(1, Ordering::Relaxed);
    LAST_SUCCESS_TIMESTAMP.store(now(), Ordering::Relaxed);
}

/// A watch was opened.
pub fn watch_up() {
    WATCH_CONNECTED.store(1, Ordering::Relaxed);
}

/// A watch ended, and is about to be opened again.
pub fn watch_down() {
    WATCH_CONNECTED.store(0, Ordering::Relaxed);
    WATCH_RECONNECTS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

pub fn rendered() {
    RENDERS_TOTAL.fetch_add(1, Ordering::Relaxed);
    LAST_RENDER_TIMESTAMP.store(now(), Ordering::Relaxed);
}

pub fn failed() {
    RENDER_FAILURES_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Reconciles, operator-side, and how long they took.
///
/// A histogram by hand, because a handful of buckets do not earn a
/// dependency any more than the counters did: Prometheus's text format
/// wants cumulative buckets, a sum and a count, and that is three
/// primitives and an array.
pub static RECONCILES_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static RECONCILE_FAILURES_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static RECONCILE_SECONDS_SUM_MILLIS: AtomicU64 = AtomicU64::new(0);

/// The bucket boundaries, in milliseconds. A reconcile is a store fetch
/// and an API write, so the interesting range is tens of milliseconds to
/// tens of seconds — a slow store, a throttled API server.
const BUCKETS_MILLIS: [u64; 8] = [10, 50, 100, 500, 1_000, 5_000, 15_000, 60_000];

static BUCKETS: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// One reconcile that worked, and what it cost.
pub fn reconciled(elapsed: std::time::Duration) {
    let millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);

    RECONCILES_TOTAL.fetch_add(1, Ordering::Relaxed);
    RECONCILE_SECONDS_SUM_MILLIS.fetch_add(millis, Ordering::Relaxed);

    for (bucket, ceiling) in BUCKETS.iter().zip(BUCKETS_MILLIS) {
        if millis <= ceiling {
            bucket.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// One reconcile that did not.
pub fn reconcile_failed() {
    RECONCILE_FAILURES_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// `base`, moved by up to a quarter of itself, so a fleet does not come
/// back to a store in step.
///
/// Deterministic given the seed and drawn from the clock, exactly as the
/// engine's own `Pace` does it — this is the same policy applied to a
/// requeue, which is a wait the engine never sees.
#[must_use]
pub fn spread(base: std::time::Duration) -> std::time::Duration {
    static ENTROPY: AtomicU64 = AtomicU64::new(0);

    let previous = ENTROPY.load(Ordering::Relaxed);
    let next = if previous == 0 { now() } else { previous }
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);

    ENTROPY.store(next, Ordering::Relaxed);

    let quarter = base / 4;
    let offset = quarter
        .checked_mul(u32::try_from(next >> 33 & 0xFF).unwrap_or(0))
        .unwrap_or(quarter)
        / 255;

    if next & 1 == 0 {
        base.saturating_add(offset)
    } else {
        base.saturating_sub(offset)
    }
}

/// The reconcile histogram, in the text format's own shape.
fn histogram(prefix: &str) -> String {
    let mut lines = format!("# TYPE {prefix}_reconcile_duration_seconds histogram\n");
    let mut cumulative = 0;

    for (bucket, ceiling) in BUCKETS.iter().zip(BUCKETS_MILLIS) {
        // Each bucket counts every observation at or under its ceiling, so
        // they are cumulative already. The `max` is against a scrape that
        // lands between two of the increments a single observation makes:
        // buckets that went backwards would be refused by Prometheus, and
        // a sample one behind is better than a sample rejected.
        cumulative = cumulative.max(bucket.load(Ordering::Relaxed));

        let seconds = ceiling as f64 / 1000.0;

        lines.push_str(&format!(
            "{prefix}_reconcile_duration_seconds_bucket{{le=\"{seconds}\"}} {cumulative}\n"
        ));
    }

    let count = RECONCILES_TOTAL.load(Ordering::Relaxed);
    let sum = RECONCILE_SECONDS_SUM_MILLIS.load(Ordering::Relaxed) as f64 / 1000.0;

    lines.push_str(&format!(
        "{prefix}_reconcile_duration_seconds_bucket{{le=\"+Inf\"}} {count}\n\
         {prefix}_reconcile_duration_seconds_sum {sum}\n\
         {prefix}_reconcile_duration_seconds_count {count}\n\
         # TYPE {prefix}_reconcile_failures_total counter\n\
         {prefix}_reconcile_failures_total {}\n",
        RECONCILE_FAILURES_TOTAL.load(Ordering::Relaxed)
    ));

    lines
}

fn exposition(prefix: &str) -> String {
    // Seconds since the store was last read successfully — **the number an
    // alert fires on.** Everything else here says what happened; this says
    // how long ago it stopped happening, which is the question a pager
    // asks. Zero until the first success, so a pod that has never read the
    // store does not look fresh.
    let last = LAST_SUCCESS_TIMESTAMP.load(Ordering::Relaxed);
    let staleness = if last == 0 {
        0
    } else {
        now().saturating_sub(last)
    };

    format!(
        "# TYPE {prefix}_renders_total counter\n\
         {prefix}_renders_total {}\n\
         # TYPE {prefix}_render_failures_total counter\n\
         {prefix}_render_failures_total {}\n\
         # TYPE {prefix}_last_render_timestamp_seconds gauge\n\
         {prefix}_last_render_timestamp_seconds {}\n\
         # TYPE {prefix}_deliveries_total counter\n\
         {prefix}_deliveries_total {}\n\
         # TYPE {prefix}_resyncs_total counter\n\
         {prefix}_resyncs_total {}\n\
         # TYPE {prefix}_watch_connected gauge\n\
         {prefix}_watch_connected {}\n\
         # TYPE {prefix}_watch_reconnects_total counter\n\
         {prefix}_watch_reconnects_total {}\n\
         # TYPE {prefix}_staleness_seconds gauge\n\
         {prefix}_staleness_seconds {staleness}\n{}",
        RENDERS_TOTAL.load(Ordering::Relaxed),
        RENDER_FAILURES_TOTAL.load(Ordering::Relaxed),
        LAST_RENDER_TIMESTAMP.load(Ordering::Relaxed),
        DELIVERIES_TOTAL.load(Ordering::Relaxed),
        RESYNCS_TOTAL.load(Ordering::Relaxed),
        WATCH_CONNECTED.load(Ordering::Relaxed),
        WATCH_RECONNECTS_TOTAL.load(Ordering::Relaxed),
        histogram(prefix),
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

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let mut scratch = [0u8; 1024];
            let read = stream.read(&mut scratch).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&scratch[..read]);

            // Two probe paths beside the scrape, because a process with an
            // HTTP port and no liveness endpoint makes a deployment write
            // an `exec` probe that shells out — and a probe that costs a
            // process is a probe nobody sets a tight period on.
            let body = if request.starts_with("GET /healthz") || request.starts_with("GET /readyz")
            {
                "ok\n".to_owned()
            } else {
                exposition(prefix)
            };

            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );

            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape a scraper refuses if it is wrong: buckets that never
    /// decrease, a `+Inf` that matches the count, and a sum in seconds.
    #[test]
    fn the_histogram_is_a_histogram() {
        reconciled(std::time::Duration::from_millis(5));
        reconciled(std::time::Duration::from_millis(750));
        reconciled(std::time::Duration::from_secs(30));

        let text = histogram("test");
        let counts: Vec<u64> = text
            .lines()
            .filter(|line| line.contains("_bucket{le="))
            .filter_map(|line| line.rsplit(' ').next()?.parse().ok())
            .collect();

        assert_eq!(counts.len(), 9, "eight buckets and +Inf: {text}");
        assert!(
            counts.windows(2).all(|pair| pair[0] <= pair[1]),
            "a bucket went backwards: {text}"
        );
        assert!(
            text.contains("test_reconcile_duration_seconds_count 3"),
            "{text}"
        );
        assert!(
            counts[0] >= 1 && counts[8] == 3,
            "the fast one lands in the first bucket and all three in +Inf: {text}"
        );
    }

    /// A wait that is spread stays near what it was asked for, and does
    /// not come back the same number every time.
    #[test]
    fn a_spread_wait_is_close_and_not_constant() {
        let base = std::time::Duration::from_secs(100);
        let waits: Vec<std::time::Duration> = (0..16).map(|_| spread(base)).collect();

        for wait in &waits {
            assert!(
                *wait >= std::time::Duration::from_secs(75)
                    && *wait <= std::time::Duration::from_secs(125),
                "{wait:?}"
            );
        }

        assert!(
            waits.iter().any(|wait| *wait != waits[0]),
            "sixteen identical waits is a fleet in lockstep"
        );
    }
}
