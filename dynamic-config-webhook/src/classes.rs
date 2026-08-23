//! Resolving `dynamic-config.rs/class` against the cluster's own objects.
//!
//! The operator has had `DynamicConfigClass` and
//! `ClusterDynamicConfigClass` since 0.1.1: a platform team names a store
//! once, and a workload references it. The webhook did not read them, so
//! the two halves of this product solved the same problem twice — the
//! operator read Classes, the injector wanted the endpoint and the auth
//! spelled out again on every pod.
//!
//! # The admission path still calls nothing
//!
//! That is the property this had to keep, and the reason it was deferred
//! for two rounds. An admission webhook that calls the API server in its
//! hot path is one that fails when the API server is busy — and **every pod
//! creation in the cluster fails with it**.
//!
//! So the API call is not on the admission path. A background task lists
//! the Classes on an interval and keeps them in memory; admission reads the
//! map and nothing else. The cost is a synchronisation delay a ConfigMap
//! already has, which is the trade `DEFERRED.md` named as the one to try.
//!
//! # And it is off unless asked
//!
//! Reading Classes needs RBAC the webhook otherwise does not have. A
//! deployment that spells its stores out in annotations, or pins them in
//! the installation document, needs none of this and should carry none of
//! it.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// What a class says about a store, flattened out of the two CRD shapes.
///
/// The two differ in exactly one way that matters here — who may use them —
/// so they are one type with that one field, rather than two types and a
/// match at every use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    /// `vault`, `consul`, `etcd`, `config-server`.
    pub source: String,
    /// The store's address.
    pub endpoint: String,
    /// `<secret>/<key>` in the **pod's own namespace**, when the class
    /// names a credential the webhook can actually mount.
    pub token_secret: Option<String>,
    /// `None` for a namespaced class, which only its own namespace sees.
    /// `Some(list)` for a cluster class; empty means every namespace.
    pub namespaces: Option<Vec<String>>,
    /// Set when a cluster class names a credential in a namespace that is
    /// not the pod's.
    ///
    /// Not an error until somebody uses it, and then a specific one: a pod
    /// can only mount Secrets from its own namespace, so this class works
    /// for the operator — which reads the Secret itself — and cannot work
    /// for a sidecar.
    pub credential_elsewhere: Option<String>,
}

impl Class {
    /// Whether a pod in `namespace` may use this class.
    #[must_use]
    pub fn admits(&self, namespace: &str) -> bool {
        match &self.namespaces {
            // Namespaced: the API server already scoped it, and the cache
            // keys it by namespace.
            None => true,
            // Absent in the CRD means every namespace, which the schema
            // says is a thing to write on purpose rather than omit.
            Some(allowed) => allowed.is_empty() || allowed.iter().any(|name| name == namespace),
        }
    }
}

/// The classes as of the last poll.
///
/// Cluster classes are keyed by `("", name)` so one map holds both without
/// a namespace ever colliding with the cluster scope.
pub type Classes = BTreeMap<(String, String), Class>;

/// The map admission reads, and a background task replaces.
#[derive(Debug, Clone, Default)]
pub struct Cache {
    classes: Arc<RwLock<Arc<Classes>>>,
}

impl Cache {
    /// An empty cache, which resolves nothing and refuses by saying so.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces everything the cache holds.
    ///
    /// Whole-map rather than per-object: a list is what the API server
    /// answers, a class that was deleted has to disappear, and a map
    /// swapped under one lock is never half a generation.
    pub fn replace(&self, classes: Classes) {
        if let Ok(mut held) = self.classes.write() {
            *held = Arc::new(classes);
        }
    }

    /// The class a pod in `namespace` means by `name`.
    ///
    /// Its own namespace first, then the cluster scope — the same order
    /// Kubernetes resolves anything else in, and the one that lets a
    /// namespace override a platform default without asking the platform.
    #[must_use]
    pub fn resolve(&self, namespace: &str, name: &str) -> Option<Class> {
        let held = self.classes.read().ok()?;

        held.get(&(namespace.to_owned(), name.to_owned()))
            .or_else(|| held.get(&(String::new(), name.to_owned())))
            .cloned()
    }

    /// How many classes are held, for the log line that says the poll
    /// worked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.classes.read().map_or(0, |held| held.len())
    }

    /// Whether nothing has been loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Reads a `DynamicConfigClassList` or `ClusterDynamicConfigClassList` into
