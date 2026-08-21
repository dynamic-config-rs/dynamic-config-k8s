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

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // Structured logs from the first line: this process's audience is
    // `kubectl logs`, and JSON is what log pipelines index. The engine's
    // own diagnostics join via its `tracing` feature.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let spec = match spec::Spec::from_args(std::env::args().skip(1)) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("dynamic-config-agent: {error}");
            eprintln!("{}", spec::USAGE);
            return std::process::ExitCode::FAILURE;
        }
    };

    match run(&spec).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            warn!(error = %error, "agent stopped");
            std::process::ExitCode::FAILURE
        }
    }
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

    if let Some(address) = &spec.metrics_addr {
        tokio::spawn(dynamic_config_agent::metrics::serve(
            address.clone(),
            "dynamic_config_agent",
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

    // A sidecar runs until the pod does not: the stop future never
    // completes, and the loop ends when the process does.
    dynamic_config_agent::sidecar::run(spec, &source, interval, std::future::pending()).await
}
