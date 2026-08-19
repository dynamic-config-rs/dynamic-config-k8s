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
fn env_inject_wraps_the_named_command() {
    let mut pod = annotated("init");
    pod["metadata"]["annotations"]["dynamic-config.rs/path"] = json!("/config/app.env");
    pod["metadata"]["annotations"]["dynamic-config.rs/template"] = json!("DB_HOST={{ db.host }}\n");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-inject"] = json!("app");
    pod["spec"]["containers"][0]["command"] = json!(["airflow", "scheduler"]);
    pod["spec"]["containers"][0]["args"] = json!(["--pid", "/tmp/pid"]);

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let command = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/0/command")
        .expect("the wrap");

    assert_eq!(
        command["value"],
        json!([
            "/bin/sh",
            "-c",
            "set -a; . /config/app.env; set +a; exec \"$@\"",
            "dynamic-config-env",
            "airflow",
            "scheduler",
            "--pid",
            "/tmp/pid"
        ])
    );
    assert!(
        patches
            .iter()
            .any(|p| p["op"] == "remove" && p["path"] == "/spec/containers/0/args"),
        "args folded into the wrap"
    );
}

#[test]
fn an_aws_secret_lands_as_the_two_standard_variables() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/source".to_owned(), json!("s3"));
    notes.insert(
        "dynamic-config.rs/endpoint".to_owned(),
        json!("myapp-config"),
    );
    notes.insert(
        "dynamic-config.rs/api-url".to_owned(),
        json!("http://minio.infra.svc:9000"),
    );
    notes.insert(
        "dynamic-config.rs/aws-secret".to_owned(),
        json!("minio-cred"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);
    let agent = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let env = agent["value"]["env"].as_array().unwrap();
    let names: Vec<&str> = env.iter().filter_map(|e| e["name"].as_str()).collect();

    assert!(names.contains(&"AWS_ACCESS_KEY_ID"));
    assert!(names.contains(&"AWS_SECRET_ACCESS_KEY"));
    assert!(env.iter().all(
        |e| e["valueFrom"]["secretKeyRef"]["name"] == json!("minio-cred")
            || e["name"] != json!("AWS_ACCESS_KEY_ID")
    ));

    // …and on any other source it is refused by name.
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/aws-secret"] = json!("minio-cred");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

#[test]
fn named_renders_multiply_the_agents() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/source.cache".to_owned(), json!("redis"));
    notes.insert(
        "dynamic-config.rs/endpoint-secret.cache".to_owned(),
        json!("redis-url/url"),
    );
    notes.insert(
        "dynamic-config.rs/key.cache".to_owned(),
        json!("myapp/cache.json"),
    );
    notes.insert(
        "dynamic-config.rs/path.cache".to_owned(),
        json!("/config/cache.toml"),
    );
    notes.insert(
        "dynamic-config.rs/ca-configmap.cache".to_owned(),
        json!("redis-ca"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let agents: Vec<&Value> = patches
        .iter()
        .filter(|p| p["path"] == "/spec/containers/-")
        .collect();

    assert_eq!(agents.len(), 2, "the default agent and the named one");
    assert_eq!(
        agents[1]["value"]["name"],
        json!("dynamic-config-agent-cache")
    );

    let args: Vec<&str> = agents[1]["value"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();

    assert!(args.contains(&"redis"));
    assert!(args.contains(&"/config/cache.toml"));
    assert!(
        args.contains(&"/etc/dynamic-config/ca-cache/ca.crt"),
        "the aux mount is suffixed: {args:?}"
    );

    // Its endpoint secret rides ITS container's env, nobody else's.
    assert_eq!(
        agents[1]["value"]["env"][0]["name"],
        json!("DYNAMIC_CONFIG_AGENT_ENDPOINT")
    );

    // And the suffixed CA volume exists.
    assert!(patches.iter().any(|p| {
        p["path"] == "/spec/volumes/-" && p["value"]["name"] == "dynamic-config-ca-cache"
    }));
}

#[test]
fn a_named_render_outside_the_shared_directory_is_refused() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/source.cache".to_owned(), json!("redis"));
    notes.insert(
        "dynamic-config.rs/endpoint.cache".to_owned(),
        json!("redis://cache:6379"),
    );
    notes.insert(
        "dynamic-config.rs/key.cache".to_owned(),
        json!("myapp/cache.json"),
    );
    notes.insert(
        "dynamic-config.rs/path.cache".to_owned(),
        json!("/elsewhere/cache.toml"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("ONE volume"));
}

#[test]
fn a_named_render_missing_its_key_is_refused_by_name() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/source.cache".to_owned(), json!("redis"));
    notes.insert(
        "dynamic-config.rs/endpoint.cache".to_owned(),
        json!("redis://cache:6379"),
    );
    notes.insert(
        "dynamic-config.rs/path.cache".to_owned(),
        json!("/config/cache.toml"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("key.cache"));
}

#[test]
fn env_restart_exports_a_fingerprint_and_probes_it() {
    let mut pod = annotated("both");
    pod["metadata"]["annotations"]["dynamic-config.rs/path"] = json!("/config/app.env");
    pod["metadata"]["annotations"]["dynamic-config.rs/template"] = json!("A={{ a }}\n");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-inject"] = json!("app");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-restart"] = json!("true");
    pod["metadata"]["annotations"]["dynamic-config.rs/watch-seconds"] = json!("30");
    pod["spec"]["containers"][0]["command"] = json!(["serve"]);

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let command = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/0/command")
        .expect("the wrap");
    let script = command["value"][2].as_str().unwrap();

    assert!(
        script.contains("DYNAMIC_CONFIG_ENV_FINGERPRINT"),
        "{script}"
    );
    assert!(script.contains("cksum < /config/app.env"), "{script}");

    let probe = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/0/livenessProbe")
        .expect("the restart trigger");

    assert_eq!(probe["value"]["periodSeconds"], json!(30));
    assert_eq!(probe["value"]["failureThreshold"], json!(1));
    assert!(probe["value"]["exec"]["command"][2]
        .as_str()
        .unwrap()
        .contains("$DYNAMIC_CONFIG_ENV_FINGERPRINT"));
}

#[test]
fn env_restart_refusals_name_the_fix() {
    // Without env-inject there is nothing to restart for.
    let mut pod = annotated("both");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-restart"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    // Init alone never changes the file again.
    let mut pod = annotated("init");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-inject"] = json!("app");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-restart"] = json!("true");
    pod["spec"]["containers"][0]["command"] = json!(["serve"]);

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("both"));

    // A livenessProbe the container already owns cannot be shared.
    let mut pod = annotated("both");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-inject"] = json!("app");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-restart"] = json!("true");
    pod["spec"]["containers"][0]["command"] = json!(["serve"]);
    pod["spec"]["containers"][0]["livenessProbe"] =
        json!({ "httpGet": { "path": "/health", "port": 8080 } });

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("livenessProbe"));
}

