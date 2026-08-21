//! The two custom resources, as Rust types — the schema is generated
//! from these, so the manifest in `deploy/` and the reconciler always
//! agree.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A named store-and-auth bundle. Pods reference it by name instead of
/// repeating endpoint and auth annotations.
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dynamic-config.rs",
    version = "v1alpha1",
    kind = "DynamicConfigClass",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct DynamicConfigClassSpec {
    /// Which store speaks at the other end: etcd, consul, vault or
    /// config-server.
    pub source: String,
    /// The store's address.
    pub endpoint: String,
    /// A Secret in this namespace holding the token, when the store
    /// wants one; its `token` key is what the agent receives.
    pub token_secret: Option<String>,
}

/// The cluster-scoped class: the
/// platform team defines stores (and holds their credentials) ONCE,
/// and tenant namespaces reference them by name without ever seeing a
/// credential. The credential Secret lives in an explicitly named
/// namespace, and `namespaces` is the allowlist that keeps "global"
/// from meaning "anyone".
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dynamic-config.rs",
    version = "v1alpha1",
    kind = "ClusterDynamicConfigClass"
)]
#[serde(rename_all = "camelCase")]
pub struct ClusterDynamicConfigClassSpec {
    /// Which store speaks at the other end.
    pub source: String,
    /// The store's address.
    pub endpoint: String,
    /// The credential, when the store wants one — with the namespace it
    /// lives in named explicitly, because a cluster-scoped object has
    /// no namespace of its own to default to.
    pub token_secret: Option<ClusterSecretRef>,
    /// Namespaces whose Renders may use this class. Absent means every
    /// namespace — say so on purpose, not by omission.
    pub namespaces: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterSecretRef {
    pub name: String,
    pub namespace: String,
    /// The key inside the Secret; `token` by default.
    #[serde(default = "default_token_key")]
    pub key: String,
}

fn default_token_key() -> String {
    "token".to_owned()
}

/// A store document reconciled into a ConfigMap, for workloads that
/// cannot take a sidecar.
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dynamic-config.rs",
    version = "v1alpha1",
    kind = "DynamicConfigRender",
    namespaced,
    status = "RenderStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct DynamicConfigRenderSpec {
    /// The class naming the store.
    pub class: String,
    /// `DynamicConfigClass` (default, this namespace) or
    /// `ClusterDynamicConfigClass`
    /// split between a namespaced store and a platform-owned one.
    #[serde(default)]
    pub class_kind: ClassKind,
    /// The document's key in the store.
    pub key: String,
    /// The ConfigMap to write, and the file name inside it (its
    /// extension picks the rendered format).
    pub target: RenderTarget,
    /// Poll interval, seconds.
    #[serde(default = "default_interval")]
    pub interval_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenderTarget {
    /// The ConfigMap to write. Exactly one of `configMap` and `secret`.
    pub config_map: Option<String>,
    /// The Secret to write — for consumers that read Kubernetes Secrets
    /// natively. Secret
    /// updates are live objects: a watcher reacts without any pod
    /// restart, and an `envFrom` consumer picks them up on its next
    /// start — environment variables freeze at container start, which
    /// is Kubernetes' rule rather than this operator's.
    pub secret: Option<String>,
    /// The file name inside the target (its extension picks the
    /// rendered format). Required for the `file` shape; unused for
    /// `envEntries`, where the keys ARE the entries.
    pub file: Option<String>,
    /// `file` (default): one rendered document under `file`'s name.
    /// `envEntries`: every leaf, dotted paths upper-snaked
    /// (`db.pool_size` → `DB_POOL_SIZE`) — the shape `envFrom` consumes.
    /// `entries`: every leaf VERBATIM (`auth.postgres-password` stays
    /// so) — for Secret keys some other chart already named, the
    /// `existingSecret` contract.
    #[serde(default)]
    pub shape: TargetShape,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum ClassKind {
    #[default]
    DynamicConfigClass,
    ClusterDynamicConfigClass,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TargetShape {
    #[default]
    File,
    EnvEntries,
    Entries,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenderStatus {
    /// The last generation successfully rendered, and when.
    pub rendered_at: Option<String>,
    /// The last failure, kind and path only — never a value, the same
    /// redaction rule as everywhere else in the organisation.
    pub last_error: Option<String>,
    /// The k8s-native shape of the two fields above: one `Ready`
    /// condition, so `kubectl wait --for=condition=Ready` and every
    /// conditions-reading tool work here the way they work everywhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<ReadyCondition>,
}

/// `metav1.Condition`'s fields, spelled locally so the CRD schema stays
/// generated from this crate's types like everything else.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadyCondition {
    pub r#type: String,
    pub status: String,
    pub reason: String,
    pub message: String,
    pub last_transition_time: String,
}

impl ReadyCondition {
    fn ready(now: &str) -> Self {
        Self {
            r#type: "Ready".to_owned(),
            status: "True".to_owned(),
            reason: "Rendered".to_owned(),
            message: "the target carries the store's current document".to_owned(),
            last_transition_time: now.to_owned(),
        }
    }

    fn not_ready(now: &str, message: &str) -> Self {
        Self {
            r#type: "Ready".to_owned(),
            status: "False".to_owned(),
            reason: "RenderFailed".to_owned(),
            // Kind and shape only — never a value.
            message: message.to_owned(),
            last_transition_time: now.to_owned(),
        }
    }
}

fn default_interval() -> u64 {
    15
}

/// The CRD manifests, YAML, from the types above.
pub fn manifests() -> Result<String, Box<dyn std::error::Error>> {
    use kube::CustomResourceExt;

    Ok(format!(
        "{}\n---\n{}\n---\n{}\n",
        serde_yaml_ng(&DynamicConfigClass::crd())?,
        serde_yaml_ng(&ClusterDynamicConfigClass::crd())?,
        serde_yaml_ng(&DynamicConfigRender::crd())?,
    ))
}

fn serde_yaml_ng<T: Serialize>(value: &T) -> Result<String, Box<dyn std::error::Error>> {
    // JSON is valid YAML; a YAML serialiser dependency buys prettiness
    // this generated file does not need.
    Ok(serde_json::to_string_pretty(value)?)
}

/// The reconciler, deliberately THIN: a `DynamicConfigRender` becomes
/// one ConfigMap, rendered through the SAME source construction and
/// rendering the sidecar agent uses. Two CRDs, reconcile-to-ConfigMap,
/// nothing more — the recorded tripwire against operator sprawl.
pub async fn run(client: kube::Client) -> Result<(), Box<dyn std::error::Error>> {
    use futures::StreamExt;
    use kube::runtime::controller::Controller;
    use kube::runtime::watcher;
    use kube::Api;

    let renders: Api<DynamicConfigRender> = Api::all(client.clone());
    let classes: Api<DynamicConfigClass> = Api::all(client.clone());
    let context = std::sync::Arc::new(client);

    // A Class edit re-renders everything referencing it — the wiring
    // that makes a rotated endpoint land without touching the Renders.
    //
    // Read from the controller's own reflector rather than by listing the
    // API. A mapper is synchronous, so an API call inside one has to be
    // blocked on — which parks the reactor thread on a network round trip,
    // once per class event, while every other reconcile waits. The
    // reflector already holds every Render the controller is watching, and
    // reading it is a lock and a filter.
    let controller = Controller::new(renders, watcher::Config::default());
    let known = controller.store();

    let mapper = {
        let known = known.clone();

        move |class: DynamicConfigClass| {
            let (Some(namespace), Some(name)) = (class.metadata.namespace, class.metadata.name)
            else {
                return Vec::new();
            };

            known
                .state()
                .into_iter()
                .filter(|render| {
                    render.spec.class == name
                        && render.metadata.namespace.as_deref() == Some(namespace.as_str())
                })
                .filter_map(|render| {
                    Some(
                        kube::runtime::reflector::ObjectRef::new(render.metadata.name.as_deref()?)
                            .within(&namespace),
                    )
                })
                .collect()
        }
    };

    let cluster_classes: Api<ClusterDynamicConfigClass> = Api::all((*context).clone());
    let cluster_mapper = {
        let known = known.clone();

        move |class: ClusterDynamicConfigClass| {
            let Some(name) = class.metadata.name else {
                return Vec::new();
            };

            known
                .state()
                .into_iter()
                .filter(|render| {
                    render.spec.class == name
                        && render.spec.class_kind == ClassKind::ClusterDynamicConfigClass
                })
                .filter_map(|render| {
                    Some(
                        kube::runtime::reflector::ObjectRef::new(render.metadata.name.as_deref()?)
                            .within(render.metadata.namespace.as_deref()?),
                    )
                })
                .collect()
        }
    };

    controller
        .watches(classes, watcher::Config::default(), mapper)
        .watches(cluster_classes, watcher::Config::default(), cluster_mapper)
        .run(reconcile, on_error, context)
        .for_each(|outcome| async move {
            match outcome {
                Ok((object, _)) => tracing::info!(name = %object.name, "reconciled"),
                Err(error) => tracing::warn!(%error, "reconcile failed"),
            }
        })
        .await;

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Kube(#[from] kube::Error),
}

/// How long to wait before trying a Render that failed again.
///
/// Doubling from five seconds to five minutes, per object, spread by up to
/// a quarter. A flat thirty seconds is two failures a minute for as long as
/// a store is down — times every Render pointing at it — and a fleet that
/// all started together makes those arrive in step.
///
/// Keyed by object, so one Render failing does not slow another down, and
/// forgotten on success: the next failure starts from five seconds again.
static BACKOFF: std::sync::Mutex<Option<std::collections::HashMap<String, u32>>> =
    std::sync::Mutex::new(None);

fn failures(key: &str, failed: bool) -> u32 {
    let mut guard = BACKOFF.lock().unwrap_or_else(|error| error.into_inner());
    let counts = guard.get_or_insert_with(std::collections::HashMap::new);

    if !failed {
        counts.remove(key);

        return 0;
    }

    let count = counts.entry(key.to_owned()).or_insert(0);

    *count = count.saturating_add(1);

    *count
}

fn on_error(
    object: std::sync::Arc<DynamicConfigRender>,
    _error: &ReconcileError,
    _context: std::sync::Arc<kube::Client>,
) -> kube::runtime::controller::Action {
    const BASE: std::time::Duration = std::time::Duration::from_secs(5);
    const CEILING: std::time::Duration = std::time::Duration::from_secs(300);

    let key = format!(
        "{}/{}",
        object.metadata.namespace.as_deref().unwrap_or_default(),
        object.metadata.name.as_deref().unwrap_or_default()
    );

    let attempt = failures(&key, true);
    let factor = 1u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX);
    let wait = BASE.checked_mul(factor).unwrap_or(CEILING).min(CEILING);

    dynamic_config_agent::metrics::reconcile_failed();

    kube::runtime::controller::Action::requeue(dynamic_config_agent::metrics::spread(wait))
}

async fn reconcile(
    render: std::sync::Arc<DynamicConfigRender>,
    client: std::sync::Arc<kube::Client>,
) -> Result<kube::runtime::controller::Action, ReconcileError> {
    use kube::api::{Patch, PatchParams};
    use kube::Api;

    let started = std::time::Instant::now();

    let namespace = render
        .metadata
        .namespace
        .clone()
        .ok_or_else(|| ReconcileError::Message("a Render without a namespace".into()))?;
    let name = render
        .metadata
        .name
        .clone()
        .ok_or_else(|| ReconcileError::Message("a Render without a name".into()))?;

    let interval = std::time::Duration::from_secs(render.spec.interval_seconds.max(1));
    let statuses: Api<DynamicConfigRender> = Api::namespaced((*client).clone(), &namespace);

    let recorder = kube::runtime::events::Recorder::new(
        (*client).clone(),
        kube::runtime::events::Reporter {
            controller: "dynamic-config-operator".into(),
            instance: None,
        },
    );
    let object_ref = kube::runtime::reflector::ObjectRef::from_obj(render.as_ref()).into();
    let now = chrono::Utc::now().to_rfc3339();

    match attempt(&render, &client, &namespace).await {
        Ok(changed) => {
            // Status only moves when something DID: an unconditional
            // timestamp write here is a fresh watch event per reconcile,
            // which is a reconcile storm feeding itself.
            let had_error = render
                .status
                .as_ref()
                .is_some_and(|status| status.last_error.is_some());
            let never_rendered = render
                .status
                .as_ref()
                .and_then(|status| status.rendered_at.as_ref())
                .is_none();

            if changed || had_error || never_rendered {
                let status = serde_json::json!({ "status": RenderStatus {
                    rendered_at: Some(now.clone()),
                    last_error: None,
                    conditions: vec![ReadyCondition::ready(&now)],
                }});

                statuses
                    .patch_status(&name, &PatchParams::default(), &Patch::Merge(&status))
                    .await?;
            }

            dynamic_config_agent::metrics::rendered();

            if changed {
                // `kubectl describe`'s language: a Normal event per
                // actual change, not per reconcile tick.
                let _ = recorder
                    .publish(
                        &kube::runtime::events::Event {
                            type_: kube::runtime::events::EventType::Normal,
                            reason: "Rendered".into(),
                            note: Some("the target now carries the store's document".into()),
                            action: "Render".into(),
                            secondary: None,
                        },
                        &object_ref,
                    )
                    .await;
            }

            failures(
                &format!(
                    "{}/{}",
                    render.metadata.namespace.as_deref().unwrap_or_default(),
                    render.metadata.name.as_deref().unwrap_or_default()
                ),
                false,
            );
            dynamic_config_agent::metrics::reconciled(started.elapsed());

            // Spread, for the same reason every other interval here is: a
            // hundred Renders created by one `kubectl apply` would otherwise
            // come back to the store together, forever.
            Ok(kube::runtime::controller::Action::requeue(
                dynamic_config_agent::metrics::spread(interval),
            ))
        }
        Err(error) => {
            // Kind and shape only — never a value, the same redaction
            // rule as everywhere else in the organisation.
            let status = serde_json::json!({ "status": RenderStatus {
                rendered_at: render.status.as_ref().and_then(|s| s.rendered_at.clone()),
                last_error: Some(error.to_string()),
                conditions: vec![ReadyCondition::not_ready(&now, &error.to_string())],
            }});

            statuses
                .patch_status(&name, &PatchParams::default(), &Patch::Merge(&status))
                .await?;

            dynamic_config_agent::metrics::failed();

            let _ = recorder
                .publish(
                    &kube::runtime::events::Event {
                        type_: kube::runtime::events::EventType::Warning,
                        reason: "RenderFailed".into(),
                        note: Some(error.to_string()),
                        action: "Render".into(),
                        secondary: None,
                    },
                    &object_ref,
                )
                .await;

            Err(ReconcileError::Message(error.to_string()))
        }
    }
}

/// Fetch, render, write — the fallible middle of a reconcile. Answers
/// whether the ConfigMap's content actually moved.
async fn attempt(
    render: &DynamicConfigRender,
    client: &kube::Client,
    namespace: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use k8s_openapi::api::core::v1::{ConfigMap, Secret};
    use kube::api::{Patch, PatchParams};
    use kube::Api;

    // Resolve the class, namespaced or cluster-scoped — the ESO split:
    // a Class is this namespace's own; a ClusterClass is the platform
    // team's, credentials held where tenants cannot read them.
    let (source_name, endpoint, token_ref) = match render.spec.class_kind {
        ClassKind::DynamicConfigClass => {
            let classes: Api<DynamicConfigClass> = Api::namespaced(client.clone(), namespace);
            let class = classes
                .get_opt(&render.spec.class)
                .await?
                .ok_or_else(|| format!("class {:?} does not exist here", render.spec.class))?;

            (
                class.spec.source,
                class.spec.endpoint,
                class
                    .spec
                    .token_secret
                    .map(|name| (name, namespace.to_owned(), "token".to_owned())),
            )
        }
        ClassKind::ClusterDynamicConfigClass => {
            let classes: Api<ClusterDynamicConfigClass> = Api::all(client.clone());
            let class = classes
                .get_opt(&render.spec.class)
                .await?
                .ok_or_else(|| format!("cluster class {:?} does not exist", render.spec.class))?;

            if let Some(allowed) = &class.spec.namespaces {
                if !allowed.iter().any(|n| n == namespace) {
                    return Err(format!(
                        "cluster class {:?} does not allow namespace {namespace:?}: \
                         its `namespaces` list is the platform team's boundary",
                        render.spec.class
                    )
                    .into());
                }
            }

            (
                class.spec.source,
                class.spec.endpoint,
                class
                    .spec
                    .token_secret
                    .map(|sr| (sr.name, sr.namespace, sr.key)),
            )
        }
    };

    // Exactly one destination, named up front.
    let target = &render.spec.target;

    match (&target.config_map, &target.secret) {
        (Some(_), Some(_)) => {
            return Err("target names both configMap and secret: one document, \
                 one destination"
                .into())
        }
        (None, None) => return Err("target names neither configMap nor secret".into()),
        _ => {}
    }

    if target.shape == TargetShape::File && target.file.is_none() {
        return Err("the file shape needs target.file: its extension picks \
             the rendered format"
            .into());
    }

    // The honesty rule, kept and completed: a ConfigMap is not a
    // Secret, so a vault document may land only in the Secret target —
    // the feature the old refusal said would lift it.
    if source_name == "vault" && target.secret.is_none() {
        return Err("a vault document rendered into a ConfigMap is a secrecy \
             downgrade; give the Render a `secret:` target instead"
            .into());
    }

    // The class's token, when it names one — read from wherever the
    // class said it lives, which for a cluster class is the platform
    // team's namespace, never the tenant's.
    let token = match &token_ref {
        None => None,
        Some((secret_name, secret_namespace, key)) => {
            let secrets: Api<Secret> = Api::namespaced(client.clone(), secret_namespace);
            let secret = secrets.get_opt(secret_name).await?.ok_or_else(|| {
                format!("token secret {secret_namespace}/{secret_name} does not exist")
            })?;

            Some(
                secret
                    .data
                    .as_ref()
                    .and_then(|data| data.get(key))
                    .map(|bytes| String::from_utf8_lossy(&bytes.0).into_owned())
                    .ok_or_else(|| {
                        format!("secret {secret_namespace}/{secret_name} has no {key:?} key")
                    })?,
            )
        }
    };

    // The agent's own Spec, sources and renderer — one implementation.
    let spec = dynamic_config_agent::spec::Spec {
        source: source_name,
        endpoint,
        key: render.spec.key.clone(),
        // The envEntries shape never renders to text, so its Spec needs
        // only a placeholder extension the validator accepts.
        out: std::path::PathBuf::from(render.spec.target.file.as_deref().unwrap_or("entries.json")),
        watch: None,
        file_mode: None,
        metrics_addr: None,
        token,
        section: None,
        auth: None,
        auth_mount: None,
        auth_role: None,
        auth_username: None,
        auth_token_path: None,
        namespace: None,
        reference: None,
        ssh_key: None,
        api_url: None,
        template: None,
        template_inline: None,
        ca: None,
        tls_cert: None,
        tls_key: None,
        password: None,
    };

    let source = dynamic_config_agent::sources::build(&spec)
        .await
        .map_err(|error| error.to_string())?;
    let fetched = source.fetch().await?;

    // The data map, by shape: one rendered document under the file's
    // name, or every leaf of the resolved document as its own
    // upper-snaked entry — the shape `envFrom` consumes and a
    // Secret-watching operator reads key by key.
    let data: std::collections::BTreeMap<String, String> = match target.shape {
        TargetShape::File => {
            let rendered = dynamic_config_agent::render::render_fetched(&fetched, &spec)
                .map_err(|error| error.to_string())?;
            let file = target.file.clone().expect("validated above");

            std::iter::once((file, rendered)).collect()
        }
        TargetShape::EnvEntries | TargetShape::Entries => {
            let document = dynamic_config_agent::render::resolve(
                &fetched.text,
                fetched.format,
                spec.section.as_deref(),
            )
            .map_err(|error| error.to_string())?;

            let entries = if target.shape == TargetShape::Entries {
                dynamic_config_agent::render::verbatim_entries(&document)
            } else {
                dynamic_config_agent::render::env_entries(&document)
            };

            entries
                .map_err(|error| error.to_string())?
                .into_iter()
                .collect()
        }
    };

    // Owned by the Render, so deleting the Render garbage-collects the
    // target — Kubernetes' own cleanup, no finalizer to get wrong.
    let owner = serde_json::json!([{
        "apiVersion": "dynamic-config.rs/v1alpha1",
        "kind": "DynamicConfigRender",
        "name": render.metadata.name,
        "uid": render.metadata.uid,
        "controller": true,
    }]);
    let apply = PatchParams::apply("dynamic-config-operator").force();

    if let Some(name) = &target.secret {
        let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);
        let unchanged = secrets
            .get_opt(name)
            .await?
            .and_then(|current| current.data)
            .is_some_and(|current| {
                current.len() == data.len()
                    && data.iter().all(|(key, value)| {
                        current
                            .get(key)
                            .is_some_and(|bytes| bytes.0 == value.as_bytes())
                    })
            });

        if unchanged {
            return Ok(false);
        }

        let manifest = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": name, "ownerReferences": owner },
            "type": "Opaque",
            "stringData": data,
        });

        secrets
            .patch(name, &apply, &Patch::Apply(&manifest))
            .await?;

        return Ok(true);
    }

    let name = target.config_map.as_ref().expect("validated above");
    let config_maps: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let unchanged = config_maps
        .get_opt(name)
        .await?
        .and_then(|current| current.data)
        .is_some_and(|current| {
            current.len() == data.len()
                && data
                    .iter()
                    .all(|(key, value)| current.get(key) == Some(value))
        });

    if unchanged {
        return Ok(false);
    }

    let manifest = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": name, "ownerReferences": owner },
        "data": data,
    });

    config_maps
        .patch(name, &apply, &Patch::Apply(&manifest))
        .await?;

    Ok(true)
}
