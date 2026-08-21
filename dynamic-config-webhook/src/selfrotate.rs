//! The third TLS mode: the webhook is its own certificate authority.
//!
//! `certManager` renews but is a dependency; the chart's `genCA` is
//! zero-dependency but never rotates. This mode is both halves: a CA
//! and leaf minted **in memory** at rotation time, written to the
//! chart's own Secret (which is mounted at `/tls`, so the existing
//! file hot-reload in `tls.rs` picks the pair up on every replica),
//! and the MutatingWebhookConfiguration's `caBundle` patched by the
//! webhook itself — the Vault-agent-injector shape.
//!
//! The RBAC this costs is narrow and stated in the chart beside
//! `failurePolicy`: get/patch on ONE webhook configuration and
//! get/create/update on ONE secret, both scoped by `resourceNames`,
//! plus leases in the release namespace — the deliberate purity trade.
//!
//! Rotation is leader-elected over a Lease: one replica rotates, every
//! replica serves whatever the Secret currently holds. Rotation
//! happens at two thirds of the pair's validity, jittered ±10% so a
//! fleet restarted together does not rotate together.

use std::time::Duration;

use base64::Engine as _;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::core::DynamicObject;
use kube::discovery::ApiResource;
use kube::Client;
use tracing::{info, warn};

/// How long each minted pair lives, by default. Short on purpose:
/// rotation is the feature, and a rotation nobody notices for a day is
/// the proof. The soak overrides it downward through the environment.
fn validity() -> Duration {
    std::env::var("DYNAMIC_CONFIG_WEBHOOK_VALIDITY_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(24 * 60 * 60))
}

/// The lease's own term; renewed at half this.
const LEASE_SECONDS: i32 = 30;

/// How far back a minted certificate's validity starts.
///
/// A node whose clock is a minute behind the minting pod would otherwise
/// refuse a certificate for not being valid yet — which looks exactly like
/// a broken rotation and is a wrong clock.
const SKEW: time::Duration = time::Duration::minutes(5);

/// Rotations that completed, and when the current pair expires.
///
/// The pair a scrape wants: a counter that should climb on a schedule,
/// and a wall-clock second that should always be in the future. An expiry
/// that stops moving while the counter stops climbing is a rotation that
/// has quietly stopped, which is the failure this mode has to make
/// visible — a webhook whose certificate expires takes every pod creation
/// in the cluster with it.
pub static ROTATIONS_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static EXPIRES_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Clone)]
pub struct Settings {
    pub namespace: String,
    pub service: String,
    pub secret: String,
    pub webhook_configuration: String,
    pub identity: String,
}

impl Settings {
    /// `None` unless the chart asked for this mode.
    pub fn from_environment() -> Option<Self> {
        if std::env::var("DYNAMIC_CONFIG_WEBHOOK_SELF_ROTATE").as_deref() != Ok("1") {
            return None;
        }

        let var = |name: &str| std::env::var(name).ok();

        Some(Self {
            namespace: var("DYNAMIC_CONFIG_WEBHOOK_NAMESPACE")?,
            service: var("DYNAMIC_CONFIG_WEBHOOK_SERVICE")?,
            secret: var("DYNAMIC_CONFIG_WEBHOOK_TLS_SECRET")?,
            webhook_configuration: var("DYNAMIC_CONFIG_WEBHOOK_MWC")?,
            identity: var("HOSTNAME").unwrap_or_else(|| "dynamic-config-webhook".to_owned()),
        })
    }
}