#[test]
fn env_inject_refusals_name_the_fix() {
    // Sidecar mode: the file would arrive after the app started.
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-inject"] = json!("app");
    pod["spec"]["containers"][0]["command"] = json!(["run"]);

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("mode"));

    // No explicit command: the ENTRYPOINT is invisible to the webhook.
    let mut pod = annotated("init");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-inject"] = json!("app");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("ENTRYPOINT"));

    // A container the pod does not have.
    let mut pod = annotated("init");
    pod["metadata"]["annotations"]["dynamic-config.rs/env-inject"] = json!("nope");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

#[test]
fn file_mode_and_identity_reach_the_agent() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/file-mode".to_owned(), json!("0640"));
    notes.insert(
        "dynamic-config.rs/agent-run-as-user".to_owned(),
        json!("1000"),
    );
    notes.insert(
        "dynamic-config.rs/agent-run-as-group".to_owned(),
        json!("1000"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    let sidecar = patches
        .iter()
        .find(|p| p["path"] == "/spec/containers/-")
        .unwrap();
    let args: Vec<&str> = sidecar["value"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mode_at = args.iter().position(|a| *a == "--file-mode").unwrap();

    assert_eq!(args[mode_at + 1], "0640");

    let security = &sidecar["value"]["securityContext"];

    assert_eq!(security["runAsUser"], json!(1000));
    assert_eq!(security["runAsGroup"], json!(1000));
    assert_eq!(security["runAsNonRoot"], json!(true), "posture holds");
}

#[test]
fn root_and_nonsense_modes_are_refused_at_admission() {
    for (name, value, says) in [
        ("dynamic-config.rs/agent-run-as-user", "0", "nonroot"),
        ("dynamic-config.rs/file-mode", "888", "octal"),
        ("dynamic-config.rs/file-mode", "0200", "read"),
    ] {
        let mut pod = annotated("sidecar");
        pod["metadata"]["annotations"][name] = json!(value);

        let response = dynamic_config_webhook::admission_response(&review(pod));

        assert_eq!(
            response.pointer("/response/allowed"),
            Some(&json!(false)),
            "{name}={value}"
        );

        let message = response
            .pointer("/response/status/message")
            .and_then(Value::as_str)
            .unwrap();

        assert!(message.contains(says), "{name}={value}: {message}");
    }
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
fn the_async_stores_are_admitted_since_0_2_0() {
    // Since 0.2.0 the async stores are admitted like any other: the
    // refusal-by-name this test used to pin retired with the agent's
    // async path.
    for store in ["etcd", "nats", "s3"] {
        let mut pod = annotated("sidecar");
        pod["metadata"]["annotations"]["dynamic-config.rs/source"] = json!(store);

        let response = dynamic_config_webhook::admission_response(&review(pod));

        assert_eq!(
            response.pointer("/response/allowed"),
            Some(&json!(true)),
            "{store}"
        );
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

/// An [`Installation`] from pairs, the way the server builds one from
/// its environment — same validation, same defaults.
fn install(pairs: &[(&str, &str)]) -> dynamic_config_webhook::Installation {
    dynamic_config_webhook::Installation::from_lookup(&|name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    })
    .expect("a valid installation")
}

/// `annotated`, plus an agent-env annotation and a namespace on the
/// review — the gate judges names against where the pod is landing.
fn agent_env_review(namespace: &str, entries: &str) -> Value {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/agent-env"] = json!(entries);

    let mut review = review(pod);
    review["request"]["namespace"] = json!(namespace);
    review
}

#[test]
fn agent_env_is_refused_until_the_installer_opens_the_gate() {
    // No allowlist configured: the default installation refuses every
    // agent-env, and the message names the chart value that opens it.
    let response =
        dynamic_config_webhook::admission_response(&agent_env_review("default", "RUST_LOG=debug"));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("agentEnvAllow"), "{message}");
    assert!(message.contains("RUST_LOG"), "{message}");
}

#[test]
fn agent_env_flows_once_allowed_and_lands_on_every_agent() {
    let allow = install(&[(
        "DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW",
        "*: RUST_LOG, AWS_*",
    )]);
    let response = dynamic_config_webhook::admission_response_with(
        &agent_env_review(
            "default",
            "RUST_LOG=debug, AWS_CA_BUNDLE=/etc/ssl/private-ca.pem",
        ),
        &allow,
    );

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));

    let patches = decoded_patches(&response);
    let agents: Vec<&Value> = patches
        .iter()
        .filter_map(|p| p.pointer("/value"))
        .filter(|v| {
            v.pointer("/name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.starts_with("dynamic-config-"))
        })
        .collect();

    assert!(!agents.is_empty());

    for agent in agents {
        let env = agent.pointer("/env").and_then(Value::as_array).unwrap();

        assert!(env.contains(&json!({ "name": "RUST_LOG", "value": "debug" })));
        assert!(env.contains(&json!({
            "name": "AWS_CA_BUNDLE", "value": "/etc/ssl/private-ca.pem",
        })));
    }
}

#[test]
fn agent_env_gate_is_namespace_scoped() {
    let allow = install(&[(
        "DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW",
        "payments: HTTPS_PROXY",
    )]);
    let entries = "HTTPS_PROXY=http://egress.infra.svc:3128";

    let inside = dynamic_config_webhook::admission_response_with(
        &agent_env_review("payments", entries),
        &allow,
    );
    assert_eq!(inside.pointer("/response/allowed"), Some(&json!(true)));

    let outside = dynamic_config_webhook::admission_response_with(
        &agent_env_review("default", entries),
        &allow,
    );
    assert_eq!(outside.pointer("/response/allowed"), Some(&json!(false)));

    let message = outside
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("\"default\""), "{message}");
}

#[test]
fn agent_env_entries_must_be_name_value_pairs() {
    for (entries, expected) in [
        ("RUST_LOG", "NAME=value"),
        ("rust-log=debug", "UPPER_SNAKE"),
        ("RUST_LOG=a, RUST_LOG=b", "twice"),
        (" ", "names nothing"),
    ] {
        let response =
            dynamic_config_webhook::admission_response(&agent_env_review("default", entries));

        assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

        let message = response
            .pointer("/response/status/message")
            .and_then(Value::as_str)
            .unwrap();
        assert!(message.contains(expected), "{entries:?}: {message}");
    }
}

#[test]
fn agent_env_does_not_shadow_what_aws_secret_sets() {
    let mut pod = annotated("sidecar");
    let notes = &mut pod["metadata"]["annotations"];
    notes["dynamic-config.rs/source"] = json!("s3");
    notes["dynamic-config.rs/aws-secret"] = json!("minio-cred");
    notes["dynamic-config.rs/agent-env"] = json!("AWS_ACCESS_KEY_ID=stolen");

    let allow = install(&[("DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW", "*: *")]);
    let response = dynamic_config_webhook::admission_response_with(&review(pod), &allow);

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("one credential, one place"), "{message}");
}

