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

/// Leases extended.
pub static LEASE_RENEWALS_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Renewals the store refused or could not answer.
pub static LEASE_RENEWAL_FAILURES_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Leases handed back on the way out.
pub static LEASE_REVOCATIONS_TOTAL: AtomicU64 = AtomicU64::new(0);
/// How long the held lease was last granted for, in seconds. Zero when the
/// store issues none.
pub static LEASE_TTL_SECONDS: AtomicU64 = AtomicU64::new(0);

/// Times the store answered that the document is not there.
///
/// Its own counter, because "the store said it is gone" and "the store did
/// not answer" are different facts and an alert on one is not an alert on
/// the other. A document that vanished used to be indistinguishable from a
/// store that was down.
pub static ABSENT_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Post-render notifications that were answered, and that were not.
pub static NOTIFICATIONS_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Notifications that failed — refused, unanswered, or past the deadline.
///
/// Never fatal: the file is already published when this moves, so a failure
/// here is a consumer that has not been told rather than a configuration
/// that did not arrive.
pub static NOTIFICATION_FAILURES_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Whether the rendered files are currently something other than what was
/// rendered.
///
/// `1` while a file on disk differs from what this agent last wrote, back
/// to `0` when it is repaired or re-rendered.
pub static DRIFT: AtomicU64 = AtomicU64::new(0);
/// How many times drift has been noticed.
pub static DRIFT_TOTAL: AtomicU64 = AtomicU64::new(0);

/// The fingerprint of the document this agent last published.
///
/// Half of the only end-to-end answer there is. `renders_total` says a
/// document reached disk; it says nothing about whether the application
/// read it, and an application that is still running the previous one is
/// the outage this metric exists to make visible.
pub static RENDERED_FINGERPRINT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// The fingerprint the application said it is actually running.
///
/// Written by `POST /applied`. Empty until something acknowledges, which is
/// most deployments — an application that never acknowledges is not
/// penalised, it simply leaves this empty and `applied` at zero.
pub static APPLIED_FINGERPRINT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// When the application last acknowledged, in seconds since the epoch.
pub static APPLIED_TIMESTAMP: AtomicU64 = AtomicU64::new(0);
/// How many acknowledgements have arrived.
pub static ACKS_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Acknowledgements naming a document this agent never published.
///
/// Not an error on its own: a restarted application acknowledges what it
/// read before the restart, and a slow one acknowledges a generation the
/// store has already replaced. A count that keeps climbing is the signal —
/// it means acknowledgements and renders are talking past each other.
pub static ACK_MISMATCHES_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Whether this pod is holding a document because it is outside the canary
/// cohort.
///
/// `1` while it is. The gauge that says a canary is in progress and this
/// pod is not in it — without which a held document looks exactly like a
/// store that has nothing new to say.
pub static CANARY_HOLDING: AtomicU64 = AtomicU64::new(0);

/// The cohort percentage this pod last read.
///
/// A hundred when no canary is configured, which is what "everybody" reads
/// as on a dashboard beside the gauge above.
pub static CANARY_PERCENT: AtomicU64 = AtomicU64::new(100);

/// How many times the store's client was rebuilt for rotated trust material.
///
/// A counter rather than a gauge: the interesting question is whether a
/// rotation was picked up at all, and a fleet where this stays at zero
/// through a CA rotation is a fleet that did not notice.
pub static TLS_RELOADS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Whether this agent is talking to its store without authenticating it.
///
/// `1` when `--tls-skip-verify` is on. A gauge rather than a log line
/// repeated per fetch, because the question it answers is a fleet question
/// — *which of these five thousand pods are doing this?* — and one alert
/// over a gauge answers it where five thousand log streams do not.
pub static TLS_VERIFICATION_SKIPPED: AtomicU64 = AtomicU64::new(0);

/// Whether the document is currently absent from the store.
///
/// `1` while it is gone, back to `0` when it returns — so an alert can fire
/// on the *state* rather than on the rate of a counter.
pub static ABSENT: AtomicU64 = AtomicU64::new(0);