/// The rotation loop, spawned beside the server. Never returns.
pub async fn run(settings: Settings) {
    // One client for the life of the process. A fresh one every fifteen
    // seconds re-read the service account token, rebuilt the TLS config
    // and opened a new connection to the API server — four times a minute,
    // for a loop whose usual answer is "not yet".
    let client = loop {
        match Client::try_default().await {
            Ok(client) => break client,
            Err(error) => {
                warn!(%error, "no kube client for self-rotation; retrying");
                tokio::time::sleep(Duration::from_secs(15)).await;
            }
        }
    };

    loop {
        if let Err(error) = attend(&client, &settings).await {
            warn!(%error, "self-rotation attempt failed; retrying");
        }

        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

async fn attend(
    client: &Client,
    settings: &Settings,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !hold_lease(client, settings).await? {
        return Ok(()); // another replica rotates; this one only serves
    }

    let secrets: Api<Secret> = Api::namespaced(client.clone(), &settings.namespace);

    if let Some(remaining) = remaining_validity(&secrets, settings).await? {
        // Rotate at two thirds spent, jittered so replicas that traded
        // leadership do not converge on one instant.
        let jitter = 0.9 + (rand::random::<f64>() * 0.2);
        let threshold = validity().as_secs_f64() / 3.0 * jitter;

        if remaining.as_secs_f64() > threshold {
            return Ok(());
        }
    }

    // The lease is thirty seconds and a rotation is a mint plus two API
    // patches — which is fast until the API server is not. Renewing
    // underneath it means a slow rotation cannot have its lease taken by
    // a replica that then rotates on top of it.
    // **A lost lease stops the rotation, rather than being logged past.**
    // The renewal answers three ways and each means something different: it
    // still leads, somebody else took it, or the API server could not be
    // asked. Only the first is a reason to keep going. Two replicas
    // rotating at once write one CA's leaf into the Secret and another's
    // bundle into the webhook configuration, and every admission TLS
    // handshake fails until somebody notices — which is the failure the
    // lease exists to prevent.
    let (lost, taken) = tokio::sync::oneshot::channel();

    let renewing = tokio::spawn({
        let client = client.clone();
        let settings = settings.clone();

        async move {
            // A renewal that *fails* is not a lease that was lost: an API
            // server refusing for a moment is ordinary, and giving up on
            // the first one would make a rotation fail on a hiccup. It
            // becomes a loss when nothing has succeeded for a whole term,
            // which is exactly when another replica may take it.
            let mut held = tokio::time::Instant::now();
            let term = Duration::from_secs(u64::try_from(LEASE_SECONDS).unwrap_or(30));

            loop {
                tokio::time::sleep(term / 3).await;

                match hold_lease(&client, &settings).await {
                    Ok(true) => held = tokio::time::Instant::now(),
                    Ok(false) => {
                        warn!("the rotation lease was taken by another replica");

                        let _ = lost.send(());

                        return;
                    }
                    Err(error) => {
                        warn!(%error, "renewing the rotation lease failed");

                        if held.elapsed() >= term {
                            warn!("no renewal succeeded within the lease's term");

                            let _ = lost.send(());

                            return;
                        }
                    }
                }
            }
        }
    });

    // Dropping the rotation is what abandons it: the writes are `await`
    // points, so cancellation lands between them and nothing half-written
    // reaches the Secret or the webhook configuration.
    let rotated = tokio::select! {
        result = rotate(client, settings) => result,
        _ = taken => {
            warn!("the rotation lease was lost mid-rotation; abandoning it unwritten");

            Ok(())
        }
    };

    renewing.abort();
    rotated
}

/// Acquire or renew the rotation lease. `true` means this replica leads.
async fn hold_lease(
    client: &Client,
    settings: &Settings,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let leases: Api<Lease> = Api::namespaced(client.clone(), &settings.namespace);
    let name = "dynamic-config-webhook-rotation";
    let now = MicroTime(chrono::Utc::now());

    let desired = |holder: &str| Lease {
        metadata: kube::api::ObjectMeta {
            name: Some(name.to_owned()),
            ..Default::default()
        },
        spec: Some(k8s_openapi::api::coordination::v1::LeaseSpec {
            holder_identity: Some(holder.to_owned()),
            lease_duration_seconds: Some(LEASE_SECONDS),
            renew_time: Some(now.clone()),
            ..Default::default()
        }),
    };

    match leases.get_opt(name).await? {
        None => match leases
            .create(&PostParams::default(), &desired(&settings.identity))
            .await
        {
            Ok(_) => Ok(true),
            // Two replicas raced the creation; the winner leads.
            Err(kube::Error::Api(response)) if response.code == 409 => Ok(false),
            Err(error) => Err(error.into()),
        },
        Some(current) => {
            let spec = current.spec.unwrap_or_default();
            let held_by_me = spec.holder_identity.as_deref() == Some(&settings.identity);
            let expired = spec
                .renew_time
                .as_ref()
                .map(|renewed| {
                    let age = chrono::Utc::now() - renewed.0;
                    age.num_seconds()
                        > i64::from(spec.lease_duration_seconds.unwrap_or(LEASE_SECONDS))
                })
                .unwrap_or(true);

            if held_by_me || expired {
                leases
                    .replace(name, &PostParams::default(), &{
                        let mut lease = desired(&settings.identity);
                        lease.metadata.resource_version = current.metadata.resource_version;
                        lease
                    })
                    .await?;

                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

/// How long the pair in the Secret is still good for; `None` when the
/// Secret is absent or unreadable — which means rotate now.
async fn remaining_validity(
    secrets: &Api<Secret>,
    settings: &Settings,
) -> Result<Option<Duration>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(secret) = secrets.get_opt(&settings.secret).await? else {
        return Ok(None);
    };

    let Some(expiry) = secret
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("dynamic-config.rs/not-after"))
        .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
    else {
        return Ok(None);
    };

    let remaining = expiry.signed_duration_since(chrono::Utc::now());

    Ok(remaining.to_std().ok())
}

/// Mint, write, patch: the actual rotation.
async fn rotate(
    client: &Client,
    settings: &Settings,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let minted = mint(settings)?;

    let secrets: Api<Secret> = Api::namespaced(client.clone(), &settings.namespace);

    // The bundle carries the NEW CA and the PREVIOUS one, together. The
    // caBundle patch lands instantly; the new leaf reaches the serving
    // processes only when the kubelet syncs the Secret — up to a minute
    // later — and for that whole window the API server would otherwise
    // trust only a CA nobody serves yet. The first soak run measured
    // exactly that: one refused admission per rotation. A two-CA window
    // closes it: the old leaf verifies against the old CA until every
    // replica has the new pair, and the next rotation prunes the old CA
    // — at which point nothing has served its leaf for a full interval.
    let previous_ca = secrets
        .get_opt(&settings.secret)
        .await?
        .and_then(|secret| secret.data)
        .and_then(|data| data.get("ca.crt").cloned())
        .map(|bytes| String::from_utf8_lossy(&bytes.0).into_owned())
        .unwrap_or_default();
    // Only the current CA rides ca.crt (a fetch client should trust the
    // serving CA); the transition window lives in the caBundle alone.
    let bundle = format!("{}{}", minted.ca_pem, first_certificate(&previous_ca));
    let not_after =
        (chrono::Utc::now() + chrono::Duration::from_std(validity()).expect("fits")).to_rfc3339();

    let secret = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": settings.secret,
            "annotations": { "dynamic-config.rs/not-after": not_after },
        },
        "type": "kubernetes.io/tls",
        "stringData": {
            "tls.crt": minted.leaf_pem,
            "tls.key": minted.key_pem,
            "ca.crt": minted.ca_pem,
        },
    });

    secrets
        .patch(
            &settings.secret,
            &PatchParams::apply("dynamic-config-webhook").force(),
            &Patch::Apply(&secret),
        )
        .await?;

    // Then the MWC: every webhook entry's caBundle, one JSON patch each.
    let resource = ApiResource::erase::<
        k8s_openapi::api::admissionregistration::v1::MutatingWebhookConfiguration,
    >(&());
    let configurations: Api<DynamicObject> = Api::all_with(client.clone(), &resource);
    let current = configurations.get(&settings.webhook_configuration).await?;
    let entries = current.data["webhooks"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);

    let bundle = base64::engine::general_purpose::STANDARD.encode(&bundle);
    let operations: Vec<serde_json::Value> = (0..entries)
        .map(|index| {
            serde_json::json!({
                "op": "replace",
                "path": format!("/webhooks/{index}/clientConfig/caBundle"),
                "value": bundle,
            })
        })
        .collect();

    let patch: json_patch::Patch = serde_json::from_value(serde_json::Value::Array(operations))?;

    configurations
        .patch(
            &settings.webhook_configuration,
            &PatchParams::default(),
            &Patch::Json::<()>(patch),
        )
        .await?;

    ROTATIONS_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    EXPIRES_AT.store(
        u64::try_from(
            (chrono::Utc::now() + chrono::Duration::from_std(validity()).expect("fits"))
                .timestamp(),
        )
        .unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );

    info!(
        secret = %settings.secret,
        configuration = %settings.webhook_configuration,
        entries,
        "rotated: new CA and leaf minted, secret written, caBundle patched"
    );

    Ok(())
}

/// The first PEM block of `text` — the CURRENT CA of the pair being
/// replaced. Taking one block (not the whole file) is what makes the
/// window exactly two CAs wide instead of growing forever.
fn first_certificate(text: &str) -> &str {
    const END: &str = "-----END CERTIFICATE-----";

    match text.find(END) {
        Some(index) => &text[..index + END.len()],
        None => "",
    }
}

struct Minted {
    ca_pem: String,
    leaf_pem: String,
    key_pem: String,
}

/// A CA and a leaf for the service's DNS names, minted in memory.
///
/// **Both carry an explicit lifetime.** Without one the certificate's own
/// validity is whatever the library defaults to — years — while the
/// rotation schedule comes from an annotation this process writes to
/// itself. A pair replaced every day but valid for four years is not a
/// short-lived credential; it is a long-lived one that happens to be
/// replaced often, and a copy taken from a node stays good long after the
/// rotation that was supposed to retire it.
///
/// **The CA outlives its leaf, on purpose.** The caBundle carries the new
/// CA and the previous one for a whole interval, so that leaves already
/// serving on other replicas keep verifying while the kubelet catches up.
/// A CA that expired with its leaf would leave that window trusting a
/// certificate authority that is no longer valid — the transition would
/// break at exactly the moment it exists to cover.
fn mint(settings: &Settings) -> Result<Minted, Box<dyn std::error::Error + Send + Sync>> {
    let leaf_life = time::Duration::try_from(validity())?;
    let starts = time::OffsetDateTime::now_utc() - SKEW;

    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "dynamic-config-webhook-ca");
    ca_params.not_before = starts;
    ca_params.not_after = starts + leaf_life * 2;

    let ca_key = rcgen::KeyPair::generate()?;
    let ca = ca_params.self_signed(&ca_key)?;

    let service = &settings.service;
    let namespace = &settings.namespace;
    let mut leaf_params = rcgen::CertificateParams::new(vec![
        format!("{service}.{namespace}.svc"),
        format!("{service}.{namespace}.svc.cluster.local"),
        service.clone(),
    ])?;

    leaf_params.not_before = starts;
    leaf_params.not_after = starts + leaf_life;

    let leaf_key = rcgen::KeyPair::generate()?;
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key)?;

    Ok(Minted {
        ca_pem: ca.pem(),
        leaf_pem: leaf.pem(),
        key_pem: leaf_key.serialize_pem(),
    })
}
