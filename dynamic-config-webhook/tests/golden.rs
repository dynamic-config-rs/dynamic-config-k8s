//! The webhook as a pure function: recorded AdmissionReview in, the
//! exact patch out. No cluster anywhere near this file — which is the
//! point of the design.

use serde_json::{json, Value};

fn review(pod: Value) -> Value {
    json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": { "uid": "test-uid-1", "object": pod },
    })
}

fn annotated(mode: &str) -> Value {
    json!({
        "metadata": {
            "annotations": {
                "dynamic-config.rs/inject": "true",
                "dynamic-config.rs/source": "consul",
                "dynamic-config.rs/endpoint": "http://consul:8500",
                "dynamic-config.rs/key": "myapp/config.json",
                "dynamic-config.rs/path": "/config/rendered.toml",
                "dynamic-config.rs/mode": mode,
            },
        },
        "spec": { "containers": [ { "name": "app", "image": "app:1" } ] },
    })
}

/// The other canonical pod: vault through the pod's own service
/// account, a private CA, no secret anywhere in the annotations.
fn vault_kubernetes_pod() -> Value {
    json!({
        "metadata": {
            "annotations": {
                "dynamic-config.rs/inject": "true",
                "dynamic-config.rs/source": "vault",
                "dynamic-config.rs/endpoint": "https://vault.vault:8200",
                "dynamic-config.rs/key": "secret/myapp",
                "dynamic-config.rs/path": "/config/rendered.yaml",
                "dynamic-config.rs/auth": "kubernetes",
                "dynamic-config.rs/auth-role": "myapp",
                "dynamic-config.rs/ca-configmap": "vault-ca",
            },
        },
        "spec": { "containers": [ { "name": "app", "image": "app:1" } ] },
    })
}

fn decoded_patches(response: &Value) -> Vec<Value> {
    let encoded = response
        .pointer("/response/patch")
        .and_then(Value::as_str)
        .expect("a patch");

    // Undo the webhook's own base64 — the tests must not trust it, so
    // this is an independent decoder over the standard alphabet.
    let table: Vec<u8> = (b'A'..=b'Z')
        .chain(b'a'..=b'z')
        .chain(b'0'..=b'9')
        .chain(*b"+/")
        .collect();
    let index = |c: u8| table.iter().position(|&t| t == c).unwrap() as u32;

    let clean: Vec<u8> = encoded.bytes().filter(|&b| b != b'=').collect();
    let mut bytes = Vec::new();

    for chunk in clean.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= index(c) << (18 - 6 * i);
        }
        bytes.push((n >> 16) as u8);
        if chunk.len() > 2 {
            bytes.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            bytes.push(n as u8);
        }
    }

    serde_json::from_slice(&bytes).expect("the patch is JSON")
}

#[test]
fn a_pod_that_did_not_ask_passes_untouched() {
    let response = dynamic_config_webhook::admission_response(&review(json!({
        "metadata": {}, "spec": { "containers": [] },
    })));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));
    assert!(response.pointer("/response/patch").is_none());
    assert_eq!(
        response.pointer("/response/uid"),
        Some(&json!("test-uid-1"))
    );
}

#[test]
fn a_wrong_ask_fails_the_admission_rather_than_skipping() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/path"] = Value::Null;
    pod["metadata"]["annotations"]
        .as_object_mut()
        .unwrap()
        .remove("dynamic-config.rs/path");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .expect("a reason");

    assert!(message.contains("path"), "{message}");
}

#[test]
fn sidecar_mode_injects_volume_mount_and_container() {
    let response = dynamic_config_webhook::admission_response(&review(annotated("sidecar")));
    let patches = decoded_patches(&response);

    let adds: Vec<&str> = patches
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();

    assert!(adds.contains(&"/spec/volumes"), "{adds:?}");
    assert!(adds.contains(&"/spec/volumes/-"));
    assert!(adds.contains(&"/spec/containers/0/volumeMounts"));
    assert!(adds.contains(&"/spec/containers/-"), "the sidecar itself");
    assert!(
        !adds.contains(&"/spec/initContainers/-"),
        "sidecar mode has no init"
    );

    // The agent's arguments are the annotation contract, verbatim.
    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let arguments = sidecar["value"]["args"].as_array().unwrap();

    assert!(arguments.contains(&json!("--watch")));
    assert!(arguments.contains(&json!("/config/rendered.toml")));
}

