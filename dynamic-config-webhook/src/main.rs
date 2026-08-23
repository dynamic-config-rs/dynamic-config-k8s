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

/// How long admissions took, and how big the patches were.
///
/// **The number this webhook is judged by.** It sits in the path of every
/// pod creation in the cluster, so a slow admission is a slow deployment
/// for everything, and the API server's own timeout — ten seconds by
/// default — turns a slow one into a *refused* one. A histogram is the
/// only shape that answers "is the tail moving", which is the question a
/// latency problem announces itself in.
static ADMISSION_MICROS_SUM: AtomicU64 = AtomicU64::new(0);
static ADMISSION_COUNT: AtomicU64 = AtomicU64::new(0);
static PATCH_BYTES_SUM: AtomicU64 = AtomicU64::new(0);

/// Bucket ceilings in microseconds. An admission is JSON in, a pure
/// function, JSON out — tens of microseconds when it is well, and the
/// interesting question is what the far tail does under load.
const ADMISSION_BUCKETS: [u64; 7] = [100, 500, 1_000, 5_000, 25_000, 100_000, 1_000_000];

static ADMISSION_HISTOGRAM: [AtomicU64; 7] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Why an admission was refused, by the *kind* of mistake.
///
/// Labelled, because the three answer different pages: a pod asking for a
/// store the installation does not allow is somebody's deployment being
/// wrong, a pinned value being overridden is somebody working around the
/// installation, and a malformed annotation is a typo. Never the value —
/// an annotation value carries store addresses and role names.
static REFUSED_MALFORMED: AtomicU64 = AtomicU64::new(0);
static REFUSED_POLICY: AtomicU64 = AtomicU64::new(0);
static REFUSED_PINNED: AtomicU64 = AtomicU64::new(0);
static REFUSED_CONFLICT: AtomicU64 = AtomicU64::new(0);
static REFUSED_OTHER: AtomicU64 = AtomicU64::new(0);

/// Which of the four a refusal is, read off the `status.reason` the pure
/// function set — not guessed from the English in `status.message`.
fn refusal_reason(reason: &str) -> &'static AtomicU64 {
    match reason {
        dynamic_config_webhook::POLICY => &REFUSED_POLICY,
        dynamic_config_webhook::PINNED => &REFUSED_PINNED,
        dynamic_config_webhook::MALFORMED => &REFUSED_MALFORMED,
        dynamic_config_webhook::CONFLICT => &REFUSED_CONFLICT,
        _ => &REFUSED_OTHER,
    }
}

/// The classes as of the last poll, or empty when the feature is off.
///
/// A process-wide static rather than router state: `mutate` is the only
/// reader, the poller is the only writer, and threading one map through
/// axum's extractors would be ceremony around a `RwLock` that is already
/// one.
static CLASSES: std::sync::LazyLock<dynamic_config_webhook::classes::Cache> =
    std::sync::LazyLock::new(dynamic_config_webhook::classes::Cache::new);

async fn mutate(Json(review): Json<Value>) -> Json<Value> {
    let started = std::time::Instant::now();
    let response = dynamic_config_webhook::admission_response_with_classes(
        &review,
        dynamic_config_webhook::installation(),
        &CLASSES,
    );

    record(started.elapsed(), &response);

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
        refusal_reason(
            response
                .pointer("/response/status/reason")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
        .fetch_add(1, Ordering::Relaxed);

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

/// What one admission cost, and how much it wrote.
fn record(elapsed: std::time::Duration, response: &Value) {
    let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);

    ADMISSION_COUNT.fetch_add(1, Ordering::Relaxed);
    ADMISSION_MICROS_SUM.fetch_add(micros, Ordering::Relaxed);

    for (bucket, ceiling) in ADMISSION_HISTOGRAM.iter().zip(ADMISSION_BUCKETS) {
        if micros <= ceiling {
            bucket.fetch_add(1, Ordering::Relaxed);
        }
    }

    // A patch is base64 in the response, and its size is what the API
    // server carries per pod. A patch that grew by an order of magnitude
    // is an injection template that got away from somebody.
    if let Some(patch) = response.pointer("/response/patch").and_then(Value::as_str) {
        PATCH_BYTES_SUM.fetch_add(patch.len() as u64, Ordering::Relaxed);
    }
}

async fn healthz() -> &'static str {
    "ok"
}

