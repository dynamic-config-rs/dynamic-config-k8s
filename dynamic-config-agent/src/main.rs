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
//! Output format follows `--out`'s extension, and `.properties`/`.ini`
//! are legal *here* although the engine's `save` refuses them: the
//! engine's contract is a round trip, and a rendered file for a consumer
//! is not one — this binary flattens under its own stated rules
//! (documented in the book's Rendering chapter) and owns that choice.

#![forbid(unsafe_code)]

mod render;
mod sources;
mod spec;

use std::time::Duration;

use tracing::{info, warn};

fn main() -> std::process::ExitCode {
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

    match run(&spec) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            warn!(error = %error, "agent stopped");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(spec: &spec::Spec) -> Result<(), Box<dyn std::error::Error>> {
    let source = sources::build(spec)?;

    info!(source = %source.describe(), out = %spec.out.display(), "agent starting");

    let mut last_rendered: Option<String> = None;

    loop {
        match render::fetch_and_render(source.as_ref(), spec) {
            Ok(rendered) => {
                if last_rendered.as_deref() != Some(rendered.as_str()) {
                    render::write_atomically(&spec.out, &rendered)?;
                    info!(bytes = rendered.len(), "rendered");
                    last_rendered = Some(rendered);
                } else {
                    info!("unchanged");
                }
            }
            Err(error) => {
                // The file keeps its last good content — the same
                // keep-serving rule the engine applies in-process. An
                // init run has no last good content to keep, so it fails.
                if spec.watch.is_none() || last_rendered.is_none() {
                    return Err(error);
                }

                warn!(error = %error, "fetch failed; the rendered file is unchanged");
            }
        }

        match spec.watch {
            Some(interval) => std::thread::sleep(interval),
            None => return Ok(()),
        }
    }

    #[allow(unreachable_code)]
    Ok(())
}

#[allow(dead_code)]
fn unused_duration_helper(_: Duration) {}
