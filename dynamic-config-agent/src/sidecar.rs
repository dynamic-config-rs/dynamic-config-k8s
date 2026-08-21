//! The sidecar loop: watch the store, render what it delivers.
//!
//! In the library rather than in `main.rs` so it can be driven by a test
//! against a store that is not a network — the loop is where the agent's
//! behaviour under a stalled stream, a dropped connection and an unchanged
//! document lives, and none of that is reachable through a binary.

use std::sync::Arc;
use std::time::Duration;

use dynamic_config::{Error, Fetched, Pace, RemoteWatch, WatchCapability};
use tracing::{info, warn};

use crate::sources::Built;
use crate::spec::Spec;

/// The sidecar: the store's own watch, plus what it cannot be trusted for.
///
/// Every store is watched rather than polled, which is not the same
/// sentence for each of them. A store that pushes delivers a change as it
/// happens; a store that answers "has it changed?" cheaply is asked that
/// question instead of being made to send the whole document — an S3
/// object used to be downloaded every interval and is now a `HEAD`.
///
/// On top of the store's watch, two things this loop owns:
///
/// - **A resync**, for a store that pushes. The failure mode of a stream is
///   silence: a subscription the broker forgot looks exactly like a store
///   where nothing has changed, and only going and asking tells them apart.
/// - **A restart**, spread and backing off. A watch that ends — a rolling
///   restart of the store, a dropped connection — is waited out and opened
///   again rather than taking the pod down with it.
pub async fn run(
    spec: &Spec,
    source: &Arc<Built>,
    interval: Duration,
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let capability = source.capability();
    // One document in flight: the renderer is a file write, so a queue
    // here would only ever hold documents that a newer one supersedes.
    let (deliver, mut delivered) = tokio::sync::mpsc::channel::<Fetched>(1);
    let handle = RemoteWatch::new();
    let mut pace = Pace::new(interval);

    // **The file exists before the watch does.** A watch delivers a
    // *change*, and the stores keep that literally: the current value is
    // not delivered at startup, because a caller that wanted it fetches
    // it. An agent is exactly such a caller — the app beside it opens the
    // rendered file as soon as the pod is ready, and without this it would
    // find nothing until the configuration happened to move or the resync
    // came round.
    //
    // A failure here ends the agent rather than being waited out: a
    // sidecar that has never written the file is a pod whose app is
    // reading something that is not there, and a restart with Kubernetes'
    // backoff is a better answer than a container that looks healthy and
    // serves nothing. Once the first render is down, every later failure
    // is survivable — there is a good file to keep.
    let first = source.fetch().await?;
    let mut rendered = Some(crate::render::render_fetched(&first, spec)?);

    crate::render::write_atomically(
        &spec.out,
        rendered.as_deref().unwrap_or_default(),
        spec.file_mode,
    )?;
    crate::metrics::rendered();
    info!(
        bytes = rendered.as_deref().unwrap_or_default().len(),
        "rendered"
    );

    let mut watcher = spawn_watch(source, &handle, interval, deliver.clone());
    let mut resync = tokio::time::interval(interval);

    // A tick that is late does not become two ticks: the work between them
    // takes as long as it takes, and catching up would mean a burst at the
    // store rather than a steady cadence.
    resync.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    resync.tick().await;

    tokio::pin!(stop);

    loop {
        let arrived = tokio::select! {
            // Sidecar mode never reaches this in production — `main` waits
            // on a future that never completes — and a test needs a way to
            // end a loop whose whole job is not to.
            () = &mut stop => return Ok(()),

            document = delivered.recv() => match document {
                Some(document) => {
                    crate::metrics::delivered();
                    Some(document)
                }
                None => None,
            },

            // Only for a store that pushes. A conditional or interval store
            // is already being asked on this cadence by its own watch, and
            // a second timer would double its load on the store.
            _ = resync.tick(), if capability == WatchCapability::Native => {
                // Counted *after* the store answers. Marking the resync
                // before the fetch moved the staleness clock on every
                // attempt, so a store that had been unreachable for an
                // hour reported a document read seconds ago — the gauge an
                // operator pages on, saying the opposite of what happened.
                match source.fetch().await {
                    Ok(document) => {
                        crate::metrics::resynced();

                        Some(document)
                    }
                    Err(error) => {
                        warn!(error = %error, "the resync failed; the rendered file is unchanged");
                        crate::metrics::failed();
                        None
                    }
                }
            }

            ended = &mut watcher => {
                match ended {
                    Ok(Ok(())) => info!("the store's watch ended; reopening"),
                    Ok(Err(error)) => {
                        warn!(error = %error, "the store's watch failed; reopening");
                        crate::metrics::failed();
                    }
                    Err(error) => warn!(error = %error, "the watch task ended; reopening"),
                }

                crate::metrics::watch_down();

                let wait = pace.next_wait();

                pace.failed();
                tokio::time::sleep(wait).await;

                watcher = spawn_watch(source, &handle, interval, deliver.clone());
                continue;
            }
        };

        let Some(document) = arrived else {
            continue;
        };

        pace.succeeded();

        match crate::render::render_fetched(&document, spec) {
            Ok(fresh) => {
                if rendered.as_deref() == Some(fresh.as_str()) {
                    info!("unchanged");
                    continue;
                }

                crate::render::write_atomically(&spec.out, &fresh, spec.file_mode)?;
                crate::metrics::rendered();
                info!(bytes = fresh.len(), "rendered");
                rendered = Some(fresh);
            }
            Err(error) => {
                // The file keeps its last good content — the same
                // keep-serving rule the engine applies in-process.
                crate::metrics::failed();
                warn!(error = %error, "rendering failed; the rendered file is unchanged");
            }
        }
    }
}

/// One run of the store's watch, on a task of its own.
fn spawn_watch(
    source: &Arc<Built>,
    handle: &RemoteWatch,
    interval: Duration,
    deliver: tokio::sync::mpsc::Sender<Fetched>,
) -> tokio::task::JoinHandle<Result<(), Error>> {
    let source = Arc::clone(source);
    let watching = handle.watching();

    crate::metrics::watch_up();

    tokio::spawn(async move { source.watch(watching, interval, deliver).await })
}