/// The store's own revision of the document last rendered, when it counts.
///
/// Zero for a store that names no revision, and for one whose revision is
/// opaque — an ETag has no number to report, and inventing one would make a
/// dashboard compare things that do not compare.
pub static GENERATION: AtomicU64 = AtomicU64::new(0);

/// How stale a document may be before this agent stops reporting ready.
///
/// Zero means no limit, which is the default: last-known-good with no
/// ceiling is the behaviour every version before this had.
pub static MAX_STALENESS_SECONDS: AtomicU64 = AtomicU64::new(0);

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

/// A lease was extended, and for how long.
pub fn lease_renewed(seconds: u64) {
    LEASE_RENEWALS_TOTAL.fetch_add(1, Ordering::Relaxed);
    LEASE_TTL_SECONDS.store(seconds, Ordering::Relaxed);
}

/// A renewal did not succeed.
pub fn lease_renewal_failed() {
    LEASE_RENEWAL_FAILURES_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// A lease was handed back.
pub fn lease_revoked() {
    LEASE_REVOCATIONS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// The lease a fetch arrived with, for a store that issues them.
pub fn lease_held(seconds: u64) {
    LEASE_TTL_SECONDS.store(seconds, Ordering::Relaxed);
}

pub fn rendered() {
    // Whatever was missing is not missing now.
    ABSENT.store(0, Ordering::Relaxed);
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
    let staleness = staleness();

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
         {prefix}_staleness_seconds {staleness}\n\
         # TYPE {prefix}_lease_renewals_total counter\n\
         {prefix}_lease_renewals_total {}\n\
         # TYPE {prefix}_lease_renewal_failures_total counter\n\
         {prefix}_lease_renewal_failures_total {}\n\
         # TYPE {prefix}_lease_revocations_total counter\n\
         {prefix}_lease_revocations_total {}\n\
         # TYPE {prefix}_lease_ttl_seconds gauge\n\
         {prefix}_lease_ttl_seconds {}\n\
         # TYPE {prefix}_generation gauge\n\
         {prefix}_generation {}\n\
         # TYPE {prefix}_notifications_total counter\n\
         {prefix}_notifications_total {}\n\
         # TYPE {prefix}_notification_failures_total counter\n\
         {prefix}_notification_failures_total {}\n\
         # TYPE {prefix}_drift gauge\n\
         {prefix}_drift {}\n\
         # TYPE {prefix}_drift_total counter\n\
         {prefix}_drift_total {}\n\
         # TYPE {prefix}_canary_holding gauge\n\
         {prefix}_canary_holding {}\n\
         # TYPE {prefix}_canary_percent gauge\n\
         {prefix}_canary_percent {}\n\
         # TYPE {prefix}_acks_total counter\n\
         {prefix}_acks_total {}\n\
         # TYPE {prefix}_ack_mismatches_total counter\n\
         {prefix}_ack_mismatches_total {}\n\
         # TYPE {prefix}_applied gauge\n\
         {prefix}_applied {}\n\
         # TYPE {prefix}_unapplied_seconds gauge\n\
         {prefix}_unapplied_seconds {}\n\
         # TYPE {prefix}_tls_reloads_total counter\n\
         {prefix}_tls_reloads_total {}\n\
         # TYPE {prefix}_tls_verification_skipped gauge\n\
         {prefix}_tls_verification_skipped {}\n\
         # TYPE {prefix}_absent gauge\n\
         {prefix}_absent {}\n\
         # TYPE {prefix}_absent_total counter\n\
         {prefix}_absent_total {}\n{}",
        RENDERS_TOTAL.load(Ordering::Relaxed),
        RENDER_FAILURES_TOTAL.load(Ordering::Relaxed),
        LAST_RENDER_TIMESTAMP.load(Ordering::Relaxed),
        DELIVERIES_TOTAL.load(Ordering::Relaxed),
        RESYNCS_TOTAL.load(Ordering::Relaxed),
        WATCH_CONNECTED.load(Ordering::Relaxed),
        WATCH_RECONNECTS_TOTAL.load(Ordering::Relaxed),
        LEASE_RENEWALS_TOTAL.load(Ordering::Relaxed),
        LEASE_RENEWAL_FAILURES_TOTAL.load(Ordering::Relaxed),
        LEASE_REVOCATIONS_TOTAL.load(Ordering::Relaxed),
        LEASE_TTL_SECONDS.load(Ordering::Relaxed),
        GENERATION.load(Ordering::Relaxed),
        NOTIFICATIONS_TOTAL.load(Ordering::Relaxed),
        NOTIFICATION_FAILURES_TOTAL.load(Ordering::Relaxed),
        DRIFT.load(Ordering::Relaxed),
        DRIFT_TOTAL.load(Ordering::Relaxed),
        CANARY_HOLDING.load(Ordering::Relaxed),
        CANARY_PERCENT.load(Ordering::Relaxed),
        ACKS_TOTAL.load(Ordering::Relaxed),
        ACK_MISMATCHES_TOTAL.load(Ordering::Relaxed),
        u64::from(applied()),
        unapplied_seconds(),
        TLS_RELOADS_TOTAL.load(Ordering::Relaxed),
        TLS_VERIFICATION_SKIPPED.load(Ordering::Relaxed),
        ABSENT.load(Ordering::Relaxed),
        ABSENT_TOTAL.load(Ordering::Relaxed),
        histogram(prefix),
    )
}

/// Whether this process is ready to be sent work.
///
/// A function rather than a rule written into the server, because the two
/// binaries that share this server mean different things by ready. The
/// agent means *a document has been rendered*; the operator means *the
/// process is up*, and keying its readiness on a render would leave an
/// operator with no `Render` resources permanently unready.
pub type Ready = fn() -> bool;

/// The agent's answer: a document exists on disk, and is not too old.
///
/// Two questions, because last-known-good answers the first and leaves the
/// second open. Before the first render there is no configuration at all,
/// and a pod whose configuration does not exist yet is one a Service should
/// not be sending traffic to. After it, a pod can go on serving a document
/// for as long as the store is unreachable — which is the point, up to
/// whatever "too old to trust" means for that configuration.
///
/// The ceiling is off unless somebody sets one: a credential may be
/// worthless after five minutes and a feature flag fine after a week, and
/// this cannot know which it is holding.
#[must_use]
pub fn rendered_at_least_once() -> bool {
    if RENDERS_TOTAL.load(Ordering::Relaxed) == 0 {
        return false;
    }

    let ceiling = MAX_STALENESS_SECONDS.load(Ordering::Relaxed);

    ceiling == 0 || staleness() <= ceiling
}

/// Sets how stale a document may be before this agent reports unready.
///
/// Zero is no ceiling, which is the default and the behaviour every version
/// before this had: last-known-good with no limit.
pub fn set_max_staleness(seconds: u64) {
    MAX_STALENESS_SECONDS.store(seconds, Ordering::Relaxed);
}

/// Seconds since a fetch last succeeded; zero before the first.
#[must_use]
pub fn staleness() -> u64 {
    let last = LAST_SUCCESS_TIMESTAMP.load(Ordering::Relaxed);

    if last == 0 {
        0
    } else {
        now().saturating_sub(last)
    }
}

/// The store answered that the document is not there.
pub fn absent() {
    ABSENT_TOTAL.fetch_add(1, Ordering::Relaxed);
    ABSENT.store(1, Ordering::Relaxed);
}

/// Records the revision a render came from, when the store counts them.
pub fn generation(revision: Option<&dynamic_config::Revision>) {
    if let Some(dynamic_config::Revision::Counter(number)) = revision {
        GENERATION.store(*number, Ordering::Relaxed);
    }
}

/// Always ready. For a process whose readiness is its own liveness.
#[must_use]
pub const fn always() -> bool {
    true
}

/// Serves the exposition on every connection — GET is the only verb a
/// scraper sends, and answering it unconditionally keeps this at forty
/// lines instead of an HTTP framework.
pub async fn serve(address: String, prefix: &'static str, ready: Ready) {
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
            //
            // The two mean different things, and answering them the same
            // way — which this did — is what made `/readyz` useless: it
            // said `ok` before the first render, so a probe on it could not
            // tell a pod with configuration from one without.
            // The one verb that is not a GET, and the only place anything
            // outside this process writes to it. An application that has
            // read the rendered document POSTs the fingerprint it is
            // running — from the meta file, or from `fingerprint()` in a
            // binding — and the answer says whether that is the current
            // one.
            let (status, body) = if request.starts_with("POST /applied") {
                let fingerprint = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body.trim())
                    .unwrap_or_default()
                    .to_owned();

                if fingerprint.is_empty() {
                    (
                        "400 Bad Request",
                        "the body is the fingerprint this application is running\n".to_owned(),
                    )
                } else if acknowledged(&fingerprint) {
                    ("200 OK", "current\n".to_owned())
                } else {
                    // Not an error the application caused: a restart
                    // acknowledges what it read before the restart, and a
                    // slow one acknowledges a generation the store has
                    // already replaced. `409` says *which* rather than
                    // pretending it matched.
                    (
                        "409 Conflict",
                        format!(
                            "a different document is published: {}\n",
                            RENDERED_FINGERPRINT
                                .lock()
                                .map(|held| held.clone())
                                .unwrap_or_default()
                        ),
                    )
                }
            } else if request.starts_with("GET /healthz") {
                ("200 OK", "ok\n".to_owned())
            } else if request.starts_with("GET /readyz") {
                if ready() {
                    ("200 OK", "ok\n".to_owned())
                } else {
                    (
                        "503 Service Unavailable",
                        "no configuration rendered\n".to_owned(),
                    )
                }
            } else {
                ("200 OK", exposition(prefix))
            };

            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: text/plain; version=0.0.4\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );

            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

