//! The agent: a pod's configuration, rendered to a file it can watch.
//!
//! ```text
//! dynamic-config-agent --source etcd --endpoint http://etcd:2379 \
//!     --key myapp/config.json --out /config/rendered.toml [--watch 15s]
//! ```
//!
//! One job: fetch a document from a remote store, resolve it through the
//! same engine every binding uses, and write the *resolved* document to
//! a path — atomically, write-then-rename, so the application's own
//! dynamic-config watcher (or anything inotify-shaped) picks up whole
//! files and never half ones. `--one-shot` is the init-container mode;
//! `--watch <interval>` is the sidecar.
//!
//! **The sidecar watches rather than polls.** Each store says how it
//! learns that its document changed, and the agent uses it: a change in
//! etcd, Consul, NATS, Redis or a config server arrives as it happens,
//! and a store that has to be asked is asked the cheapest question it
//! offers — an S3 object is a `HEAD` rather than a download. The interval
//! is what remains: how often to ask a store that must be asked, and how
//! often to re-read one that pushes, because a subscription the broker
//! forgot looks exactly like a store where nothing has changed.
//!
//! Output format follows `--out`'s extension, and `.properties`/`.ini`
//! are legal *here* although the engine's `save` refuses them: the
//! engine's contract is a round trip, and a rendered file for a consumer
//! is not one — this binary flattens under its own stated rules
//! (documented in the book's Rendering chapter) and owns that choice.

#![forbid(unsafe_code)]

use dynamic_config_agent::{render, sources, spec};

use tracing::{info, warn};

/// SIGTERM is how Kubernetes asks; ctrl-c is how a terminal does.
///
/// The same shape the webhook uses, for the same reason: a shutdown that
/// only hears one of the two is a shutdown that works in exactly one of
/// the two places it runs.
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
    let _ = interrupt.await;
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // Structured logs from the first line: this process's audience is
    // `kubectl logs`, and JSON is what log pipelines index. The engine's
    // own diagnostics join via its `tracing` feature. OTLP traces ride
    // beside them when a collector is configured, and nothing is exported
    // unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set — so no existing
    // deployment changes behaviour.
    let telemetry = dynamic_config_telemetry::install("dynamic-config-agent");

    let spec = match spec::Spec::from_args(std::env::args().skip(1)) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("dynamic-config-agent: {error}");
            eprintln!("{}", spec::USAGE);
            telemetry.shutdown();
            return std::process::ExitCode::FAILURE;
        }
    };

    let outcome = match run(&spec).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            warn!(error = %error, "agent stopped");
            std::process::ExitCode::FAILURE
        }
    };

    // Explicitly, before the process goes: a batch exporter holds spans
    // for up to its scheduled delay, and the last seconds of a pod that is
    // shutting down are usually the interesting ones.
    telemetry.shutdown();

    outcome
}

async fn run(spec: &spec::Spec) -> Result<(), Box<dyn std::error::Error>> {
    let source = std::sync::Arc::new(sources::build(spec).await?);
    let capability = source.capability();

    info!(
        source = %source.describe(),
        out = %spec.out.display(),
        watch = %capability,
        "agent starting"
    );

    // Once, loudly, at startup — and as a gauge, which is the half that
    // scales: a fleet-wide alert finds every pod doing this, where a log
    // line repeated per fetch would only bury it.
    if spec.tls_skip_verify {
        dynamic_config_agent::metrics::TLS_VERIFICATION_SKIPPED
            .store(1, std::sync::atomic::Ordering::Relaxed);

        warn!(
            endpoint = %spec.endpoint,
            "TLS verification is OFF for this store: anything on the network \
             path can read this configuration and rewrite it before it \
             arrives. --ca trusts one more certificate and keeps the server \
             authenticated"
        );
    }

    // Before the server starts, so a probe can never see the default while
    // the configured ceiling is still on its way.
    if let Some(ceiling) = spec.max_staleness {
        dynamic_config_agent::metrics::set_max_staleness(ceiling.as_secs());
    }

    if let Some(address) = &spec.metrics_addr {
        tokio::spawn(dynamic_config_agent::metrics::serve(
            address.clone(),
            "dynamic_config_agent",
            // Ready means a document exists. A pod whose configuration has
            // not arrived yet is one a Service should not be sending
            // traffic to, and pod readiness is already AND-ed across
            // containers — so this is the whole mechanism.
            //
            // With `--require-ack` it means more: the application has said
            // it is *running* that document. Stronger, and off by default,
            // because it needs the application to say so and one that
            // never does would never become ready.
            if spec.require_ack {
                dynamic_config_agent::metrics::rendered_and_applied
            } else {
                dynamic_config_agent::metrics::rendered_at_least_once
            },
        ));
    }

    let Some(interval) = spec.watch else {
        // Init mode: one fetch, one render, and a failure is a failure —
        // there is no last good content to keep.
        let fetched = source.fetch().await?;
        let rendered = render::render_fetched(&fetched, spec)?;

        render::write_atomically(&spec.out, &rendered, spec.file_mode)?;
        dynamic_config_agent::metrics::rendered();
        info!(bytes = rendered.len(), "rendered");

        return Ok(());
    };

    // A sidecar runs until the pod is asked to stop. That used to be a
    // future which never completes — the loop ended when the process was
    // killed, which is fine for a file and not fine for a lease: a
    // credential minted for this pod alone would outlive it, valid until
    // its TTL ran out with nobody holding it.
    //
    // The loop can also come back asking to be rebuilt, which is how a
    // rotated CA is picked up **without a pod restart**: a new client, the
    // same process, and the rendered file never leaves the volume.
    let mut source = source;

    loop {
        match dynamic_config_agent::sidecar::run(spec, &source, interval, shutdown_signal()).await?
        {
            dynamic_config_agent::sidecar::Ended::Stopped => return Ok(()),
            dynamic_config_agent::sidecar::Ended::Rebuild => {
                // A build that fails against the *new* material is fatal
                // rather than a retry: the old client is already gone, the
                // material on disk is what the store now expects, and a
                // process looping on a bad certificate is worse than a pod
                // that restarts with the failure in its events.
                source = std::sync::Arc::new(sources::build(spec).await?);

                info!(
                    source = %source.describe(),
                    "the store's client was rebuilt from the new trust material"
                );
            }
        }
    }
}