fn namespaced_review(namespace: &str, pod: Value) -> Value {
    let mut review = review(pod);
    review["request"]["namespace"] = json!(namespace);
    review
}

#[test]
fn a_denied_source_is_refused_where_the_deny_says() {
    let gates = install(&[("DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY", "sandbox: consul")]);

    let refused = dynamic_config_webhook::admission_response_with(
        &namespaced_review("sandbox", annotated("sidecar")),
        &gates,
    );
    assert_eq!(refused.pointer("/response/allowed"), Some(&json!(false)));

    let message = refused
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("sourceDeny"), "{message}");

    let elsewhere = dynamic_config_webhook::admission_response_with(
        &namespaced_review("default", annotated("sidecar")),
        &gates,
    );
    assert_eq!(elsewhere.pointer("/response/allowed"), Some(&json!(true)));
}

#[test]
fn a_source_allowlist_admits_only_what_it_names() {
    let gates = install(&[(
        "DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW",
        "payments: vault; *: consul",
    )]);

    // consul is open everywhere; vault only in payments.
    let consul = dynamic_config_webhook::admission_response_with(
        &namespaced_review("default", annotated("sidecar")),
        &gates,
    );
    assert_eq!(consul.pointer("/response/allowed"), Some(&json!(true)));

    let vault_outside = dynamic_config_webhook::admission_response_with(
        &namespaced_review("default", vault_kubernetes_pod()),
        &gates,
    );
    assert_eq!(
        vault_outside.pointer("/response/allowed"),
        Some(&json!(false))
    );

    let message = vault_outside
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("sourceAllow"), "{message}");
    assert!(
        message.contains("consul"),
        "names the allowed set: {message}"
    );

    let vault_inside = dynamic_config_webhook::admission_response_with(
        &namespaced_review("payments", vault_kubernetes_pod()),
        &gates,
    );
    assert_eq!(
        vault_inside.pointer("/response/allowed"),
        Some(&json!(true))
    );
}