/// the flat shape above.
///
/// `namespaced` says which CRD this is, which decides how a credential
/// reference is read: a namespaced class names a Secret beside itself, a
/// cluster class names one anywhere and most of those are unusable from a
/// pod.
#[must_use]
pub fn from_list(list: &serde_json::Value, namespaced: bool) -> Classes {
    let mut classes = Classes::new();

    let Some(items) = list.get("items").and_then(serde_json::Value::as_array) else {
        return classes;
    };

    for item in items {
        let Some(name) = item
            .pointer("/metadata/name")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };

        let namespace = if namespaced {
            item.pointer("/metadata/namespace")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        } else {
            String::new()
        };

        let (Some(source), Some(endpoint)) = (
            item.pointer("/spec/source")
                .and_then(serde_json::Value::as_str),
            item.pointer("/spec/endpoint")
                .and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };

        let mut token_secret = None;
        let mut credential_elsewhere = None;

        if namespaced {
            // A string: a Secret beside the class, whose `token` key is
            // what the agent receives — which is exactly the annotation's
            // own `<secret>/<key>` form.
            token_secret = item
                .pointer("/spec/tokenSecret")
                .and_then(serde_json::Value::as_str)
                .map(|secret| format!("{secret}/token"));
        } else if let Some(reference) = item.pointer("/spec/tokenSecret") {
            let secret = reference.get("name").and_then(serde_json::Value::as_str);
            let holds = reference
                .get("namespace")
                .and_then(serde_json::Value::as_str);
            let key = reference
                .get("key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("token");

            match (secret, holds) {
                // Recorded rather than resolved: which namespace the pod is
                // in is not known here, so whether this is usable is
                // decided at admission.
                (Some(secret), Some(holds)) => {
                    token_secret = Some(format!("{secret}/{key}"));
                    credential_elsewhere = Some(holds.to_owned());
                }
                _ => continue,
            }
        }

        let namespaces = if namespaced {
            None
        } else {
            Some(
                item.pointer("/spec/namespaces")
                    .and_then(serde_json::Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        };

        classes.insert(
            (namespace, name.to_owned()),
            Class {
                source: source.to_owned(),
                endpoint: endpoint.to_owned(),
                token_secret,
                namespaces,
                credential_elsewhere,
            },
        );
    }

    classes
}

/// Where the kubelet mounts the webhook's own service-account token.
const TOKEN: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";
/// And the cluster's CA.
const CA: &str = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt";

/// How often the classes are re-listed.
///
/// Thirty seconds: a class is a platform object that changes when somebody
/// edits it, and the delay this buys is the one a mounted ConfigMap already
/// has. Shorter would be a list of the whole cluster's classes more often,
/// against an API server every admission already depends on being well.
const EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// Fills `cache` from the API server, forever.
///
/// **Not on the admission path.** This is the only thing in this binary
/// that calls the API server at all, it runs on its own task, and a poll
/// that fails leaves the previous map in place — a webhook that started
/// refusing every classed pod because the API server hiccuped would be the
/// outage this design exists to avoid.
///
/// Returns when the token or the CA cannot be read, which means the
/// deployment asked for classes without granting them; the caller logs
/// that once rather than every thirty seconds.
pub async fn poll(cache: Cache) -> Result<std::convert::Infallible, String> {
    let ca = std::fs::read(CA).map_err(|error| format!("{CA}: {error}"))?;

    let roots = ureq::tls::parse_pem(&ca)
        .filter_map(Result::ok)
        .filter_map(|item| match item {
            ureq::tls::PemItem::Certificate(certificate) => Some(certificate),
            _ => None,
        })
        .collect::<Vec<_>>();

    if roots.is_empty() {
        return Err(format!("{CA}: holds no certificate"));
    }

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::new_with_certs(&roots))
                .build(),
        )
        .build()
        .new_agent();

    loop {
        match list(&agent) {
            Ok(classes) => {
                let held = classes.len();
                cache.replace(classes);
                tracing::debug!(classes = held, "the class cache was refreshed");
            }
            // Kept rather than cleared: the previous map is still the best
            // answer available, and refusing every classed pod because one
            // list failed would turn an API hiccup into an outage.
            Err(error) => {
                tracing::warn!(%error, "the classes could not be listed; keeping the last set")
            }
        }

        tokio::time::sleep(EVERY).await;
    }
}

