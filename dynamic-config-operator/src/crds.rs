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
pub struct DynamicConfigRenderSpec {
    /// The class naming the store.
    pub class: String,
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
pub struct RenderTarget {
    pub config_map: String,
    pub file: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct RenderStatus {
    /// The last generation successfully rendered, and when.
    pub rendered_at: Option<String>,
    /// The last failure, kind and path only — never a value, the same
    /// redaction rule as everywhere else in the organisation.
    pub last_error: Option<String>,
}

fn default_interval() -> u64 {
    15
}

/// The CRD manifests, YAML, from the types above.
pub fn manifests() -> Result<String, Box<dyn std::error::Error>> {
    use kube::CustomResourceExt;

    Ok(format!(
        "{}\n---\n{}\n",
        serde_yaml_ng(&DynamicConfigClass::crd())?,
        serde_yaml_ng(&DynamicConfigRender::crd())?,
    ))
}

fn serde_yaml_ng<T: Serialize>(value: &T) -> Result<String, Box<dyn std::error::Error>> {
    // JSON is valid YAML; a YAML serialiser dependency buys prettiness
    // this generated file does not need.
    Ok(serde_json::to_string_pretty(value)?)
}

/// The controller loop — 0.3.0's work; the shape is here so `main` and
/// the RBAC in `deploy/` are settled from day one.
pub async fn run(client: kube::Client) -> Result<(), Box<dyn std::error::Error>> {
    use futures::StreamExt;
    use kube::runtime::watcher;
    use kube::Api;

    let renders: Api<DynamicConfigRender> = Api::all(client);

    // Watch-and-log until the reconciler lands: an operator that starts,
    // sees, and says so is testable in kind before it mutates anything.
    let mut stream = std::pin::pin!(watcher(renders, watcher::Config::default()));

    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => tracing::info!(?event, "observed"),
            Err(error) => tracing::warn!(%error, "watch error"),
        }
    }

    Ok(())
}