/// A post-render notification that was answered.
pub fn notified() {
    NOTIFICATIONS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// One that was not.
pub fn notification_failed() {
    NOTIFICATION_FAILURES_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Records what was just published, for an acknowledgement to match.
pub fn published(fingerprint: &str) {
    if let Ok(mut held) = RENDERED_FINGERPRINT.lock() {
        held.clear();
        held.push_str(fingerprint);
    }
}

/// The application says it is running `fingerprint`.
///
/// Answers whether it matches what was published — the caller turns that
/// into the HTTP status, so an application that acknowledges the wrong
/// generation finds out rather than being told `ok` and left believing it
/// converged.
pub fn acknowledged(fingerprint: &str) -> bool {
    ACKS_TOTAL.fetch_add(1, Ordering::Relaxed);
    APPLIED_TIMESTAMP.store(now(), Ordering::Relaxed);

    if let Ok(mut held) = APPLIED_FINGERPRINT.lock() {
        held.clear();
        held.push_str(fingerprint);
    }

    let matches = RENDERED_FINGERPRINT
        .lock()
        .is_ok_and(|rendered| *rendered == fingerprint);

    if !matches {
        ACK_MISMATCHES_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    matches
}

/// Whether the application is running what was last published.
///
/// `false` before anything is published *and* before anything acknowledges,
/// which are two different situations that this gauge deliberately does not
/// tell apart: neither one is a converged pod.
#[must_use]
pub fn applied() -> bool {
    let (Ok(rendered), Ok(applied)) = (RENDERED_FINGERPRINT.lock(), APPLIED_FINGERPRINT.lock())
    else {
        return false;
    };

    !rendered.is_empty() && *rendered == *applied
}

/// How long the published document has gone unacknowledged.
///
/// Zero when the application is up to date, and zero when nothing has been
/// published — there is nothing to be behind on. Counts from the render
/// rather than from the last acknowledgement, because the question is how
/// long *this* document has been waiting.
#[must_use]
pub fn unapplied_seconds() -> u64 {
    if applied() {
        return 0;
    }

    let rendered_at = LAST_RENDER_TIMESTAMP.load(Ordering::Relaxed);

    if rendered_at == 0 {
        0
    } else {
        now().saturating_sub(rendered_at)
    }
}

/// Ready only once the application has acknowledged what was published.
///
/// For `dynamic-config.rs/require-ack`: a pod that has a document its
/// application has not applied is a pod a Service should not be sending
/// traffic to, which is the strongest readiness this integration can
/// offer — and it needs the application's cooperation, which is why it is
/// not the default.
#[must_use]
pub fn rendered_and_applied() -> bool {
    rendered_at_least_once() && applied()
}

/// This pod is outside the cohort and is holding what it fetched.
pub fn canary_holding(percent: Option<u8>) {
    CANARY_HOLDING.store(1, Ordering::Relaxed);
    CANARY_PERCENT.store(u64::from(percent.unwrap_or(100)), Ordering::Relaxed);
}

/// The cohort widened far enough to include it.
pub fn canary_admitted(percent: Option<u8>) {
    CANARY_HOLDING.store(0, Ordering::Relaxed);
    CANARY_PERCENT.store(u64::from(percent.unwrap_or(100)), Ordering::Relaxed);
}

/// The store's client was rebuilt for rotated trust material.
pub fn tls_reloaded() {
    TLS_RELOADS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// The rendered files are no longer what was rendered.
pub fn drifted() {
    DRIFT_TOTAL.fetch_add(1, Ordering::Relaxed);
    DRIFT.store(1, Ordering::Relaxed);
}

/// They are again.
pub fn undrifted() {
    DRIFT.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two probe paths mean different things, and answering them the
    /// same way — which this did — made `/readyz` useless: it said `ok`
    /// before the first render, so nothing could tell a pod with
    /// configuration from one without.
    #[test]
    fn readiness_waits_for_a_render_and_liveness_does_not() {
        RENDERS_TOTAL.store(0, Ordering::Relaxed);

        assert!(!rendered_at_least_once());

        rendered();

        assert!(rendered_at_least_once());
    }

    /// The operator shares this server and already probes `/readyz`. An
    /// operator with no `Render` resources has rendered nothing and is
    /// working perfectly, so its answer cannot be the agent's.
    #[test]
    fn the_operator_is_ready_whatever_has_been_rendered() {
        assert!(always());
    }

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

#[cfg(test)]
mod acknowledgement_tests {
    use super::*;

    /// Serialised: these read and write process-wide statics, which is what
    /// they are — one agent, one document, one application.
    fn reset() {
        published("");
        if let Ok(mut held) = APPLIED_FINGERPRINT.lock() {
            held.clear();
        }
    }

    #[test]
    fn an_acknowledgement_of_the_current_document_converges() {
        let _guard = ORDER.lock();
        reset();

        published("sha256:aaa");

        assert!(!applied(), "nothing has acknowledged yet");
        assert!(acknowledged("sha256:aaa"), "that is what was published");
        assert!(applied());
    }

    /// A restarted application acknowledges what it read before the
    /// restart, and a slow one acknowledges a generation the store has
    /// already replaced. Neither is an error the application caused, and
    /// neither is convergence.
    #[test]
    fn an_acknowledgement_of_a_different_document_does_not() {
        let _guard = ORDER.lock();
        reset();

        published("sha256:bbb");

        assert!(!acknowledged("sha256:aaa"));
        assert!(
            !applied(),
            "the application is behind, and the gauge says so"
        );
    }

    /// Nothing published is not convergence either — there is nothing to
    /// have converged on.
    #[test]
    fn an_agent_that_has_published_nothing_is_never_applied() {
        let _guard = ORDER.lock();
        reset();

        assert!(!acknowledged("sha256:aaa"));
        assert!(!applied());
    }

    /// An application that never acknowledges is not penalised: it leaves
    /// the gauge at zero and nothing else changes.
    #[test]
    fn silence_is_not_a_failure() {
        let _guard = ORDER.lock();
        reset();

        published("sha256:ccc");

        assert!(!applied());
        assert_eq!(
            ACK_MISMATCHES_TOTAL.load(Ordering::Relaxed),
            ACK_MISMATCHES_TOTAL.load(Ordering::Relaxed),
            "nothing arrived, so nothing was counted against it"
        );
    }

    static ORDER: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