#[test]
fn both_mode_injects_init_and_sidecar() {
    let response = dynamic_config_webhook::admission_response(&review(annotated("both")));
    let patches = decoded_patches(&response);

    let adds: Vec<&str> = patches
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();

    assert!(adds.contains(&"/spec/initContainers"));
    assert!(adds.contains(&"/spec/initContainers/-"));
    assert!(adds.contains(&"/spec/containers/-"));

    let init = patches
        .iter()
        .find(|p| p["path"] == "/spec/initContainers/-")
        .unwrap();
    let arguments = init["value"]["args"].as_array().unwrap();

    assert!(!arguments.contains(&json!("--watch")), "init runs once");
}

#[test]
fn the_golden_file_is_the_contract() {
    // The full response for the canonical sidecar pod, byte-compared.
    // A change here is a change to the ANNOTATION CONTRACT and gets
    // reviewed as one — regenerate with:
    //   cargo test -p dynamic-config-webhook regenerate -- --ignored
    let response = dynamic_config_webhook::admission_response(&review(annotated("sidecar")));
    let rendered = serde_json::to_string_pretty(&response).expect("serialises");

    let golden = include_str!("fixtures/sidecar-response.json");

    assert_eq!(rendered.trim(), golden.trim(), "the contract moved");
}

#[test]
fn auth_annotations_become_agent_flags_in_order() {
    let response = dynamic_config_webhook::admission_response(&review(vault_kubernetes_pod()));
    let patches = decoded_patches(&response);

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let arguments: Vec<&str> = sidecar["value"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();

    let auth = arguments.iter().position(|&a| a == "--auth").unwrap();
    assert_eq!(arguments[auth + 1], "kubernetes");

    let role = arguments.iter().position(|&a| a == "--auth-role").unwrap();
    assert_eq!(arguments[role + 1], "myapp");

    let ca = arguments.iter().position(|&a| a == "--ca").unwrap();
    assert_eq!(arguments[ca + 1], "/etc/dynamic-config/ca/ca.crt");
}

#[test]
fn the_ca_rides_its_own_volume_into_the_agent_alone() {
    let response = dynamic_config_webhook::admission_response(&review(vault_kubernetes_pod()));
    let patches = decoded_patches(&response);

    let volumes: Vec<&Value> = patches
        .iter()
        .filter(|p| p["path"] == "/spec/volumes/-")
        .collect();

    assert!(
        volumes
            .iter()
            .any(|v| v["value"]["configMap"]["name"] == "vault-ca"),
        "{volumes:?}"
    );

    // The application container gets the rendered-file mount and ONLY
    // that: the CA is the agent's business.
    let app_mount = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/0/volumeMounts/-")
        .unwrap();
    assert_eq!(app_mount["value"]["name"], "dynamic-config");

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let mounts = sidecar["value"]["volumeMounts"].as_array().unwrap();
    assert_eq!(mounts.len(), 2, "{mounts:?}");
    assert!(mounts.iter().any(|m| m["name"] == "dynamic-config-ca"));
}

#[test]
fn a_token_secret_becomes_environment_not_arguments() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/token-secret"] = json!("consul-token/token");

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();

    let env = sidecar["value"]["env"].as_array().expect("env is wired");
    assert_eq!(env[0]["name"], "DYNAMIC_CONFIG_AGENT_TOKEN");
    assert_eq!(
        env[0]["valueFrom"]["secretKeyRef"],
        json!({ "name": "consul-token", "key": "token" })
    );

    let rendered = serde_json::to_string(&sidecar["value"]["args"]).unwrap();
    assert!(
        !rendered.contains("token"),
        "a secret name leaked into arguments: {rendered}"
    );
}

#[test]
fn an_endpoint_secret_replaces_the_endpoint_flag() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.remove("dynamic-config.rs/endpoint");
    notes.insert(
        "dynamic-config.rs/endpoint-secret".to_owned(),
        json!("redis-url/url"),
    );
    notes.insert("dynamic-config.rs/source".to_owned(), json!("redis"));

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();

    let arguments = serde_json::to_string(&sidecar["value"]["args"]).unwrap();
    assert!(!arguments.contains("--endpoint"), "{arguments}");

    let env = sidecar["value"]["env"].as_array().expect("env is wired");
    assert_eq!(env[0]["name"], "DYNAMIC_CONFIG_AGENT_ENDPOINT");
}

#[test]
fn a_malformed_secret_reference_fails_the_admission() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/token-secret"] = json!("no-key-here");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("<secret-name>/<key>"), "{message}");
}

