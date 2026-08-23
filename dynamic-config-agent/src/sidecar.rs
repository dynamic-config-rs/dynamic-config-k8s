//! The sidecar loop: watch the store, render what it delivers.
//!
//! In the library rather than in `main.rs` so it can be driven by a test
//! against a store that is not a network — the loop is where the agent's
//! behaviour under a stalled stream, a dropped connection and an unchanged
//! document lives, and none of that is reachable through a binary.

use std::sync::Arc;
use std::time::Duration;

use dynamic_config::{Error, Lease, Pace, RemoteWatch, WatchCapability};
use tracing::{info, warn};

use crate::sources::Built;
use crate::spec::{OnDelete, OnDrift, Spec};

/// Why the loop came back.
///
/// A loop that only ever ended one way did not need this. Rebuilding the
/// store's client for rotated trust material is a second way, and it is
/// **not** a failure — the process stays up, the rendered file stays on
/// disk, the last-known-good stays, and every counter keeps counting. Only
/// the client is new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The pod is stopping.
    Stopped,
    /// The store's trust material changed and the client has to be built
    /// again from it.
    Rebuild,
}

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
/// - **A rebuild**, when the store's trust material rotates underneath it.
///   The kubelet rewrites a mounted Secret in place and tells nobody, so
///   the loop ends with [`Ended::Rebuild`] and its caller builds a fresh
///   client from the new material — without restarting the pod.
pub async fn run(
    spec: &Spec,
    source: &Arc<Built>,
    interval: Duration,
    stop: impl std::future::Future<Output = ()>,
) -> Result<Ended, Box<dyn std::error::Error>> {
    run_from(spec, source, interval, stop, None, None).await
}

/// What a caller is told when this loop accepts a document.
///
/// One caller wants it: the CSI node plugin, where a document is fetched
/// once for a node and rendered once per pod reading it. This loop
/// publishes the spec it was given; the rest is the caller's, and this is
/// how the caller hears about it.
pub type OnDocument<'a> = &'a (dyn Fn(&dynamic_config::Fetched) + Send + Sync);