#[test]
fn the_deny_gate_covers_named_renders_too() {
    let gates = install(&[("DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY", "*: redis")]);

    let mut pod = annotated("sidecar");
    let notes = &mut pod["metadata"]["annotations"];
    notes["dynamic-config.rs/source.cache"] = json!("redis");
    notes["dynamic-config.rs/endpoint.cache"] = json!("redis://redis:6379");
    notes["dynamic-config.rs/key.cache"] = json!("myapp");
    notes["dynamic-config.rs/path.cache"] = json!("/config/cache.toml");

    let response =
        dynamic_config_webhook::admission_response_with(&namespaced_review("default", pod), &gates);
    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

/// The resolution ladder: annotation over store default over fleet
/// default over built-in — checked on the rendered agent container.
#[test]
fn store_defaults_sit_between_annotations_and_fleet_defaults() {
    let tiers = install(&[
        ("DYNAMIC_CONFIG_AGENT_WATCH_SECONDS", "60"),
        ("DYNAMIC_CONFIG_AGENT_FILE_MODE", "0600"),
        ("DYNAMIC_CONFIG_AGENT_RUN_AS_USER", "2000"),
        ("DYNAMIC_CONFIG_AGENT_METRICS_PORT", "9102"),
        (
            "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
            "consul: watch-seconds=7, file-mode=0640",
        ),
    ]);

    let request = dynamic_config_webhook::of_pod_with(&annotated("sidecar"), &tiers)
        .unwrap()
        .unwrap();

    // consul has a store default: it wins over the fleet's 60/0600.
    assert_eq!(request.watch_seconds, 7);
    assert_eq!(request.file_mode.as_deref(), Some("640"));
    // No consul entry for these: the fleet default holds.
    assert_eq!(request.run_as_user, Some(2000));
    assert_eq!(request.metrics_port, Some(9102));

    // And the annotation still outranks both tiers.
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/watch-seconds"] = json!("3");
    pod["metadata"]["annotations"]["dynamic-config.rs/metrics-port"] = json!("0");

    let request = dynamic_config_webhook::of_pod_with(&pod, &tiers)
        .unwrap()
        .unwrap();

    assert_eq!(request.watch_seconds, 3);
    // metrics-port "0" is the per-pod opt-out of the fleet default.
    assert_eq!(request.metrics_port, None);
}

#[test]
fn fleet_defaults_cover_mode_volume_and_sidecar_shape() {
    let tiers = install(&[
        ("DYNAMIC_CONFIG_AGENT_MODE", "both"),
        ("DYNAMIC_CONFIG_AGENT_VOLUME_MEDIUM", "disk"),
        ("DYNAMIC_CONFIG_AGENT_NATIVE_SIDECAR", "true"),
    ]);

    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]
        .as_object_mut()
        .unwrap()
        .remove("dynamic-config.rs/mode");

    let request = dynamic_config_webhook::of_pod_with(&pod, &tiers)
        .unwrap()
        .unwrap();

    assert_eq!(request.mode, dynamic_config_webhook::Mode::Both);
    assert!(!request.volume_memory);
    assert!(request.native_sidecar);
}

#[test]
fn fleet_environment_rides_under_the_pods_own() {
    let tiers = install(&[
        (
            "DYNAMIC_CONFIG_AGENT_ENV",
            "HTTPS_PROXY=http://egress:3128, RUST_LOG=info",
        ),
        ("DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW", "*: RUST_LOG"),
    ]);

    // The pod overrides RUST_LOG; HTTPS_PROXY arrives from the fleet
    // without needing the allowlist — the installer set it themselves.
    let response = dynamic_config_webhook::admission_response_with(
        &agent_env_review("default", "RUST_LOG=debug"),
        &tiers,
    );
    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));

    let patches = decoded_patches(&response);
    let agent = patches
        .iter()
        .filter_map(|p| p.pointer("/value"))
        .find(|v| v.pointer("/name") == Some(&json!("dynamic-config-agent")))
        .unwrap();
    let env = agent.pointer("/env").and_then(Value::as_array).unwrap();

    assert!(env.contains(&json!({ "name": "HTTPS_PROXY", "value": "http://egress:3128" })));
    assert!(env.contains(&json!({ "name": "RUST_LOG", "value": "debug" })));
    assert!(!env.contains(&json!({ "name": "RUST_LOG", "value": "info" })));
}