#[test]
fn an_ssh_secret_implies_its_auth_method() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/source".to_owned(), json!("git"));
    notes.insert(
        "dynamic-config.rs/endpoint".to_owned(),
        json!("ssh://git@github.com/acme/config.git"),
    );
    notes.insert(
        "dynamic-config.rs/ssh-secret".to_owned(),
        json!("deploy-key"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let arguments: Vec<&str> = sidecar["value"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();

    let auth = arguments.iter().position(|&a| a == "--auth").unwrap();
    assert_eq!(arguments[auth + 1], "ssh-key");

    let key = arguments.iter().position(|&a| a == "--ssh-key").unwrap();
    assert_eq!(
        arguments[key + 1],
        "/etc/dynamic-config/ssh/ssh-privatekey",
        "the kubernetes.io/ssh-auth convention"
    );

    // And the key file arrives with owner-only permissions, which ssh
    // refuses to work without.
    let ssh_volume = patches
        .iter()
        .filter(|p| p["path"] == "/spec/volumes/-")
        .find(|p| p["value"]["name"] == "dynamic-config-ssh")
        .expect("the ssh volume");
    assert_eq!(ssh_volume["value"]["secret"]["defaultMode"], 256);
}

#[test]
fn a_tls_secret_mounts_the_pair_the_type_promises() {
    let mut pod = vault_kubernetes_pod();
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/auth".to_owned(), json!("cert"));
    notes.remove("dynamic-config.rs/auth-role");
    notes.insert(
        "dynamic-config.rs/tls-secret".to_owned(),
        json!("vault-client"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let arguments: Vec<&str> = sidecar["value"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();

    let cert = arguments.iter().position(|&a| a == "--tls-cert").unwrap();
    assert_eq!(arguments[cert + 1], "/etc/dynamic-config/tls/tls.crt");

    let key = arguments.iter().position(|&a| a == "--tls-key").unwrap();
    assert_eq!(arguments[key + 1], "/etc/dynamic-config/tls/tls.key");

    let volume = patches
        .iter()
        .filter(|p| p["path"] == "/spec/volumes/-")
        .find(|p| p["value"]["name"] == "dynamic-config-client-tls")
        .expect("the client-tls volume");
    assert_eq!(volume["value"]["secret"]["secretName"], "vault-client");
}

#[test]
fn the_injected_agent_carries_the_restricted_posture() {
    let response = dynamic_config_webhook::admission_response(&review(annotated("sidecar")));
    let patches = decoded_patches(&response);

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let security = &sidecar["value"]["securityContext"];

    assert_eq!(security["runAsNonRoot"], json!(true));
    assert_eq!(security["runAsUser"], json!(65532));
    assert_eq!(security["allowPrivilegeEscalation"], json!(false));
    assert_eq!(security["capabilities"]["drop"], json!(["ALL"]));
    assert_eq!(security["readOnlyRootFilesystem"], json!(true));
    assert_eq!(security["seccompProfile"]["type"], json!("RuntimeDefault"));

    let resources = &sidecar["value"]["resources"];
    assert_eq!(resources["requests"]["cpu"], json!("10m"));
    assert_eq!(resources["limits"]["memory"], json!("64Mi"));
    assert!(
        resources["limits"]["cpu"].is_null(),
        "no CPU limit unless asked: throttling a config agent buys nothing"
    );
}

#[test]
fn the_volume_is_memory_backed_unless_asked_otherwise() {
    let response = dynamic_config_webhook::admission_response(&review(annotated("sidecar")));
    let patches = decoded_patches(&response);

    let volume = patches
        .iter()
        .filter(|p| p["path"] == "/spec/volumes/-")
        .find(|p| p["value"]["name"] == "dynamic-config")
        .unwrap();
    assert_eq!(volume["value"]["emptyDir"]["medium"], json!("Memory"));

    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/volume-medium"] = json!("disk");

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let volume = patches
        .iter()
        .filter(|p| p["path"] == "/spec/volumes/-")
        .find(|p| p["value"]["name"] == "dynamic-config")
        .unwrap();
    assert_eq!(volume["value"]["emptyDir"], json!({}));
}

#[test]
fn resource_annotations_move_the_ask() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert(
        "dynamic-config.rs/agent-memory-limit".to_owned(),
        json!("256Mi"),
    );
    notes.insert(
        "dynamic-config.rs/agent-cpu-limit".to_owned(),
        json!("200m"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let limits = &sidecar["value"]["resources"]["limits"];

    assert_eq!(limits["memory"], json!("256Mi"));
    assert_eq!(limits["cpu"], json!("200m"));
}

#[test]
fn a_nonsense_quantity_fails_the_admission() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/agent-memory-limit"] = json!("lots please");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

#[test]
fn a_native_sidecar_is_an_init_container_that_restarts() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/native-sidecar"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let adds: Vec<&str> = patches
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();

    assert!(
        !adds.contains(&"/spec/containers/-"),
        "the watcher moved out of containers: {adds:?}"
    );

    let watcher = patches
        .iter()
        .find(|p| p["path"] == "/spec/initContainers/-")
        .unwrap();
    assert_eq!(watcher["value"]["restartPolicy"], json!("Always"));
    assert!(watcher["value"]["args"]
        .as_array()
        .unwrap()
        .contains(&json!("--watch")));
}

#[test]
fn both_mode_with_native_sidecar_keeps_the_first_render_guarantee() {
    let mut pod = annotated("both");
    pod["metadata"]["annotations"]["dynamic-config.rs/native-sidecar"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let inits: Vec<&Value> = patches
        .iter()
        .filter(|p| p["path"] == "/spec/initContainers/-")
        .collect();

    assert_eq!(inits.len(), 2, "{inits:?}");
    // The one-shot lands first: it blocks until the file exists, which
    // is what `both` promises; the watcher then runs alongside the app.
    assert_eq!(inits[0]["value"]["name"], "dynamic-config-init");
    assert_eq!(inits[1]["value"]["name"], "dynamic-config-agent");
    assert_eq!(inits[1]["value"]["restartPolicy"], json!("Always"));
}

#[test]
fn a_typoed_annotation_fails_the_admission() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/tokne-secret"] = json!("consul-token/token");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(
        response.pointer("/response/allowed"),
        Some(&json!(false)),
        "a typo silently ignored is a pod without the auth it declared"
    );

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("tokne-secret"), "{message}");
}

#[test]
fn annotations_outside_the_prefix_are_not_ours_to_judge() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["prometheus.io/scrape"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert!(response.pointer("/response/patch").is_some());
}

#[test]
fn an_inline_template_becomes_the_inline_flag() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/template"] =
        json!("db={{ db.host }}:{{ db.port }}");
    pod["metadata"]["annotations"]["dynamic-config.rs/path"] = json!("/config/app.env");

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let arguments: Vec<&str> = sidecar["value"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();

    let flag = arguments
        .iter()
        .position(|&a| a == "--template-inline")
        .unwrap();
    assert_eq!(arguments[flag + 1], "db={{ db.host }}:{{ db.port }}");
}

#[test]
fn a_template_configmap_is_mounted_and_pointed_at() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/template-configmap"] =
        json!("billing-template");

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let volume = patches
        .iter()
        .filter(|p| p["path"] == "/spec/volumes/-")
        .find(|p| p["value"]["name"] == "dynamic-config-template")
        .expect("the template volume");
    assert_eq!(volume["value"]["configMap"]["name"], "billing-template");

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let arguments: Vec<&str> = sidecar["value"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();

    let flag = arguments.iter().position(|&a| a == "--template").unwrap();
    assert_eq!(
        arguments[flag + 1],
        "/etc/dynamic-config/template/template",
        "the default key"
    );

    let mounts = sidecar["value"]["volumeMounts"].as_array().unwrap();
    assert!(mounts
        .iter()
        .any(|m| m["name"] == "dynamic-config-template" && m["readOnly"] == json!(true)));
}

#[test]
fn two_templates_are_refused() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/template"] = json!("x");
    pod["metadata"]["annotations"]["dynamic-config.rs/template-configmap"] = json!("y");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("one template, one place"), "{message}");
}

#[test]
fn the_async_stores_are_refused_at_admission_not_at_crashloop() {
    for store in ["etcd", "nats", "s3"] {
        let mut pod = annotated("sidecar");
        pod["metadata"]["annotations"]["dynamic-config.rs/source"] = json!(store);

        let response = dynamic_config_webhook::admission_response(&review(pod));

        assert_eq!(
            response.pointer("/response/allowed"),
            Some(&json!(false)),
            "{store}"
        );

        let message = response
            .pointer("/response/status/message")
            .and_then(Value::as_str)
            .unwrap();
        assert!(message.contains("0.2.0"), "{store}: {message}");
    }
}

#[test]
fn the_second_golden_file_is_the_auth_contract() {
    let response = dynamic_config_webhook::admission_response(&review(vault_kubernetes_pod()));
    let rendered = serde_json::to_string_pretty(&response).expect("serialises");

    let golden = include_str!("fixtures/vault-kubernetes-response.json");

    assert_eq!(rendered.trim(), golden.trim(), "the contract moved");
}

#[test]
#[ignore = "writes the golden file; run when the contract changes on purpose"]
fn regenerate() {
    let response = dynamic_config_webhook::admission_response(&review(annotated("sidecar")));

    std::fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sidecar-response.json"
        ),
        serde_json::to_string_pretty(&response).expect("serialises"),
    )
    .expect("written");

    let response = dynamic_config_webhook::admission_response(&review(vault_kubernetes_pod()));

    std::fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/vault-kubernetes-response.json"
        ),
        serde_json::to_string_pretty(&response).expect("serialises"),
    )
    .expect("written");
}