/// Both kinds, in one pass.
fn list(agent: &ureq::Agent) -> Result<Classes, String> {
    // Re-read per poll: a projected token rotates, and one read at start
    // stops working after an hour.
    let token = std::fs::read_to_string(TOKEN)
        .map_err(|error| format!("{TOKEN}: {error}"))?
        .trim()
        .to_owned();

    let mut classes = Classes::new();

    for (path, namespaced) in [
        (
            "/apis/dynamic-config.rs/v1alpha1/clusterdynamicconfigclasses",
            false,
        ),
        (
            "/apis/dynamic-config.rs/v1alpha1/dynamicconfigclasses",
            true,
        ),
    ] {
        let url = format!("https://kubernetes.default.svc{path}");

        let body: serde_json::Value = agent
            .get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .map_err(|error| format!("listing {path}: {error}"))?
            .body_mut()
            .read_json()
            .map_err(|error| format!("reading {path}: {error}"))?;

        classes.extend(from_list(&body, namespaced));
    }

    Ok(classes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespaced_list() -> serde_json::Value {
        serde_json::json!({
            "items": [{
                "metadata": { "name": "team-vault", "namespace": "billing" },
                "spec": {
                    "source": "vault",
                    "endpoint": "https://vault.billing:8200",
                    "tokenSecret": "vault-token",
                },
            }],
        })
    }

    fn cluster_list() -> serde_json::Value {
        serde_json::json!({
            "items": [
                {
                    "metadata": { "name": "infra-consul" },
                    "spec": {
                        "source": "consul",
                        "endpoint": "http://consul.infra:8500",
                        "namespaces": ["billing", "payments"],
                    },
                },
                {
                    "metadata": { "name": "everywhere" },
                    "spec": { "source": "etcd", "endpoint": "http://etcd:2379" },
                },
                {
                    "metadata": { "name": "platform-vault" },
                    "spec": {
                        "source": "vault",
                        "endpoint": "https://vault.platform:8200",
                        "tokenSecret": {
                            "name": "vault-token",
                            "namespace": "platform",
                            "key": "token",
                        },
                    },
                },
            ],
        })
    }

    #[test]
    fn a_namespaced_class_resolves_in_its_own_namespace_and_nowhere_else() {
        let cache = Cache::new();
        cache.replace(from_list(&namespaced_list(), true));

        let class = cache.resolve("billing", "team-vault").expect("its own");

        assert_eq!(class.source, "vault");
        assert_eq!(class.token_secret.as_deref(), Some("vault-token/token"));

        assert!(cache.resolve("payments", "team-vault").is_none());
    }

    #[test]
    fn a_cluster_class_admits_the_namespaces_it_lists_and_no_others() {
        let cache = Cache::new();
        cache.replace(from_list(&cluster_list(), false));

        let class = cache.resolve("billing", "infra-consul").expect("listed");

        assert!(class.admits("billing"));
        assert!(!class.admits("sandbox"), "the list is an allowlist");
    }

    /// Absent means every namespace, which the schema asks to be written on
    /// purpose rather than omitted — but omitted is what it means.
    #[test]
    fn a_cluster_class_with_no_list_admits_everyone() {
        let cache = Cache::new();
        cache.replace(from_list(&cluster_list(), false));

        assert!(cache
            .resolve("anywhere", "everywhere")
            .expect("global")
            .admits("anywhere"));
    }

    /// A pod can only mount Secrets from its own namespace, so a cluster
    /// class whose credential lives elsewhere works for the operator and
    /// cannot work for a sidecar. Recorded here, refused at admission.
    #[test]
    fn a_credential_in_another_namespace_is_recorded_as_such() {
        let cache = Cache::new();
        cache.replace(from_list(&cluster_list(), false));

        let class = cache.resolve("billing", "platform-vault").expect("there");

        assert_eq!(class.credential_elsewhere.as_deref(), Some("platform"));
    }

    /// Its own namespace before the cluster scope: a namespace can override
    /// a platform default without asking the platform.
    #[test]
    fn a_namespaced_class_shadows_a_cluster_one_of_the_same_name() {
        let mut both = from_list(&cluster_list(), false);
        both.extend(from_list(
            &serde_json::json!({
                "items": [{
                    "metadata": { "name": "everywhere", "namespace": "billing" },
                    "spec": { "source": "vault", "endpoint": "https://vault.billing:8200" },
                }],
            }),
            true,
        ));

        let cache = Cache::new();
        cache.replace(both);

        assert_eq!(
            cache.resolve("billing", "everywhere").expect("own").source,
            "vault"
        );
        assert_eq!(
            cache
                .resolve("other", "everywhere")
                .expect("cluster")
                .source,
            "etcd"
        );
    }

    /// An empty cache resolves nothing rather than answering with a
    /// half-built class, which is what makes "not found" a refusal an
    /// operator can act on.
    #[test]
    fn an_empty_cache_resolves_nothing() {
        assert!(Cache::new().resolve("billing", "team-vault").is_none());
        assert!(Cache::new().is_empty());
    }
}