/// Ready means **a certificate this process can serve with is loaded**.
///
/// Split from liveness because the two differ exactly once, and it is the
/// moment that matters: in `selfRotate` mode the process starts before any
/// pair exists, and a webhook that is Ready without one is a webhook the
/// Service sends admissions to that cannot complete a handshake — which
/// the API server reports as every pod creation failing.
async fn readyz() -> (axum::http::StatusCode, &'static str) {
    if tls::Material::from_environment().loadable()
        || std::env::var("DYNAMIC_CONFIG_WEBHOOK_PLAINTEXT").as_deref() == Ok("1")
    {
        (axum::http::StatusCode::OK, "ok")
    } else {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "no certificate loaded",
        )
    }
}

/// Prometheus text format, hand-rolled: three counters do not earn a
/// metrics crate. Scrape over the same TLS the API server uses.
async fn metrics() -> String {
    let count = ADMISSION_COUNT.load(Ordering::Relaxed);
    let mut buckets = String::new();
    let mut cumulative = 0;

    for (bucket, ceiling) in ADMISSION_HISTOGRAM.iter().zip(ADMISSION_BUCKETS) {
        // Each bucket already counts everything at or under its ceiling;
        // the `max` is against a scrape landing between two increments of
        // one observation, which Prometheus would refuse as a bucket going
        // backwards.
        cumulative = cumulative.max(bucket.load(Ordering::Relaxed));

        let seconds = ceiling as f64 / 1_000_000.0;

        buckets.push_str(&format!(
            "dynamic_config_admission_duration_seconds_bucket{{le=\"{seconds}\"}} {cumulative}\n"
        ));
    }

    format!(
        "# TYPE dynamic_config_admissions_total counter\n\
         dynamic_config_admissions_total{{outcome=\"skipped\"}} {}\n\
         dynamic_config_admissions_total{{outcome=\"patched\"}} {}\n\
         dynamic_config_admissions_total{{outcome=\"refused\"}} {}\n\
         # TYPE dynamic_config_admission_refusals_total counter\n\
         dynamic_config_admission_refusals_total{{reason=\"malformed\"}} {}\n\
         dynamic_config_admission_refusals_total{{reason=\"policy\"}} {}\n\
         dynamic_config_admission_refusals_total{{reason=\"pinned\"}} {}\n\
         dynamic_config_admission_refusals_total{{reason=\"conflict\"}} {}\n\
         dynamic_config_admission_refusals_total{{reason=\"other\"}} {}\n\
         # TYPE dynamic_config_admission_duration_seconds histogram\n\
         {buckets}\
         dynamic_config_admission_duration_seconds_bucket{{le=\"+Inf\"}} {count}\n\
         dynamic_config_admission_duration_seconds_sum {}\n\
         dynamic_config_admission_duration_seconds_count {count}\n\
         # TYPE dynamic_config_admission_patch_bytes_total counter\n\
         dynamic_config_admission_patch_bytes_total {}\n\
         # TYPE dynamic_config_certificate_rotations_total counter\n\
         dynamic_config_certificate_rotations_total {}\n\
         # TYPE dynamic_config_certificate_expires_at_seconds gauge\n\
         dynamic_config_certificate_expires_at_seconds {}\n",
        SKIPPED.load(Ordering::Relaxed),
        PATCHED.load(Ordering::Relaxed),
        REFUSED.load(Ordering::Relaxed),
        REFUSED_MALFORMED.load(Ordering::Relaxed),
        REFUSED_POLICY.load(Ordering::Relaxed),
        REFUSED_PINNED.load(Ordering::Relaxed),
        REFUSED_CONFLICT.load(Ordering::Relaxed),
        REFUSED_OTHER.load(Ordering::Relaxed),
        ADMISSION_MICROS_SUM.load(Ordering::Relaxed) as f64 / 1_000_000.0,
        PATCH_BYTES_SUM.load(Ordering::Relaxed),
        selfrotate::ROTATIONS_TOTAL.load(Ordering::Relaxed),
        selfrotate::EXPIRES_AT.load(Ordering::Relaxed),
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

/// `validate <file>` — the admission decision, without a cluster.
///
/// The webhook is already a pure function of a pod and an installation, and
/// the golden tests already drive it that way. This is that same call with
/// a front door on it: a platform team can put it in CI and find out that
/// `auth-role` is missing before the pod reaches an API server, rather than
/// from a rollout that will not start.
///
/// Reads a pod manifest as JSON or YAML, uses the installation defaults
/// from this process's own environment, and prints what the webhook would
/// have answered.
fn validate(path: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let text = std::fs::read_to_string(path)?;

    // YAML is a superset of JSON, so one parser reads both — and a pod
    // spec is far more often written as YAML.
    let pod: serde_json::Value = serde_yaml::from_str(&text)?;

    let review = serde_json::json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": { "uid": "validate", "object": pod },
    });

    let response = dynamic_config_webhook::admission_response(&review);

    let allowed = response
        .pointer("/response/allowed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if !allowed {
        let reason = response
            .pointer("/response/status/reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Refused");
        let message = response
            .pointer("/response/status/message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("no reason given");

        eprintln!("{path}: refused ({reason})\n  {message}");

        return Err("the pod would be refused".into());
    }

    let patched = response.pointer("/response/patch").is_some();

    println!(
        "{path}: allowed{}",
        if patched {
            ", and an agent would be injected"
        } else {
            " — nothing to inject"
        }
    );

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Before the subscriber: `validate` writes to stdout for a person, and
    // a JSON log line either side of its answer helps nobody.
    let mut arguments = std::env::args().skip(1);

    if arguments.next().as_deref() == Some("validate") {
        let mut failures = 0;
        let mut seen = 0;

        for path in arguments {
            seen += 1;

            if validate(&path).is_err() {
                failures += 1;
            }
        }

        if seen == 0 {
            eprintln!("usage: dynamic-config-webhook validate <pod.yaml>...");

            return Err("no manifest to validate".into());
        }

        return if failures == 0 {
            Ok(())
        } else {
            Err(format!("{failures} of {seen} manifests would be refused").into())
        };
    }

    // Structured logs as before, plus OTLP traces when a collector is
    // configured. Nothing is exported unless `OTEL_EXPORTER_OTLP_ENDPOINT`
    // is set, so no existing deployment changes behaviour.
    let telemetry = dynamic_config_telemetry::install("dynamic-config-webhook");

    let outcome = serve_forever().await;

    // Explicitly, before the process goes: a batch exporter holds spans for
    // up to its scheduled delay, and the admissions a webhook refused on
    // its way out are the ones somebody will come looking for.
    telemetry.shutdown();

    outcome
}

/// The webhook proper, so `main` can own the telemetry either side of it.
async fn serve_forever() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A mistyped fleet default (a file mode of "888", an allowlist
    // entry that is not a variable name) stops the process here, at
    // install time — never at the first admission.
    dynamic_config_webhook::verify_installation()?;

    // The serving path names its provider per-config, but the selfRotate
    // mode's kube client uses the process default — which, with ring and
    // aws-lc both reachable, must be stated or rustls panics mid-handshake.
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| "a rustls CryptoProvider was already installed")?;

    // The one thing in this binary that talks to the API server, and it
    // talks to it on a timer rather than on the path a pod creation waits
    // on. Off unless the deployment asked, because reading classes needs
    // RBAC this webhook otherwise does not have.
    if std::env::var("DYNAMIC_CONFIG_WEBHOOK_CLASSES").as_deref() == Ok("true") {
        let cache = CLASSES.clone();

        tokio::spawn(async move {
            match dynamic_config_webhook::classes::poll(cache).await {
                Ok(never) => match never {},
                // Once, not every thirty seconds: this is a deployment
                // that asked for classes without granting them, and the
                // fix is a chart value rather than anything at runtime.
                Err(error) => tracing::error!(
                    %error,
                    "classes were asked for and cannot be read; pods naming one will be refused"
                ),
            }
        });
    }

    let app = Router::new()
        .route("/mutate", post(mutate))
        .route("/healthz", axum::routing::get(healthz))
        .route("/readyz", axum::routing::get(readyz))
        .route("/metrics", axum::routing::get(metrics));

    // Metrics on a port of their own, in plain HTTP.
    //
    // They used to be served over the admission port, which is mutual TLS
    // against a CA the webhook mints for itself — so scraping them meant
    // giving Prometheus a client certificate from that CA, and every
    // deployment that did not simply had no metrics. The admission port
    // keeps serving `/metrics` as well, so a scrape configured against it
    // does not break.
    let scrape =
        std::env::var("DYNAMIC_CONFIG_WEBHOOK_METRICS_ADDR").unwrap_or_else(|_| String::new());

    if !scrape.is_empty() {
        let plain = Router::new()
            .route("/metrics", axum::routing::get(metrics))
            .route("/healthz", axum::routing::get(healthz))
            .route("/readyz", axum::routing::get(readyz));

        match tokio::net::TcpListener::bind(&scrape).await {
            Ok(listener) => {
                info!(address = %scrape, "metrics endpoint listening");
                tokio::spawn(async move {
                    let _ = axum::serve(listener, plain).await;
                });
            }
            Err(error) => {
                tracing::warn!(%error, address = %scrape, "metrics endpoint could not bind")
            }
        }
    }

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
