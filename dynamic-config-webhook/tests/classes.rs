//! `dynamic-config.rs/class`, against a cache rather than a cluster.
//!
//! Which is how the admission path itself works: the API call is on a
//! background timer and never on the path a pod creation waits on, so the
//! decision under test here is the whole of the decision in production.

use dynamic_config_webhook::classes::{from_list, Cache};
use serde_json::{json, Value};

fn review(pod: Value, namespace: &str) -> Value {
    json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": { "uid": "u", "namespace": namespace, "object": pod },
    })
}

/// A pod that says only what is its own: the key, the path, and which
/// class to get the rest from.
fn classed(class: &str) -> Value {
    json!({
        "metadata": {
            "annotations": {
                "dynamic-config.rs/inject": "true",
                "dynamic-config.rs/class": class,
                "dynamic-config.rs/key": "myapp/config.json",
                "dynamic-config.rs/path": "/config/rendered.toml",
            },
        },
        "spec": { "containers": [ { "name": "app", "image": "app:1" } ] },
    })
}

fn cache() -> Cache {
    let cache = Cache::new();
    let mut classes = from_list(
        &json!({
            "items": [{
                "metadata": { "name": "infra-consul" },
                "spec": {
                    "source": "consul",
                    "endpoint": "http://consul.infra:8500",
                    "namespaces": ["billing"],
                },
            }],
        }),
        false,
    );

    classes.extend(from_list(
        &json!({
            "items": [{
                "metadata": { "name": "team-vault", "namespace": "billing" },
                "spec": {
                    "source": "vault",
                    "endpoint": "https://vault.billing:8200",
                    "tokenSecret": "vault-token",
                },
            }],
        }),
        true,
    ));

    cache.replace(classes);
    cache
}

fn answer(pod: Value, namespace: &str, cache: &Cache) -> Value {
    dynamic_config_webhook::admission_response_with_classes(
        &review(pod, namespace),
        &dynamic_config_webhook::Installation::from_lookup(&|_: &str| None).expect("empty"),
        cache,
    )
}

fn decoded(response: &Value) -> String {
    response
        .pointer("/response/patch")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The whole point: a pod names a class and a key, and the store comes from
/// the object a platform team maintains.
#[test]
fn a_class_supplies_the_store_a_pod_did_not_spell_out() {
    let response = answer(classed("team-vault"), "billing", &cache());

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));
    assert!(!decoded(&response).is_empty(), "an agent was injected");
}

/// The class is a default, not a pin. A pod that names both keeps its own,
/// which is the rule the installation document already follows.
#[test]
fn a_pods_own_endpoint_wins_over_the_classs() {
    let mut pod = classed("team-vault");
    pod["metadata"]["annotations"]["dynamic-config.rs/endpoint"] =
        json!("https://vault.override:8200");

    let response = answer(pod, "billing", &cache());

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));
}

/// The `namespaces` list is what keeps a cluster-scoped class from meaning
/// anyone.
#[test]
fn a_cluster_class_refuses_a_namespace_it_does_not_admit() {
    let response = answer(classed("infra-consul"), "sandbox", &cache());

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("does not admit"));
}

/// A namespaced class belongs to its namespace and to no other.
#[test]
fn a_namespaced_class_is_invisible_from_elsewhere() {
    let response = answer(classed("team-vault"), "payments", &cache());

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

/// The default build resolves nothing, so a pod naming a class is told
/// that rather than admitted without the store the class was to supply.
#[test]
fn without_the_feature_a_class_is_refused_and_says_why() {
    let response = answer(classed("team-vault"), "billing", &Cache::new());

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();

    assert!(message.contains("webhook.classes.enabled"), "{message}");
}

/// A pod that names no class is untouched by any of it, which is every pod
/// in an installation that does not use them.
#[test]
fn a_pod_without_a_class_is_unaffected() {
    let pod = json!({
        "metadata": {
            "annotations": {
                "dynamic-config.rs/inject": "true",
                "dynamic-config.rs/source": "consul",
                "dynamic-config.rs/endpoint": "http://consul:8500",
                "dynamic-config.rs/key": "myapp/config.json",
                "dynamic-config.rs/path": "/config/rendered.toml",
            },
        },
        "spec": { "containers": [ { "name": "app", "image": "app:1" } ] },
    });

    let response = answer(pod, "billing", &Cache::new());

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));
}

/// A pod that was injected once and is applied again — `kubectl get pod -o
/// yaml | kubectl apply -f -`, or a controller replaying a stored spec —
/// carries its `class` annotation, because injection does not remove it.
/// If the class has since been renamed, deleted, or put behind a
/// `webhook.classes.enabled` an administrator has turned off, resolving it
/// again can only refuse a pod that is already running correctly.
///
/// So the mark is read before the class is: an already-injected pod is a
/// no-op, and a no-op needs no store.
#[test]
fn an_already_injected_pod_is_not_refused_by_a_class_that_stopped_resolving() {
    let mut pod = classed("team-vault");

    pod["metadata"]["annotations"]["dynamic-config.rs/status"] = json!("injected");
    pod["spec"]["containers"]
        .as_array_mut()
        .expect("containers")
        .push(json!({ "name": "dynamic-config-agent", "image": "agent:v0.3.0" }));

    // The empty cache is the worst case and the likeliest one: it is what
    // an installation with `webhook.classes.enabled: false` has.
    let response = answer(pod, "billing", &Cache::new());

    assert_eq!(
        response.pointer("/response/allowed"),
        Some(&json!(true)),
        "a pod already carrying the injection was refused on re-admission"
    );
    assert!(
        decoded(&response).is_empty(),
        "an already-injected pod must be patched again by nothing"
    );
}
