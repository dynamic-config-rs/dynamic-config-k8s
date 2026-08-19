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
    loop {
        if let Err(error) = attend(&settings).await {
            warn!(%error, "self-rotation attempt failed; retrying");
        }

        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

async fn attend(settings: &Settings) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::try_default().await?;

    if !hold_lease(&client, settings).await? {
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

    rotate(&client, settings).await
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
fn mint(settings: &Settings) -> Result<Minted, Box<dyn std::error::Error + Send + Sync>> {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "dynamic-config-webhook-ca");

    let ca_key = rcgen::KeyPair::generate()?;
    let ca = ca_params.self_signed(&ca_key)?;

    let service = &settings.service;
    let namespace = &settings.namespace;
    let leaf_params = rcgen::CertificateParams::new(vec![
        format!("{service}.{namespace}.svc"),
        format!("{service}.{namespace}.svc.cluster.local"),
        service.clone(),
    ])?;

    let leaf_key = rcgen::KeyPair::generate()?;
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key)?;

    Ok(Minted {
        ca_pem: ca.pem(),
        leaf_pem: leaf.pem(),
        key_pem: leaf_key.serialize_pem(),
    })
}
