//! Leader election over a Lease, so replicas are spare capacity rather
//! than duplicate work.
//!
//! Two replicas reconciling the same Render write the same ConfigMap from
//! two directions: harmless on a good day, and on a bad one two different
//! documents landing in whichever order the API server saw them. So one
//! leads and the rest wait, and a leader that dies is replaced within the
//! lease's term rather than by somebody noticing.
//!
//! **Not shared with the webhook's rotation lease**, which looks similar
//! and is a different shape: that one is held for the length of one
//! rotation and released, this one is held for as long as the process
//! reconciles. A common abstraction over the two would have to carry both
//! lifetimes and would be longer than either.
//!
//! The identity is the pod's name, which Kubernetes already guarantees is
//! unique in a namespace — so two replicas cannot believe they are the
//! same holder, and a restarted pod does not inherit its predecessor's
//! term.

use std::time::Duration;

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::api::{ObjectMeta, PostParams};
use kube::{Api, Client};

/// How long a term lasts. A leader that stops renewing is replaced this
/// long after its last renewal.
const TERM: Duration = Duration::from_secs(15);

/// How often the leader renews. A third of the term, so two renewals can
/// be lost to a slow API server without the lease expiring under a leader
/// that is perfectly healthy.
const RENEW: Duration = Duration::from_secs(5);

/// How long a follower waits before asking again.
const POLL: Duration = Duration::from_secs(5);

/// The name every replica of this operator contends for.
const LEASE: &str = "dynamic-config-operator";

/// Blocks until this replica leads, then keeps renewing in the background.
///
/// The returned future resolves when the term is **lost** — an API server
/// that refuses the renewal, or another holder that took over while this
/// one was not looking. The caller's answer to that is to stop: a process
/// that has lost the lease and carries on reconciling is the thing the
/// lease exists to prevent.
///
/// # Errors
///
/// If the API server cannot be reached at all.
pub async fn lead(
    client: &Client,
    namespace: &str,
    identity: &str,
) -> Result<impl std::future::Future<Output = ()>, kube::Error> {
    let leases: Api<Lease> = Api::namespaced(client.clone(), namespace);

    while !claim(&leases, identity).await? {
        tracing::info!(lease = LEASE, "another replica leads; waiting");
        tokio::time::sleep(POLL).await;
    }

    tracing::info!(lease = LEASE, %identity, "leading");

    let leases = leases.clone();
    let identity = identity.to_owned();

    Ok(async move {
        let mut held = tokio::time::Instant::now();

        loop {
            tokio::time::sleep(RENEW).await;

            match claim(&leases, &identity).await {
                Ok(true) => held = tokio::time::Instant::now(),
                Ok(false) => {
                    tracing::warn!(lease = LEASE, "the lease was taken by another replica");

                    return;
                }
                Err(error) => {
                    // Not fatal on its own: a renewal that fails once has
                    // two more attempts before the term runs out, and an
                    // operator that gave up on one API blip would hand the
                    // cluster a leaderless gap for no reason.
                    tracing::warn!(%error, lease = LEASE, "renewing the lease failed");

                    // Fatal once the *term* has gone by without one
                    // succeeding, though — which is the moment another
                    // replica may take the lease, and this one carrying on
                    // is two operators reconciling the same objects. The
                    // documentation above says this future resolves when
                    // the term is lost; without this it only resolved when
                    // somebody else announced they had taken it.
                    if held.elapsed() >= TERM {
                        tracing::warn!(
                            lease = LEASE,
                            "no renewal succeeded within the term; standing down"
                        );

                        return;
                    }
                }
            }
        }
    })
}

/// Takes or renews the lease. `true` means this replica holds it.
async fn claim(leases: &Api<Lease>, identity: &str) -> Result<bool, kube::Error> {
    let now = MicroTime(chrono::Utc::now());

    // **`acquireTime` is when leadership was taken; `renewTime` is when it
    // was last confirmed.** Writing both on every renewal made the first
    // one unanswerable — `kubectl describe lease` could not say when the
    // leader last changed, and `leaseTransitions` stayed at whatever it
    // started as. So a renewal carries the acquire time it found, and a
    // takeover sets a new one and counts itself.
    let mine =
        |version: Option<String>, acquired: Option<MicroTime>, transitions: Option<i32>| Lease {
            metadata: ObjectMeta {
                name: Some(LEASE.to_owned()),
                resource_version: version,
                ..Default::default()
            },
            spec: Some(LeaseSpec {
                holder_identity: Some(identity.to_owned()),
                lease_duration_seconds: Some(i32::try_from(TERM.as_secs()).unwrap_or(i32::MAX)),
                renew_time: Some(now.clone()),
                acquire_time: Some(acquired.unwrap_or_else(|| now.clone())),
                lease_transitions: transitions,
                ..Default::default()
            }),
        };

    let Some(current) = leases.get_opt(LEASE).await? else {
        // The first holder: acquired now, and the first transition.
        return match leases
            .create(&PostParams::default(), &mine(None, None, Some(0)))
            .await
        {
            Ok(_) => Ok(true),
            // Another replica created it in the same instant; it leads.
            Err(kube::Error::Api(response)) if response.code == 409 => Ok(false),
            Err(error) => Err(error),
        };
    };

    let spec = current.spec.unwrap_or_default();
    let held_by_me = spec.holder_identity.as_deref() == Some(identity);
    let expired = spec.renew_time.as_ref().is_none_or(|renewed| {
        let since = chrono::Utc::now() - renewed.0;

        since.num_seconds()
            > i64::from(
                spec.lease_duration_seconds
                    .unwrap_or_else(|| i32::try_from(TERM.as_secs()).unwrap_or(i32::MAX)),
            )
    });

    if !held_by_me && !expired {
        return Ok(false);
    }

    // The resource version is the fence: two replicas that both saw an
    // expired lease cannot both write over it, because the second write
    // is refused for being based on a version that has moved.
    // Renewing keeps what is there; taking over from an expired holder
    // stamps a fresh acquisition and counts the transition.
    let (acquired, transitions) = if held_by_me {
        (spec.acquire_time.clone(), spec.lease_transitions)
    } else {
        (None, Some(spec.lease_transitions.unwrap_or(0) + 1))
    };

    match leases
        .replace(
            LEASE,
            &PostParams::default(),
            &mine(current.metadata.resource_version, acquired, transitions),
        )
        .await
    {
        Ok(_) => Ok(true),
        Err(kube::Error::Api(response)) if response.code == 409 => Ok(false),
        Err(error) => Err(error),
    }
}