/// [`run`], for a caller that has already fetched the first document.
///
/// The CSI node plugin is the one: it must render before it returns to
/// the kubelet — that is the property a volume buys over a sidecar — and
/// then hands the watch over here. Without this it fetched twice for
/// every first claim of a document on a node.
///
/// The second reason is the one that keeps this from being an
/// optimisation. A fetch of a dynamic secret **mints a lease**, and only
/// the lease this loop keeps is renewed and handed back; a discarded one
/// sits out its TTL with nobody to revoke it. A CSI volume cannot ask for
/// a dynamic secret today — `dynamic` is not among the attributes
/// `publish.rs` maps to flags — so that is a door held shut rather than a
/// leak repaired, and this is what keeps it shut if the attribute is ever
/// added.
pub async fn run_from(
    spec: &Spec,
    source: &Arc<Built>,
    interval: Duration,
    stop: impl std::future::Future<Output = ()>,
    first: Option<dynamic_config::Fetched>,
    on_document: Option<OnDocument<'_>>,
) -> Result<Ended, Box<dyn std::error::Error>> {
    let capability = source.capability();
    // One document in flight, and *latest wins* — which is what the
    // comment here always claimed. It was an `mpsc` of capacity one, which
    // is a queue that holds one: a burst made the blocking stores block
    // their own watch loop and the async stores tear the connection down.
    let (deliver, mut delivered) = crate::sources::delivery();
    let handle = RemoteWatch::new();

    // Two paces, because there are two questions and one answer cannot
    // serve both.
    //
    // `pace` is *is the store readable* — a document arriving resets it.
    // `reopening` is *is the stream healthy*, and only the watch touches
    // it. They were one, and a store whose stream kept dropping while its
    // reads kept working reopened the connection at the full interval
    // forever: every resync declared success and wiped the backoff the
    // reopen path had just started building.
    let mut pace = Pace::new(interval);
    let mut reopening = Pace::new(interval);

    // **The file exists before the watch does.** A watch delivers a
    // *change*, and the stores keep that literally: the current value is
    // not delivered at startup, because a caller that wanted it fetches
    // it. An agent is exactly such a caller — the app beside it opens the
    // rendered file as soon as the pod is ready, and without this it would
    // find nothing until the configuration happened to move or the resync
    // came round.
    //
    // A failure here used to end the agent unconditionally, and for a pod
    // that has never written the file that is still the right answer: its
    // app would be reading something that is not there, and a restart with
    // Kubernetes' backoff beats a container that looks healthy and serves
    // nothing.
    //
    // But the volume is an `emptyDir`, which survives a *container*
    // restart and dies with the pod — so an agent coming back after an OOM
    // kill or a crash often finds its own last render sitting there.
    // Refusing to start on it turned a store outage into "every restarting
    // pod stays down", when the file it needed was already on disk.
    // `--startup-policy` is which of the two a deployment wants.
    let mut first_lease = None;
    // Already in hand for the CSI plugin; fetched here for everyone else.
    let initial = match first {
        Some(fetched) => Ok(fetched),
        None => source.fetch().await,
    };

    let mut rendered: Option<Vec<crate::render::Published>> = match initial {
        Ok(first) => {
            let fresh = crate::render::render_all(&first, spec)?;

            publish(spec, &fresh, first.revision.as_ref())?;

            if let Some(hear) = on_document {
                hear(&first);
            }

            first_lease = first.lease.clone();

            if let Some(lease) = &first_lease {
                crate::metrics::lease_held(lease.ttl.as_secs());
                info!(
                    seconds = lease.ttl.as_secs(),
                    renewable = lease.renewable,
                    "the document is issued under a lease"
                );
            }

            Some(fresh)
        }
        // Nothing was issued, so there is nothing to renew or hand back.
        Err(error) => recovered(spec, &error)?,
    };

    let mut watcher = spawn_watch(source, &handle, interval, deliver.clone());
    let mut resync = tokio::time::interval(interval);

    // A tick that is late does not become two ticks: the work between them
    // takes as long as it takes, and catching up would mean a burst at the
    // store rather than a steady cadence.
    resync.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    resync.tick().await;

    // The lease the document was issued under, if it was issued under one.
    // Held here rather than fetched again, because a lease is renewed
    // whether or not anybody reads — which is precisely what makes it a
    // different thing from a document.
    let mut lease = first_lease;

    // Fires at a fraction of the lease's life, not on the resync interval:
    // a credential's clock has nothing to do with how often the store is
    // polled. Long when there is no lease, so the branch costs nothing.
    let mut renewal = tokio::time::interval(renew_after(lease.as_ref(), &mut pace));
    renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    renewal.tick().await;

    // Copied out of the spec so the spawned resync can own them: the task
    // outlives the borrow, and a policy and a list of paths are cheaper to
    // clone than to reach back for.
    let on_delete = spec.on_delete;
    let outputs: Vec<std::path::PathBuf> = std::iter::once(spec.out.clone())
        .chain(spec.also.iter().map(|rendering| rendering.out.clone()))
        .collect();

    // The rendered files are checked against what was written on the same
    // cadence the store is read: often enough that a stray write is noticed
    // in the same window a store change would have been, and no more often
    // than that. Ungated — drift has nothing to do with how the store
    // delivers.
    // The trust material as it is right now, so a rotation can be told from
    // the state it started in. `None` for a source with no TLS material,
    // which is most of them and costs nothing.
    let material = spec
        .tls_reload
        .then(|| crate::trust::Material::of(spec))
        .flatten();

    // Read once, at start: the pod does not move, and a file read per
    // failure is a file read exactly when something is already wrong. A
    // reporter that cannot be built is a warning and nothing more — the
    // renders it would have described still happen.
    let reporter = spec
        .events
        .then(crate::events::Reporter::new)
        .and_then(|built| match built {
            Ok(reporter) => Some(reporter),
            Err(error) => {
                warn!(%error, "Kubernetes Events were asked for and cannot be written");
                None
            }
        });

    // This pod's cohort, fixed for its life: the name does not change, so
    // neither does the bucket, and a cohort that reshuffled as it widened
    // would put every pod through the new document eventually and prove
    // nothing about any of them.
    let cohort = spec.canary.as_ref().map(|_| {
        crate::canary::bucket(
            &std::env::var("DYNAMIC_CONFIG_POD_NAME").unwrap_or_else(|_| "unnamed".to_owned()),
        )
    });

    // A document this pod fetched and is not allowed to publish yet. Held
    // rather than dropped: when the cohort widens there is nothing to
    // re-fetch, and the store may not have anything new to say for hours.
    let mut withheld: Option<Vec<crate::render::Published>> = None;

    let mut trusting = tokio::time::interval(interval);
    trusting.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    trusting.tick().await;

    let mut canarying = tokio::time::interval(interval);
    canarying.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    canarying.tick().await;

    let mut drifting = tokio::time::interval(interval);
    drifting.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    drifting.tick().await;

    // How a spawned resync tells the loop to stop. `Fail` is the only
    // policy that ends the agent, and the task that discovers it is not
    // the one that can return.
    let (gone, mut is_gone) = tokio::sync::watch::channel(false);

    // The resync in flight, if one is. One at a time: a store slower than
    // the interval would otherwise have a second fetch started before the
    // first came back, and then a third.
    let mut resyncing: Option<tokio::task::JoinHandle<()>> = None;

    tokio::pin!(stop);

    loop {
        let arrived = tokio::select! {
            // Sidecar mode never reaches this in production — `main` waits
            // on a future that never completes — and a test needs a way to
            // end a loop whose whole job is not to.
            () = &mut stop => {
                // The one place a lease is handed back. Best-effort with a
                // deadline: a pod that cannot reach the store while it is
                // terminating must still terminate, and Kubernetes is
                // already counting down to SIGKILL.
                if let (Some(held), true) = (lease.clone(), spec.revoke_on_shutdown) {
                    match tokio::time::timeout(spec.revoke_grace, source.revoke(held)).await {
                        Ok(Some(Ok(()))) => {
                            crate::metrics::lease_revoked();
                            info!("the lease was handed back");
                        }
                        Ok(Some(Err(error))) => {
                            warn!(error = %error, "the lease could not be revoked; it will expire on its own");
                        }
                        // A store that issues no leases, which is every
                        // store but one.
                        Ok(None) => {}
                        Err(_) => warn!(
                            "revoking the lease did not finish in time; it will expire on its own"
                        ),
                    }
                }

                return Ok(Ended::Stopped);
            }

            document = delivered.recv() => match document {
                Some(document) => {
                    crate::metrics::delivered();

                    // A delivery is the stream proving itself — the one
                    // event that says the watch is healthy, as opposed to a
                    // resync saying the store is readable.
                    reopening.succeeded();

                    Some(document)
                }
                None => None,
            },

            // Only for a store that pushes. A conditional or interval store
            // is already being asked on this cadence by its own watch, and
            // a second timer would double its load on the store.
            // Ungated, unlike the resync below. A lease's clock is the
            // credential's, not the store's: Vault reports `Conditional`
            // (and `Interval` for a dynamic engine), so gating this the way
            // the resync is gated would mean no renewal ever fired for the
            // one store that issues leases.
            // The document was deleted and the policy says so. Its own
            // arm rather than a flag checked on the next wake: a pod that
            // must not hold a revoked credential should not hold it for
            // another interval.
            Ok(()) = is_gone.changed() => {
                if *is_gone.borrow() {
                    return Err("the document is no longer in the store".into());
                }

                None
            }

            _ = renewal.tick(), if lease.is_some() => {
                let held = lease.clone().expect("guarded above");

                // Whether to try extending at all. Vault says which leases
                // can be — `renewable: false` is what every `pki/issue`
                // answers, and what a database role past its `max_ttl`
                // becomes — and a lease marked that way refuses every
                // renewal it is sent.
                //
                // Asking anyway costs a round trip per cycle per pod, and
                // moves `lease_renewal_failures_total` on a configuration
                // where nothing is wrong. A failure counter that climbs
                // steadily on a healthy fleet is a counter nobody alerts
                // on, which costs more than the round trip.
                let extended = if held.renewable {
                    source.renew(held.clone()).await
                } else {
                    None
                };

                // Whether a new credential has to be minted, and — kept
                // apart on purpose — whether arriving here was a failure.
                // A lease that was never renewable is the first without
                // being the second.
                let reissue = match &extended {
                    Some(Ok(_)) => false,
                    Some(Err(error)) => {
                        crate::metrics::lease_renewal_failed();
                        warn!(error = %error, "the lease could not be renewed; re-fetching");

                        true
                    }
                    // `renew` answers `None` only for a source that holds
                    // no lease at all. The non-renewable path above never
                    // asked it, so here the two are told apart by the flag
                    // rather than by the answer.
                    None => !held.renewable,
                };

                if let Some(Ok(granted)) = extended {
                    crate::metrics::lease_renewed(granted.ttl.as_secs());
                    info!(
                        seconds = granted.ttl.as_secs(),
                        "the lease was extended"
                    );

                    // What the store granted, never what was asked for:
                    // a role's ceiling is not visible from here, so the
                    // next renewal is scheduled from the answer.
                    renewal = tokio::time::interval(renew_after(Some(&granted), &mut pace));
                    renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    renewal.tick().await;

                    lease = Some(granted);

                    // **A renewal is not a render.** Extending a lease
                    // keeps the same credential alive; the document on
                    // disk has not moved, and rewriting it would wake
                    // every application watching the file for no reason.
                    None
                } else if reissue {
                    if !held.renewable {
                        info!(
                            seconds = held.ttl.as_secs(),
                            "the lease cannot be renewed; re-issuing the credential"
                        );
                    }

                    // A credential at the end of its life is not waited out
                    // the way a read is: the only recovery is a new one.
                    // Re-fetching *does* render, because new credentials
                    // are a new document — which is exactly what a renewal
                    // is not.
                    match source.fetch().await {
                        Ok(document) => {
                            lease = document.lease.clone();

                            renewal = tokio::time::interval(
                                renew_after(lease.as_ref(), &mut pace),
                            );
                            renewal.set_missed_tick_behavior(
                                tokio::time::MissedTickBehavior::Delay,
                            );
                            renewal.tick().await;

                            Some(document)
                        }
                        Err(error) => {
                            warn!(error = %error, "the credential could not be re-issued");
                            crate::metrics::failed();

                            None
                        }
                    }
                } else {
                    // Not a leased store at all.
                    None
                }
            }

            // An `interval` rather than a future built fresh each pass.
            // A future that begins with a sleep is **starved** by any
            // faster arm: `select!` drops the losers, so a check that had
            // not finished waiting starts its wait again, and with the
            // drift timer on the same cadence it never got past it. An
            // interval keeps its own schedule across cancellations.
            _ = trusting.tick(), if material.is_some() => {
                let material = material.as_ref().expect("guarded above");

                if !material.settled_change().await {
                    None
                } else {
                    crate::metrics::tls_reloaded();
                    info!(
                        "the store's trust material changed; rebuilding its client without \
                         restarting the pod"
                    );

                    return Ok(Ended::Rebuild);
                }
            }

            // The cohort widening is a change to a mounted file, so it is
            // noticed the same way the trust material's rotation is —
            // and on the same cadence, which is the one this agent
            // already checks everything on.
            _ = canarying.tick(), if withheld.is_some() => {
                let percent = spec.canary.as_ref().and_then(|path| crate::canary::percent(path));

                if crate::canary::admitted(cohort.unwrap_or(0), percent) {
                    let fresh = withheld.take().expect("guarded above");

                    crate::metrics::canary_admitted(percent);
                    info!(
                        percent = percent.unwrap_or(100),
                        "the canary cohort widened to include this pod; publishing what it held"
                    );

                    match publish(spec, &fresh, None) {
                        Ok(()) => rendered = Some(fresh),
                        Err(error) => {
                            crate::metrics::failed();
                            warn!(error = %error, "the held document could not be written");
                        }
                    }
                }

                None
            }

            _ = drifting.tick(), if rendered.is_some() => {
                let published = rendered.as_ref().expect("guarded above");

                match drifted(published) {
                    None => {
                        crate::metrics::undrifted();
                        None
                    }
                    Some(path) => {
                        crate::metrics::drifted();

                        match spec.on_drift {
                            OnDrift::Warn => {
                                warn!(
                                    path = %path.display(),
                                    "the rendered file is not what was rendered; something \
                                     else in this pod has written to it"
                                );

                                None
                            }
                            OnDrift::Repair => {
                                warn!(
                                    path = %path.display(),
                                    "the rendered file is not what was rendered; writing it back"
                                );

                                // Through `publish`, so the meta file and
                                // the notification follow it exactly as
                                // they would for a change from the store.
                                if let Err(error) = publish(spec, published, None) {
                                    warn!(error = %error, "the repair failed");
                                    crate::metrics::failed();
                                } else {
                                    crate::metrics::undrifted();
                                }

                                None
                            }
                            OnDrift::Fail => {
                                warn!(
                                    path = %path.display(),
                                    "the rendered file is not what was rendered; ending the agent"
                                );

                                return Err("the rendered file was written by something else".into());
                            }
                        }
                    }
                }
            }

            _ = resync.tick(), if capability == WatchCapability::Native && resyncing.is_none() => {
                // **Spawned, not awaited here.** An `await` in a `select!`
                // arm body holds the whole loop: one hung `GET` used to
                // stop deliveries, renewals and the shutdown branch along
                // with the resync it belonged to. It becomes another
                // producer into the same slot instead, and the guard above
                // keeps a slow store from stacking one fetch on the next.
                let source = Arc::clone(source);
                let deliver = deliver.clone();
                let outputs = outputs.clone();
                let gone = gone.clone();
                let reporter = reporter.clone();

                resyncing = Some(tokio::spawn(async move {
                    // Counted *after* the store answers. Marking the resync
                    // before the fetch moved the staleness clock on every
                    // attempt, so a store that had been unreachable for an
                    // hour reported a document read seconds ago — the gauge
                    // an operator pages on, saying the opposite of what
                    // happened.
                    match source.fetch().await {
                        Ok(document) => {
                            crate::metrics::resynced();

                            // Through the slot, so a resync and a push race
                            // the way two pushes do: latest wins.
                            let _ = deliver.send(document);
                        }
                        Err(error) if error.kind() == dynamic_config::ErrorKind::Absent => {
                            // **Not an outage.** The store answered and the
                            // document is gone. Waiting will not bring it
                            // back, which is exactly why this is told apart
                            // from a store that did not answer — before, a
                            // deleted secret and an unreachable Vault
                            // produced the same line and the same silence.
                            crate::metrics::absent();

                            // The condition `kubectl describe pod` had no
                            // way of showing: a deleted document used to be
                            // a log line and a gauge, both a step away from
                            // where somebody was looking.
                            report(
                                reporter.as_ref(),
                                "DocumentAbsent",
                                &error.to_string(),
                            );

                            match on_delete {
                                OnDelete::Retain => warn!(
                                    error = %error,
                                    "the document is no longer in the store; the last good \
                                     file keeps serving"
                                ),
                                OnDelete::Remove => {
                                    warn!(
                                        error = %error,
                                        "the document is no longer in the store; emptying \
                                         the rendered files"
                                    );

                                    for out in &outputs {
                                        if let Err(error) =
                                            crate::render::write_atomically(out, "", None)
                                        {
                                            warn!(error = %error, path = %out.display(), "could not empty it");
                                        }
                                    }
                                }
                                OnDelete::Fail => {
                                    warn!(
                                        error = %error,
                                        "the document is no longer in the store; ending the agent"
                                    );

                                    let _ = gone.send(true);
                                }
                            }
                        }
                        Err(error) => {
                            warn!(error = %error, "the resync failed; the rendered file is unchanged");
                            crate::metrics::failed();
                        }
                    }
                }));

                None
            }

            // Reaped so the guard above can let the next one start. Nothing
            // is read from it: whatever it fetched has already gone through
            // the slot.
            Some(()) = async {
                match &mut resyncing {
                    Some(handle) => handle.await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                resyncing = None;

                None
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

                // Recorded *before* the wait is drawn. The other order made
                // the backoff lag by one round: the first reopen after a
                // drop always waited the healthy interval, however long the
                // stream had been failing.
                reopening.failed();
                tokio::time::sleep(reopening.next_wait()).await;

                watcher = spawn_watch(source, &handle, interval, deliver.clone());
                continue;
            }
        };

        let Some(document) = arrived else {
            continue;
        };

        pace.succeeded();

        match crate::render::render_all(&document, spec) {
            Ok(fresh) => {
                // The whole set, not the first file: two renderings of one
                // document can move independently — a change in the `db`
                // section leaves the `cache` file identical — and writing
                // the set only when *something* in it moved keeps a
                // no-op change from touching every file's mtime.
                if unchanged(&rendered, &fresh) {
                    info!("unchanged");
                    continue;
                }

                // Outside the cohort: keep what is being served and keep
                // the new set, so widening the percentage publishes it
                // without the store having to say anything again.
                if let (Some(cohort), Some(path)) = (cohort, spec.canary.as_ref()) {
                    let percent = crate::canary::percent(path);

                    if !crate::canary::admitted(cohort, percent) {
                        crate::metrics::canary_holding(percent);
                        info!(
                            cohort,
                            percent = percent.unwrap_or(100),
                            "a new document is held: this pod is outside the canary cohort"
                        );

                        withheld = Some(fresh);
                        continue;
                    }
                }

                // Warned about rather than fatal, like every other runtime
                // failure here. This was the one that ended the process —
                // so a transient `ENOSPC` on the tmpfs killed a sidecar
                // that was holding a perfectly good file, while an
                // unreachable store beside it was merely logged.
                // Before the write and not after it. On a node plugin
                // this loop is publishing one pod's file and telling the
                // others about the same document, and the two must not be
                // chained: a pod that has been deleted takes its volume
                // directory with it, so the write below starts failing
                // while the other pods on this node are still reading
                // perfectly well. Hanging their delivery on this pod's
                // success let one departure silence everyone.
                if let Some(hear) = on_document {
                    hear(&document);
                }

                match publish(spec, &fresh, document.revision.as_ref()) {
                    Ok(()) => rendered = Some(fresh),
                    Err(error) => {
                        crate::metrics::failed();
                        warn!(error = %error, "the write failed; the rendered file is unchanged");
                    }
                }
            }
            Err(error) => {
                // The file keeps its last good content — the same
                // keep-serving rule the engine applies in-process.
                crate::metrics::failed();
                warn!(error = %error, "rendering failed; the rendered file is unchanged");

                // `kubectl describe pod` is where somebody looks first,
                // and until this the answer was one `kubectl logs -c` away
                // from the question.
                report(reporter.as_ref(), "RenderFailed", &error.to_string());
            }
        }
    }
}

/// Publishes a rendered document, and the meta file beside it if one was
/// asked for.
///
/// One function because the two must not drift: a meta file describing a
/// document that was not written is worse than none, so the document goes
/// first and the meta only follows a write that worked.
fn publish(
    spec: &Spec,
    rendered: &[crate::render::Published],
    revision: Option<&dynamic_config::Revision>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Everything in `rendered` has already resolved, validated and
    // rendered — `render_all` refuses the whole set if any of it failed.
    // What is left here is the writing, and the writing is the part that
    // cannot be made atomic across files: each rename is, the set is not.
    // A reader can in principle catch the microseconds between two of
    // them, which is a rename apart rather than a fetch apart.
    for file in rendered {
        // Before the rename that destroys it, and never fatal: a history
        // that could not be written is a diagnostic that is missing, not a
        // render that failed.
        if let Err(error) = crate::render::keep_previous(&file.out, spec.history) {
            warn!(error = %error, path = %file.out.display(), "the previous generation was not kept");
        }

        crate::render::write_atomically(&file.out, &file.document, file.file_mode)?;
    }

    crate::metrics::rendered();
    crate::metrics::generation(revision);

    // What an acknowledgement has to match. The main render's digest, not
    // the set's: an application acknowledges the document it read, and the
    // document it read is the one at `--out`.
    if let Some(main) = rendered.first() {
        crate::metrics::published(&crate::render::digest(&main.document));
    }

    // After the renames and never before: the whole promise of the
    // notification is that the document is already there when it arrives.
    if let Some(endpoint) = &spec.notify_http {
        notify(endpoint);
    }

    if spec.meta {
        for file in rendered {
            let digest = crate::render::digest(&file.document);

            // A failure here does not fail the render: the document is
            // already in place and correct, and an application reading it
            // does not need the description of it to exist.
            if let Err(error) =
                crate::render::write_meta(&file.out, revision, &digest, file.file_mode)
            {
                warn!(error = %error, path = %file.out.display(), "the meta file could not be written");
            }
        }
    }

    info!(
        files = rendered.len(),
        bytes = rendered
            .iter()
            .map(|file| file.document.len())
            .sum::<usize>(),
        "rendered"
    );

    Ok(())
}

/// How long to wait before handing the lease back is *not* how long to
/// wait before giving up on handing it back.
///
/// Kubernetes is already counting down to SIGKILL when this runs — the
/// grace period is thirty seconds by default — so a revocation that cannot
/// finish quickly is one the lease's own expiry will have to cover.
/// The default `--revoke-grace`, when a pod names none.
pub const REVOKE_DEADLINE: Duration = Duration::from_secs(5);

/// The fraction of a lease's life to renew at.
///
/// Two thirds, near enough: early enough that a failure leaves room for a
/// re-fetch before the credential dies, late enough that a short lease is
/// not renewed continuously. The reference implementation uses the same
/// fraction for the same reason.
const RENEW_AT: f64 = 0.65;

/// The fraction of a **non-renewable** lease's life to re-fetch at.
///
/// Later than [`RENEW_AT`], because the two are doing different things. A
/// renewal is cheap and reversible — it fails, and there is still a third
/// of the lease left to get a new credential in. A re-fetch *is* the new
/// credential, so running it early only shortens the life of the one in
/// use: the same secret is minted more often, and every application
/// watching the file is woken for it.
///
/// Ninety per cent leaves a tenth of the lease as the margin for the fetch
/// itself, its retries, and the render. The reference implementation uses
/// the same threshold for the same class of lease.
const REFETCH_AT: f64 = 0.9;

/// When the next renewal should fire.
///
/// Spread through the pace already in hand rather than a second jitter
/// policy: a thousand pods that started together hold a thousand leases
/// that expire together, and renewing them in lockstep is the same
/// thundering herd the poll interval already avoids.
///
/// A store with no lease gets a long wait, since the branch is gated off
/// anyway — the number only has to be a number.
fn renew_after(lease: Option<&Lease>, pace: &mut Pace) -> Duration {
    let Some(lease) = lease else {
        return Duration::from_secs(3600);
    };

    // Two fractions, because a lease that cannot be renewed is on a
    // different clock. See `REFETCH_AT`.
    let fraction = if lease.renewable {
        RENEW_AT
    } else {
        REFETCH_AT
    };

    // Through the whole `Duration` rather than its seconds: `as_secs()`
    // truncates, so every lease shorter than about two seconds renewed on
    // the same floor whatever it had actually been granted.
    let at = lease.ttl.mul_f64(fraction);

    // Never zero: a lease with no life left would spin the loop.
    pace.spread(at.max(Duration::from_millis(1)))
}

/// The set as it currently sits on disk, with `main` already read.
///
/// Used only on the recovery path: it is what the process is *serving*
/// rather than what it rendered, which is exactly the thing the loop's
/// unchanged-check has to compare against so a first successful fetch
/// rewrites the files rather than deciding they already match.
fn on_disk(spec: &Spec, main: String) -> Vec<crate::render::Published> {
    let mut held = vec![crate::render::Published {
        out: spec.out.clone(),
        document: main,
        file_mode: spec.file_mode,
    }];

    for rendering in &spec.also {
        held.push(crate::render::Published {
            out: rendering.out.clone(),
            // Absent reads as empty, so the next successful render is a
            // change and writes it.
            document: std::fs::read_to_string(&rendering.out).unwrap_or_default(),
            file_mode: rendering.file_mode,
        });
    }

    held
}

/// The first published file whose contents are no longer what was written.
///
/// `None` when every one of them still matches. Reads the files rather than
/// stat-ing them: an mtime says something touched the file, and the
/// question here is whether the bytes moved.
fn drifted(published: &[crate::render::Published]) -> Option<std::path::PathBuf> {
    published.iter().find_map(|file| {
        match std::fs::read_to_string(&file.out) {
            Ok(text) if text == file.document => None,
            // Unreadable counts as drifted, and deliberately: a rendered
            // file that has become a directory, or that somebody deleted,
            // is not the file this agent wrote either.
            _ => Some(file.out.clone()),
        }
    })
}

/// Whether a freshly rendered set is the one already on disk.
///
/// Compared as a whole, because two renderings of one document move
/// independently: a change in the `db` section leaves a `cache` file
/// byte-identical, and rewriting it would touch an mtime every watcher in
/// the pod is looking at.
fn unchanged(
    held: &Option<Vec<crate::render::Published>>,
    fresh: &[crate::render::Published],
) -> bool {
    let Some(held) = held else {
        return false;
    };

    held.len() == fresh.len()
        && held
            .iter()
            .zip(fresh)
            .all(|(held, fresh)| held.out == fresh.out && held.document == fresh.document)
}

/// What is on disk already, when the first fetch failed.
///
/// The policy decides; this is only where it is applied. Returns the
/// document to go on serving, or the original failure when there is nothing
/// to serve and nothing that says starting anyway is acceptable.
fn recovered(
    spec: &Spec,
    error: &dyn std::fmt::Display,
) -> Result<Option<Vec<crate::render::Published>>, Box<dyn std::error::Error>> {
    use crate::spec::StartupPolicy;

    match spec.startup_policy {
        StartupPolicy::RequireFresh => {
            warn!(error = %error, "the first fetch failed and this agent requires a fresh document");

            Err(format!("the first fetch failed: {error}").into())
        }

        StartupPolicy::AllowCached => match std::fs::read_to_string(&spec.out) {
            Ok(cached) if !cached.trim().is_empty() => {
                let held = on_disk(spec, cached.clone());

                // Loud, because this is the one state where the file on
                // disk and the store disagree and nothing here knows by how
                // much. `staleness_seconds` never moves until a fetch
                // succeeds, which is exactly the signal an operator needs.
                warn!(
                    error = %error,
                    bytes = cached.len(),
                    path = %spec.out.display(),
                    "the first fetch failed; serving the document already on disk"
                );
                crate::metrics::failed();

                Ok(Some(held))
            }
            // Nothing cached, so there is nothing to allow: this is the
            // original behaviour, and the original reason for it.
            _ => {
                warn!(error = %error, "the first fetch failed and nothing is cached");

                Err(format!("the first fetch failed: {error}").into())
            }
        },

        StartupPolicy::BestEffort => {
            warn!(
                error = %error,
                "the first fetch failed; starting anyway, and the application \
                 will read an empty configuration"
            );
            crate::metrics::failed();

            let existing = std::fs::read_to_string(&spec.out).unwrap_or_default();

            // Every configured output, not only the first: a pod that
            // starts on nothing must find *all* of its files there, or the
            // application opens one that does not exist.
            if existing.is_empty() {
                crate::render::write_atomically(&spec.out, "", spec.file_mode)?;
            }

            for rendering in &spec.also {
                if std::fs::read_to_string(&rendering.out).is_err() {
                    crate::render::write_atomically(&rendering.out, "", rendering.file_mode)?;
                }
            }

            Ok(Some(on_disk(spec, existing)))
        }
    }
}

/// One run of the store's watch, on a task of its own.
fn spawn_watch(
    source: &Arc<Built>,
    handle: &RemoteWatch,
    interval: Duration,
    deliver: crate::sources::Deliver,
) -> tokio::task::JoinHandle<Result<(), Error>> {
    let source = Arc::clone(source);
    let watching = handle.watching();

    crate::metrics::watch_up();

    tokio::spawn(async move { source.watch(watching, interval, deliver).await })
}

/// Tells the application, without making the render wait for it.
///
/// Spawned rather than awaited: the file is published, the loop's next
/// delivery must not queue behind an application that is slow to answer,
/// and a notification that fails has undone nothing. Everything it can do
/// is a log line and a counter.
fn notify(endpoint: &crate::notify::Endpoint) {
    let endpoint = endpoint.clone();

    tokio::spawn(async move {
        match endpoint.call().await {
            Ok(status) if (200..400).contains(&status) => {
                crate::metrics::notified();
                info!(status, "the application was told");
            }
            Ok(status) => {
                crate::metrics::notification_failed();
                warn!(status, "the reload endpoint refused the notification");
            }
            Err(error) => {
                crate::metrics::notification_failed();
                warn!(error = %error, "the reload endpoint could not be told");
            }
        }
    });
}

/// Writes an Event, off the loop and never in its way.
///
/// Spawned: an Event is commentary on work that has already happened or
/// already failed, and a render must not wait on the API server to hear
/// about it. A failure here is logged once and dropped.
fn report(reporter: Option<&crate::events::Reporter>, reason: &str, message: &str) {
    let Some(reporter) = reporter.cloned() else {
        return;
    };

    let reason = reason.to_owned();
    let message = message.to_owned();

    tokio::task::spawn_blocking(move || {
        if let Err(error) = reporter.warn(&reason, &message) {
            warn!(%error, "the Kubernetes Event could not be written");
        }
    });
}
