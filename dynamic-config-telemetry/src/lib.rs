//! OpenTelemetry for the three programs here, and only for them.
//!
//! The engine offers an `otel` feature that records into a meter somebody
//! else configured. *Somebody else* is this crate: a pipeline owns a
//! runtime, a batch processor and a shutdown, and a library that takes
//! those over is a library that has to be fought. Programs may take them
//! over, because programs own `main`.
//!
//! # Compiled in, dormant, and silent about it
//!
//! Nothing is exported unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set. That is
//! the whole opt-in: an existing deployment that does not set it behaves
//! exactly as it did, and one that does gets traces without changing an
//! image. The variable is OpenTelemetry's own, so a collector already
//! injecting it into pods needs no annotation from us.
//!
//! # What it does not do
//!
//! It does not replace the Prometheus text the binaries already serve. A
//! scrape is a fine way to collect these numbers and many deployments have
//! one; the OTel Collector's `prometheus` receiver reads it directly. Two
//! ways out, and neither is the price of the other.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The endpoint variable, which is also the switch.
const ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// A live pipeline, held so it can be shut down.
///
/// **Bind it.** Dropping it immediately ends the exporter, and a program
/// that lets it go at the end of `main`'s first statement exports nothing
/// while wondering why.
#[must_use = "dropping this ends the exporter; bind it for the life of the process"]
pub struct Telemetry {
    provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl Telemetry {
    /// Flushes what has been recorded and stops.
    ///
    /// Worth calling explicitly before a process exits: a batch exporter
    /// holds spans for up to its scheduled delay, and the last few seconds
    /// of a pod that is shutting down are usually the interesting ones.
    pub fn shutdown(self) {
        if let Some(provider) = self.provider {
            // Best-effort: a collector that has already gone is not a
            // reason to fail an exit that is otherwise clean.
            let _ = provider.shutdown();
        }
    }
}

/// Installs the log subscriber, and an OTLP tracing layer beside it when
/// one is configured.
///
/// `service` names this binary — `dynamic-config-agent` and so on. It is a
/// constant per program, not derived from anything a request carries: a
/// service name taken from input is a cardinality incident.
///
/// # Panics
///
/// Never for want of a collector. An endpoint that cannot be reached is a
/// warning and a process that goes on running without traces, because a
/// configuration agent that refuses to start because its *telemetry* is
/// down would be a worse outage than the one it was reporting.
pub fn install(service: &'static str) -> Telemetry {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // Structured logs from the first line, exactly as before: this
    // process's audience is `kubectl logs`, and JSON is what log pipelines
    // index.
    let logs = tracing_subscriber::fmt::layer().json();

    let Ok(endpoint) = std::env::var(ENDPOINT) else {
        tracing_subscriber::registry()
            .with(filter)
            .with(logs)
            .init();

        return Telemetry { provider: None };
    };

    match exporter(service) {
        Ok(provider) => {
            let tracer = opentelemetry::trace::TracerProvider::tracer(&provider, service);

            tracing_subscriber::registry()
                .with(filter)
                .with(logs)
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .init();

            tracing::info!(%endpoint, "exporting traces over OTLP");

            Telemetry {
                provider: Some(provider),
            }
        }
        Err(error) => {
            tracing_subscriber::registry()
                .with(filter)
                .with(logs)
                .init();

            tracing::warn!(
                %error,
                %endpoint,
                "the OTLP exporter could not be built; continuing without traces"
            );

            Telemetry { provider: None }
        }
    }
}

/// The pipeline itself.
fn exporter(
    service: &'static str,
) -> Result<opentelemetry_sdk::trace::SdkTracerProvider, Box<dyn std::error::Error>> {
    use opentelemetry::KeyValue;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()?;

    // What a trace has to carry to be findable in a cluster. The three pod
    // attributes come from the downward API, so they are absent rather than
    // wrong when nobody wired them — a made-up pod name is worse than none.
    let mut attributes = vec![
        KeyValue::new("service.name", service),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
    ];

    for (variable, attribute) in [
        ("POD_NAME", "k8s.pod.name"),
        ("POD_NAMESPACE", "k8s.namespace.name"),
        ("NODE_NAME", "k8s.node.name"),
    ] {
        if let Ok(value) = std::env::var(variable) {
            attributes.push(KeyValue::new(attribute, value));
        }
    }

    Ok(opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            opentelemetry_sdk::Resource::builder_empty()
                .with_attributes(attributes)
                .build(),
        )
        .build())
}
