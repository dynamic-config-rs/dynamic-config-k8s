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
    // `medium` absent is the node's disk; the size limit rides both media,
    // because an unbounded volume on disk fills the node instead of the
    // pod and is no better for it.
    assert!(volume["value"]["emptyDir"]["medium"].is_null());
    assert_eq!(volume["value"]["emptyDir"]["sizeLimit"], json!("16Mi"));
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

/// A pod with a sidecar beside its application: the log shipper has no
/// business holding a rendered credential, and `file-mode` cannot draw
/// that line — it runs as the same UID.
#[test]
fn only_the_named_containers_receive_the_rendered_volume() {
    let mut pod = annotated("sidecar");
    pod["spec"]["containers"] = json!([
        { "name": "app", "image": "app:1" },
        { "name": "logs", "image": "fluent-bit:3" },
    ]);
    pod["metadata"]["annotations"]["dynamic-config.rs/inject-containers"] = json!("app");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));

    let patches = decoded_patches(&response);
    let mounted: Vec<&str> = patches
        .iter()
        .filter(|patch| {
            patch["value"]["name"] == json!("dynamic-config")
                && patch["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("/volumeMounts/-"))
        })
        .filter_map(|patch| patch["path"].as_str())
        .collect();

    assert_eq!(
        mounted,
        vec!["/spec/containers/0/volumeMounts/-"],
        "only the application was named, so only the application is mounted"
    );
}

/// The default is unchanged, and stays unchanged: every container, which
/// is what the reference implementation defaults to as well. The option
/// exists to narrow it, never to be the thing somebody has to remember.
#[test]
fn without_the_annotation_every_container_receives_it() {
    let mut pod = annotated("sidecar");
    pod["spec"]["containers"] = json!([
        { "name": "app", "image": "app:1" },
        { "name": "logs", "image": "fluent-bit:3" },
    ]);

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);
    let mounted = patches
        .iter()
        .filter(|patch| {
            patch["value"]["name"] == json!("dynamic-config")
                && patch["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("/volumeMounts/-"))
        })
        .count();

    assert_eq!(mounted, 2);
}

/// A typo fails in the worst direction available — the application never
/// sees its configuration and no other container does either — so it is
/// refused, like every other name in this contract.
#[test]
fn inject_containers_naming_nothing_is_refused() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/inject-containers"] = json!("ap");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("no such container"));
}

/// The env wrapper sources the rendered file, so leaving its container out
/// is two annotations disagreeing rather than a configuration.
#[test]
fn env_inject_into_a_container_left_out_is_refused() {
    let mut pod = annotated("both");
    pod["spec"]["containers"] = json!([
        { "name": "app", "image": "app:1", "command": ["/bin/app"] },
        { "name": "logs", "image": "fluent-bit:3" },
    ]);
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/env-inject".to_owned(), json!("app"));
    notes.insert(
        "dynamic-config.rs/inject-containers".to_owned(),
        json!("logs"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("inject-containers"));
}

/// The injected agent's `args`, flattened.
fn agent_arguments(response: &Value) -> Vec<String> {
    decoded_patches(response)
        .iter()
        .find(|patch| {
            patch["path"] == "/spec/containers/-" || patch["path"] == "/spec/initContainers/-"
        })
        .and_then(|patch| patch["value"]["args"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The name to check, when the address is not it. No installation opt-in:
/// this one keeps the server authenticated, it only moves which name is
/// checked.
#[test]
fn a_server_name_reaches_the_agent() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/endpoint"] = json!("https://10.0.0.5:8500");
    pod["metadata"]["annotations"]["dynamic-config.rs/tls-server-name"] = json!("consul.internal");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));
    assert!(agent_arguments(&response).contains(&"consul.internal".to_owned()));
}

/// The one annotation a workload cannot reach on its own. Without the
/// installation saying so, it is refused — and the refusal names the value
/// an administrator sets, because the person reading it cannot set it.
#[test]
fn skipping_verification_is_refused_unless_the_installation_offers_it() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/tls-skip-verify"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();

    assert!(message.contains("allowTlsSkipVerify"), "{message}");
}

/// With the installation's blessing it passes, reaches the agent as a bare
/// flag, and still earns a warning — the pod's author and the person
/// reading `kubectl apply` are often not the same person.
#[test]
fn an_offered_skip_verify_passes_with_a_warning() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/tls-skip-verify"] = json!("true");

    let install = dynamic_config_webhook::Installation::from_lookup(&|name: &str| {
        (name == "DYNAMIC_CONFIG_WEBHOOK_ALLOW_TLS_SKIP_VERIFY").then(|| "true".to_owned())
    })
    .expect("a well-formed installation");

    let response = dynamic_config_webhook::admission_response_with(&review(pod), &install);

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));

    let arguments = agent_arguments(&response);

    assert!(arguments.contains(&"--tls-skip-verify".to_owned()));

    // A bare flag: an empty string after it would reach the agent as an
    // argument it does not recognise.
    let at = arguments
        .iter()
        .position(|argument| argument == "--tls-skip-verify")
        .unwrap();

    assert_ne!(arguments.get(at + 1).map(String::as_str), Some(""));

    let warnings = response
        .pointer("/response/warnings")
        .and_then(Value::as_array)
        .expect("a warning");

    assert!(warnings
        .iter()
        .filter_map(Value::as_str)
        .any(|warning| warning.contains("not authenticated")));
}

/// Naming an authority and then not checking it are two answers to one
/// question, pointing opposite ways.
#[test]
fn skip_verify_alongside_a_certificate_authority_is_refused() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert(
        "dynamic-config.rs/tls-skip-verify".to_owned(),
        json!("true"),
    );
    notes.insert(
        "dynamic-config.rs/ca-configmap".to_owned(),
        json!("consul-ca"),
    );

    let install = dynamic_config_webhook::Installation::from_lookup(&|name: &str| {
        (name == "DYNAMIC_CONFIG_WEBHOOK_ALLOW_TLS_SKIP_VERIFY").then(|| "true".to_owned())
    })
    .expect("a well-formed installation");

    let response = dynamic_config_webhook::admission_response_with(&review(pod), &install);

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("contradictory"));
}

/// A pod that must run something before the configuration exists gets
/// nothing from this; a pod whose own init container reads the rendered
/// file needs it. Appending stays the default for the first case.
#[test]
fn init_first_puts_the_agent_ahead_of_the_pods_own() {
    let mut pod = annotated("init");
    pod["spec"]["initContainers"] = json!([{ "name": "migrate", "image": "migrate:1" }]);
    pod["metadata"]["annotations"]["dynamic-config.rs/init-first"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    assert!(
        patches
            .iter()
            .any(|patch| patch["path"] == "/spec/initContainers/0"),
        "the agent goes in at index 0, ahead of `migrate`"
    );

    // And without it, nothing moves.
    let mut pod = annotated("init");
    pod["spec"]["initContainers"] = json!([{ "name": "migrate", "image": "migrate:1" }]);

    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));

    assert!(patches
        .iter()
        .any(|patch| patch["path"] == "/spec/initContainers/-"));
}

/// `sidecar` mode has no init container, so asking for one to be first is
/// asking about something that is not there.
#[test]
fn init_first_without_an_init_container_is_refused() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/init-first"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

/// The UID comes from the application rather than from a number somebody
/// has to keep in step with it.
#[test]
fn run_as_same_user_takes_the_applications_uid() {
    let mut pod = annotated("sidecar");
    pod["spec"]["containers"] = json!([{
        "name": "app",
        "image": "app:1",
        "securityContext": { "runAsUser": 1234 },
    }]);
    pod["metadata"]["annotations"]["dynamic-config.rs/agent-run-as-same-user"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));

    let agent = decoded_patches(&response)
        .into_iter()
        .find(|patch| patch["path"] == "/spec/containers/-")
        .unwrap();

    assert_eq!(agent["value"]["securityContext"]["runAsUser"], json!(1234));
}

/// Absent is refused rather than guessed: inheriting whatever the image
/// happens to run as is a UID that moves when the image does, silently.
#[test]
fn run_as_same_user_with_nothing_to_inherit_is_refused() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/agent-run-as-same-user"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("no runAsUser to inherit"));
}

/// The agent stays nonroot in every configuration, including this one.
#[test]
fn run_as_same_user_will_not_inherit_root() {
    let mut pod = annotated("sidecar");
    pod["spec"]["containers"] = json!([{
        "name": "app",
        "image": "app:1",
        "securityContext": { "runAsUser": 0 },
    }]);
    pod["metadata"]["annotations"]["dynamic-config.rs/agent-run-as-same-user"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

/// Read-only, into the agent alone, at a path the pod does not choose.
#[test]
fn an_extra_secret_reaches_the_agent_and_nothing_else() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/extra-secret"] = json!("nats-creds");

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let patches = decoded_patches(&response);

    assert!(patches.iter().any(|patch| {
        patch["path"] == "/spec/volumes/-"
            && patch["value"]["secret"]["secretName"] == json!("nats-creds")
    }));

    let agent = patches
        .iter()
        .find(|patch| patch["path"] == "/spec/containers/-")
        .unwrap();
    let mounts = agent["value"]["volumeMounts"].as_array().unwrap();
    let extra = mounts
        .iter()
        .find(|mount| mount["name"] == json!("dynamic-config-extra"))
        .expect("the agent mounts it");

    assert_eq!(extra["readOnly"], json!(true));
    assert_eq!(extra["mountPath"], json!("/etc/dynamic-config/extra"));

    // The application containers get the rendered volume and nothing else.
    let app_mounts: Vec<&Value> = patches
        .iter()
        .filter(|patch| patch["path"] == "/spec/containers/0/volumeMounts/-")
        .collect();

    assert!(app_mounts
        .iter()
        .all(|mount| mount["value"]["name"] == json!("dynamic-config")));
}

/// Past the pod's grace period the kubelet sends SIGKILL first, the
/// revocation is cut off mid-request, and the lease stays out anyway —
/// which is the outcome the annotation was set to avoid.
#[test]
fn a_revoke_grace_past_the_termination_grace_is_refused() {
    let mut pod = annotated("sidecar");
    pod["spec"]["terminationGracePeriodSeconds"] = json!(10);
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/source".to_owned(), json!("vault"));
    notes.insert("dynamic-config.rs/dynamic".to_owned(), json!("true"));
    notes.insert("dynamic-config.rs/revoke-grace".to_owned(), json!("30s"));

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap();

    assert!(message.contains("SIGKILL"), "{message}");
}

/// Inside it, it reaches the agent.
#[test]
fn a_revoke_grace_within_the_termination_grace_reaches_the_agent() {
    let mut pod = annotated("sidecar");
    pod["spec"]["terminationGracePeriodSeconds"] = json!(60);
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/source".to_owned(), json!("vault"));
    notes.insert("dynamic-config.rs/dynamic".to_owned(), json!("true"));
    notes.insert("dynamic-config.rs/revoke-grace".to_owned(), json!("20s"));

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));

    let arguments = agent_arguments(&response);
    let at = arguments
        .iter()
        .position(|argument| argument == "--revoke-grace")
        .expect("the flag reaches the agent");

    assert_eq!(arguments[at + 1], "20");
}

/// The endpoint reaches the watching agent, and a non-localhost one is
/// refused where an operator sees it rather than in a CrashLoopBackOff.
#[test]
fn a_localhost_reload_endpoint_reaches_the_agent() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/notify-http"] =
        json!("http://127.0.0.1:8080/-/reload");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));
    assert!(agent_arguments(&response).contains(&"http://127.0.0.1:8080/-/reload".to_owned()));
}

/// An init container writes once and exits, before the application it
/// would notify has started.
#[test]
fn a_reload_endpoint_on_an_init_only_agent_is_refused() {
    let mut pod = annotated("init");
    pod["metadata"]["annotations"]["dynamic-config.rs/notify-http"] =
        json!("http://127.0.0.1:8080/-/reload");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

#[test]
fn a_drift_policy_reaches_the_agent_and_a_wrong_one_does_not() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/on-drift"] = json!("repair");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));
    assert!(agent_arguments(&response).contains(&"repair".to_owned()));

    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/on-drift"] = json!("fix-it");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

#[test]
fn a_history_depth_reaches_the_agent_and_disk_refuses_it() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/history"] = json!("3");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));

    let arguments = agent_arguments(&response);
    let at = arguments
        .iter()
        .position(|argument| argument == "--history")
        .expect("the flag reaches the agent");

    assert_eq!(arguments[at + 1], "3");

    // On node-backed storage a replaced *secret* would outlive the pod
    // that held it and survive a reboot, so the two together are refused.
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/history".to_owned(), json!("3"));
    notes.insert("dynamic-config.rs/volume-medium".to_owned(), json!("disk"));

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("outlives the pod"));
}

/// The strongest readiness this integration can offer, and the one that
/// needs the application's cooperation — so it is asked for rather than
/// assumed.
#[test]
fn require_ack_reaches_the_agent_and_needs_a_probe_to_read_it() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/require-ack"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));
    assert!(agent_arguments(&response).contains(&"--require-ack".to_owned()));

    // The acknowledgement is only ever read by the readiness probe, so
    // asking for one without a probe asks for nothing.
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert("dynamic-config.rs/require-ack".to_owned(), json!("true"));
    notes.insert("dynamic-config.rs/readiness".to_owned(), json!("false"));

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));

    // And an init container exits before the application starts.
    let mut pod = annotated("init");
    pod["metadata"]["annotations"]["dynamic-config.rs/require-ack"] = json!("true");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

/// A ConfigMap rather than an annotation, because widening the cohort must
/// not be a new pod spec — a rolling restart discards the very state the
/// canary is watching.
#[test]
fn a_canary_configmap_is_mounted_and_named_to_the_agent() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/canary-configmap"] = json!("rollout");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));

    let patches = decoded_patches(&response);

    assert!(patches.iter().any(|patch| {
        patch["path"] == "/spec/volumes/-"
            && patch["value"]["configMap"]["name"] == json!("rollout")
    }));

    let arguments = agent_arguments(&response);
    let at = arguments
        .iter()
        .position(|argument| argument == "--canary")
        .expect("the flag reaches the agent");

    assert_eq!(arguments[at + 1], "/etc/dynamic-config/canary/percent");

    // The cohort is the pod's own name hashed, so the agent has to know it.
    let agent = patches
        .iter()
        .find(|patch| patch["path"] == "/spec/containers/-")
        .unwrap();

    assert!(agent["value"]["env"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == json!("DYNAMIC_CONFIG_POD_NAME")));
}

/// An init container publishes once and exits, so there is no later
/// document for a cohort to hold back.
#[test]
fn a_canary_on_an_init_only_agent_is_refused() {
    let mut pod = annotated("init");
    pod["metadata"]["annotations"]["dynamic-config.rs/canary-configmap"] = json!("rollout");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

/// The one configuration where the rendered volume is charged to a
/// resource nothing here declares: on disk, with a history depth, a pod
/// could fill a node without exceeding any limit it had declared.
#[test]
fn ephemeral_storage_reaches_the_container() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert(
        "dynamic-config.rs/agent-ephemeral-request".to_owned(),
        json!("64Mi"),
    );
    notes.insert(
        "dynamic-config.rs/agent-ephemeral-limit".to_owned(),
        json!("256Mi"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let agent = decoded_patches(&response)
        .into_iter()
        .find(|patch| patch["path"] == "/spec/containers/-")
        .unwrap();

    assert_eq!(
        agent["value"]["resources"]["requests"]["ephemeral-storage"],
        json!("64Mi")
    );
    assert_eq!(
        agent["value"]["resources"]["limits"]["ephemeral-storage"],
        json!("256Mi")
    );

    // And the cpu/memory that were always there are still there.
    assert_eq!(
        agent["value"]["resources"]["requests"]["memory"],
        json!("32Mi")
    );
}

/// A request above its own limit is a pod the scheduler refuses, with a
/// message about the pod rather than the annotation that caused it.
#[test]
fn an_ephemeral_request_above_its_limit_is_refused() {
    let mut pod = annotated("sidecar");
    let notes = pod["metadata"]["annotations"].as_object_mut().unwrap();
    notes.insert(
        "dynamic-config.rs/agent-ephemeral-request".to_owned(),
        json!("1Gi"),
    );
    notes.insert(
        "dynamic-config.rs/agent-ephemeral-limit".to_owned(),
        json!("512Mi"),
    );

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
}

#[test]
fn a_fetch_timeout_reaches_the_agent() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/timeout"] = json!("45");

    let response = dynamic_config_webhook::admission_response(&review(pod));
    let arguments = agent_arguments(&response);
    let at = arguments
        .iter()
        .position(|argument| argument == "--timeout")
        .expect("the flag reaches the agent");

    assert_eq!(arguments[at + 1], "45");
}

/// An arbitrary image on the injected container runs chosen code beside
/// every application that asks, so an installation lists what is allowed.
#[test]
fn a_pod_named_agent_image_needs_the_installation_to_allow_it() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/agent-image"] =
        json!("ghcr.io/acme/agent:next");

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(false)));
    assert!(response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("agentImageAllow"));

    // With a prefix that admits it, it reaches the container.
    let install = dynamic_config_webhook::Installation::from_lookup(&|name: &str| {
        (name == "DYNAMIC_CONFIG_WEBHOOK_AGENT_IMAGE_ALLOW").then(|| "ghcr.io/acme/".to_owned())
    })
    .expect("a well-formed installation");

    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/agent-image"] =
        json!("ghcr.io/acme/agent:next");

    let response = dynamic_config_webhook::admission_response_with(&review(pod), &install);

    assert_eq!(response.pointer("/response/allowed"), Some(&json!(true)));

    let agent = decoded_patches(&response)
        .into_iter()
        .find(|patch| patch["path"] == "/spec/containers/-")
        .unwrap();

    assert_eq!(agent["value"]["image"], json!("ghcr.io/acme/agent:next"));
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
    // Since 0.1.1 the async stores are admitted like any other: the
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

/// The developer who knows nothing but "inject me": source, endpoint,
/// key and path all arrive from the installation.
#[test]
fn a_pod_can_deploy_knowing_only_that_it_wants_config() {
    let tiers = install(&[
        ("DYNAMIC_CONFIG_AGENT_SOURCE", "consul"),
        ("DYNAMIC_CONFIG_AGENT_PATH", "/config/rendered.toml"),
        (
            "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
            "consul: endpoint=http://consul.infra.svc:8500, key=myapp/config.json",
        ),
    ]);

    let pod = json!({
        "metadata": { "annotations": { "dynamic-config.rs/inject": "true" } },
        "spec": { "containers": [ { "name": "app", "image": "app:1" } ] },
    });

    let request = dynamic_config_webhook::of_pod_with(&pod, &tiers)
        .unwrap()
        .unwrap();

    assert_eq!(request.source, "consul");
    assert_eq!(
        request.endpoint.as_deref(),
        Some("http://consul.infra.svc:8500")
    );
    assert_eq!(request.key, "myapp/config.json");
    assert_eq!(request.path, "/config/rendered.toml");
}

/// Every store-shaped field can arrive from the per-store tier — auth,
/// its mount and role, the CA, a section, a token Secret, a template.
#[test]
fn store_defaults_cover_every_store_shaped_field() {
    let tiers = install(&[(
        "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
        "vault: endpoint=https://vault.vault.svc:8200, section=db, \
         auth=kubernetes, auth-mount=kubernetes, auth-role=myapp, \
         ca-configmap=vault-ca, key=secret/myapp",
    )]);

    let pod = json!({
        "metadata": {
            "annotations": {
                "dynamic-config.rs/inject": "true",
                "dynamic-config.rs/source": "vault",
                "dynamic-config.rs/path": "/config/rendered.yaml",
            },
        },
        "spec": { "containers": [ { "name": "app", "image": "app:1" } ] },
    });

    let request = dynamic_config_webhook::of_pod_with(&pod, &tiers)
        .unwrap()
        .unwrap();

    assert_eq!(
        request.endpoint.as_deref(),
        Some("https://vault.vault.svc:8200")
    );
    assert_eq!(request.key, "secret/myapp");
    assert_eq!(request.ca.as_ref().unwrap().name, "vault-ca");

    for pair in [
        ("--section", "db"),
        ("--auth", "kubernetes"),
        ("--auth-mount", "kubernetes"),
        ("--auth-role", "myapp"),
    ] {
        assert!(
            request
                .arguments
                .iter()
                .any(|(flag, value)| (flag.as_str(), value.as_str()) == pair),
            "{pair:?} missing from {:?}",
            request.arguments
        );
    }
}

#[test]
fn pinned_values_refuse_differing_annotations_and_pass_matching_ones() {
    let tiers = install(&[(
        "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
        "consul: watch-seconds=7!, endpoint=http://consul.infra.svc:8500!",
    )]);

    let mut differing = annotated("sidecar");
    differing["metadata"]["annotations"]["dynamic-config.rs/watch-seconds"] = json!("9");

    let error = dynamic_config_webhook::of_pod_with(&differing, &tiers).unwrap_err();
    assert!(error.contains("pins"), "{error}");

    // The same value restated is not a conflict…
    let mut matching = annotated("sidecar");
    matching["metadata"]["annotations"]["dynamic-config.rs/watch-seconds"] = json!("7");
    matching["metadata"]["annotations"]["dynamic-config.rs/endpoint"] =
        json!("http://consul.infra.svc:8500");
    assert!(dynamic_config_webhook::of_pod_with(&matching, &tiers).is_ok());

    // …and a pinned endpoint cannot be sidestepped through the pair's
    // other half.
    let mut sidestep = annotated("sidecar");
    sidestep["metadata"]["annotations"]
        .as_object_mut()
        .unwrap()
        .remove("dynamic-config.rs/endpoint");
    sidestep["metadata"]["annotations"]["dynamic-config.rs/endpoint-secret"] = json!("evil/url");

    let error = dynamic_config_webhook::of_pod_with(&sidestep, &tiers).unwrap_err();
    assert!(error.contains("answers as one"), "{error}");
}

#[test]
fn overridable_false_pins_the_set_and_spares_the_unset() {
    let tiers = install(&[
        ("DYNAMIC_CONFIG_AGENT_DEFAULTS_OVERRIDABLE", "false"),
        ("DYNAMIC_CONFIG_AGENT_FILE_MODE", "0640"),
        ("DYNAMIC_CONFIG_AGENT_WATCH_SECONDS", "30?"),
    ]);

    // file-mode is installation-set and unmarked: pinned by the flag.
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/file-mode"] = json!("0600");
    let error = dynamic_config_webhook::of_pod_with(&pod, &tiers).unwrap_err();
    assert!(error.contains("pins"), "{error}");

    // watch-seconds carries "?": overridable even under the flag.
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/watch-seconds"] = json!("5");
    let request = dynamic_config_webhook::of_pod_with(&pod, &tiers)
        .unwrap()
        .unwrap();
    assert_eq!(request.watch_seconds, 5);

    // metrics-port was never installation-set: still the pod's.
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/metrics-port"] = json!("9102");
    assert!(dynamic_config_webhook::of_pod_with(&pod, &tiers).is_ok());
}

#[test]
fn a_pinned_fleet_source_refuses_a_different_store() {
    let tiers = install(&[("DYNAMIC_CONFIG_AGENT_SOURCE", "consul!")]);

    let error = dynamic_config_webhook::of_pod_with(&vault_kubernetes_pod(), &tiers).unwrap_err();
    assert!(error.contains("pins"), "{error}");

    // Restating consul, or saying nothing, both pass.
    assert!(dynamic_config_webhook::of_pod_with(&annotated("sidecar"), &tiers).is_ok());
}

/// The override ladder inside one store's group: `overridable=false`
/// pins that store's values, a `?` marker reopens one of them, and
/// other stores stay untouched.
#[test]
fn a_store_can_pin_its_own_group() {
    let tiers = install(&[(
        "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
        "consul: overridable=false, watch-seconds=7, file-mode=0640?; \
         redis: watch-seconds=9",
    )]);

    // watch-seconds is pinned by the store's flag…
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/watch-seconds"] = json!("30");
    let error = dynamic_config_webhook::of_pod_with(&pod, &tiers).unwrap_err();
    assert!(error.contains("pins"), "{error}");

    // …file-mode carries "?" and stays the pod's…
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/file-mode"] = json!("0600");
    let request = dynamic_config_webhook::of_pod_with(&pod, &tiers)
        .unwrap()
        .unwrap();
    assert_eq!(request.file_mode.as_deref(), Some("600"));

    // …and redis pods override their watch freely: the flag was
    // consul's, not the fleet's.
    let mut pod = annotated("sidecar");
    let notes = &mut pod["metadata"]["annotations"];
    notes["dynamic-config.rs/source"] = json!("redis");
    notes["dynamic-config.rs/endpoint"] = json!("redis://redis.infra.svc:6379");
    notes["dynamic-config.rs/watch-seconds"] = json!("30");
    let request = dynamic_config_webhook::of_pod_with(&pod, &tiers)
        .unwrap()
        .unwrap();
    assert_eq!(request.watch_seconds, 30);
}

// ---------------------------------------------------------------------------
// What a refusal says about itself
// ---------------------------------------------------------------------------

/// A refusal carries a machine-readable `reason` beside its sentence.
///
/// A scrape aggregates on it, so it is part of what this webhook promises.
/// The three paths are three different conversations: a store the
/// installation does not allow is a deployment being wrong, a pinned value
/// being overridden is somebody working around the installation, and a
/// malformed annotation is a typo.
#[test]
fn a_refusal_says_which_kind_it_is() {
    let review = |pod: serde_json::Value| {
        serde_json::json!({
            "request": { "uid": "u", "namespace": "team-a", "object": pod },
        })
    };
    let reason = |response: &serde_json::Value| {
        response
            .pointer("/response/status/reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };

    // Policy: a store this installation does not allow here.
    let denied = install(&[("DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY", "vault")]);
    let response =
        dynamic_config_webhook::admission_response_with(&review(vault_kubernetes_pod()), &denied);

    assert_eq!(reason(&response), dynamic_config_webhook::POLICY);

    // Pinned: a value the installation fixed, overridden by a pod.
    let pinned = install(&[("DYNAMIC_CONFIG_AGENT_SOURCE", "consul!")]);
    let response =
        dynamic_config_webhook::admission_response_with(&review(vault_kubernetes_pod()), &pinned);

    assert_eq!(
        reason(&response),
        dynamic_config_webhook::PINNED,
        "{}",
        response
            .pointer("/response/status/message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    );

    // Malformed: a value that is not the shape it has to be.
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]["dynamic-config.rs/file-mode"] = json!("888");
    let response = dynamic_config_webhook::admission_response_with(&review(pod), &install(&[]));

    assert_eq!(reason(&response), dynamic_config_webhook::MALFORMED);
}

// ---------------------------------------------------------------------------
// Injecting twice
// ---------------------------------------------------------------------------

/// **A second admission of a patched pod must not patch it again.**
///
/// A mutating webhook is not called once. `reinvocationPolicy: IfNeeded`
/// asks the API server to call it again whenever a later webhook changes
/// the pod, and some controllers resubmit a spec that has already been
/// admitted. Injecting twice produces two containers with one name, which
/// the API server refuses — so the pod does not start at all, and the
/// failure looks nothing like its cause.
#[test]
fn a_second_admission_does_not_inject_twice() {
    use base64::Engine as _;

    let review = |pod: &serde_json::Value| json!({ "request": { "uid": "u", "namespace": "team", "object": pod } });
    let apply = |pod: &serde_json::Value, response: &serde_json::Value| {
        let encoded = response
            .pointer("/response/patch")
            .and_then(serde_json::Value::as_str)
            .expect("the pod asked, so it was patched");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("the patch is base64");
        let operations: json_patch::Patch =
            serde_json::from_slice(&decoded).expect("the patch is a JSON patch");

        let mut patched = pod.clone();
        json_patch::patch(&mut patched, &operations).expect("the patch applies");

        patched
    };
    let container_names = |pod: &serde_json::Value| -> Vec<String> {
        pod.pointer("/spec/containers")
            .and_then(serde_json::Value::as_array)
            .map(|containers| {
                containers
                    .iter()
                    .filter_map(|container| Some(container.get("name")?.as_str()?.to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    };

    let pod = annotated("sidecar");
    let once = apply(
        &pod,
        &dynamic_config_webhook::admission_response(&review(&pod)),
    );

    assert_eq!(
        container_names(&once),
        ["app", "dynamic-config-agent"],
        "the first admission injects"
    );
    assert_eq!(
        once.pointer(&format!(
            "/metadata/annotations/dynamic-config.rs~1{}",
            dynamic_config_webhook::STATUS
        ))
        .and_then(serde_json::Value::as_str),
        Some(dynamic_config_webhook::INJECTED),
        "and marks the pod: {once}"
    );

    let second = dynamic_config_webhook::admission_response(&review(&once));

    assert!(
        second.pointer("/response/patch").is_none(),
        "the second admission patched a pod that was already injected: {second}"
    );
    assert_eq!(
        second.pointer("/response/allowed"),
        Some(&json!(true)),
        "and it is allowed, not refused — the first pass already decided"
    );
}

/// The mark is this webhook's to write, and a pod saying something else
/// with it is refused rather than quietly skipped.
#[test]
fn a_pod_may_not_invent_its_own_injection_status() {
    let mut pod = annotated("sidecar");
    pod["metadata"]["annotations"]
        [format!("dynamic-config.rs/{}", dynamic_config_webhook::STATUS)] = json!("nearly");

    let error = dynamic_config_webhook::of_pod_with(&pod, &install(&[])).unwrap_err();

    assert!(error.contains("not a pod's to set"), "{error}");
}

/// A pod that already uses a name the injection needs is refused, not
/// patched into a pod the API server will reject.
///
/// Two containers with one name is invalid, so the pod never starts —
/// and the error a user sees then is the API server's, about a spec they
/// did not write. Refusing here puts the sentence where the mistake is.
#[test]
fn a_container_name_the_injection_needs_is_refused_rather_than_duplicated() {
    for list in ["containers", "initContainers"] {
        let mut pod = annotated("sidecar");
        pod["spec"][list] = json!([
            { "name": "app", "image": "app:1" },
            { "name": "dynamic-config-agent", "image": "somebody-elses:1" }
        ]);

        let response = dynamic_config_webhook::admission_response(&review(pod));

        assert_eq!(
            response.pointer("/response/allowed"),
            Some(&json!(false)),
            "{list}: {response}"
        );
        assert_eq!(
            response
                .pointer("/response/status/reason")
                .and_then(serde_json::Value::as_str),
            Some(dynamic_config_webhook::CONFLICT)
        );

        let message = response
            .pointer("/response/status/message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        assert!(message.contains("dynamic-config-agent"), "{message}");
        assert!(message.contains("rename"), "{message}");
    }
}

// ---------------------------------------------------------------------------
// The readiness probe on the injected container
// ---------------------------------------------------------------------------

/// A pod with a metrics port, which is what the chart now gives every
/// installation by default.
fn probed(extra: &[(&str, &str)]) -> Value {
    let mut pod = annotated("sidecar");

    let annotations = pod
        .pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("the fixture has annotations");

    annotations.insert("dynamic-config.rs/metrics-port".to_owned(), json!("9110"));

    for (key, value) in extra {
        annotations.insert(format!("dynamic-config.rs/{key}"), json!(value));
    }

    pod
}

/// The probe is what makes `/readyz` worth answering: pod readiness is
/// AND-ed across containers, so a Service sends no traffic to a pod whose
/// configuration does not exist yet.
#[test]
fn a_watching_agent_with_a_metrics_port_is_probed() {
    let response = dynamic_config_webhook::admission_response(&review(probed(&[])));
    let patches = decoded_patches(&response);

    let container = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value"))
        .find(|value| {
            value.pointer("/name").and_then(Value::as_str) == Some("dynamic-config-agent")
        })
        .expect("the agent container is injected");

    let probe = container
        .pointer("/readinessProbe")
        .expect("a watching agent with a port answers a probe on it");

    assert_eq!(
        probe.pointer("/httpGet/path").and_then(Value::as_str),
        Some("/readyz")
    );
    // By name, so the probe cannot drift from the port the args opened.
    assert_eq!(
        probe.pointer("/httpGet/port").and_then(Value::as_str),
        Some("metrics")
    );
    assert!(
        probe
            .pointer("/failureThreshold")
            .and_then(Value::as_u64)
            .is_some_and(|threshold| threshold >= 10),
        "a slow first fetch is not a failure: {probe}"
    );
}

/// The escape hatch for a deployment that would rather start than wait for
/// a store that is down.
#[test]
fn readiness_false_leaves_the_container_unprobed() {
    let response =
        dynamic_config_webhook::admission_response(&review(probed(&[("readiness", "false")])));

    let patches = decoded_patches(&response);
    let container = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value"))
        .find(|value| {
            value.pointer("/name").and_then(Value::as_str) == Some("dynamic-config-agent")
        })
        .expect("the agent container is injected");

    assert!(container.pointer("/readinessProbe").is_none());

    // And the port is still there: opting out of the probe is not opting
    // out of metrics.
    assert!(container.pointer("/ports").is_some());
}

/// A one-shot init container has nothing long-lived to probe, and a probe
/// on a container that exits is a pod that never starts.
#[test]
fn an_init_agent_is_never_probed() {
    let mut pod = probed(&[]);
    pod.pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations")
        .insert("dynamic-config.rs/mode".to_owned(), json!("init"));

    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));

    let container = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value"))
        .find(|value| value.pointer("/name").and_then(Value::as_str) == Some("dynamic-config-init"))
        .expect("the init container is injected");

    assert!(container.pointer("/readinessProbe").is_none());
}

#[test]
fn readiness_is_true_or_false_and_nothing_else() {
    let response =
        dynamic_config_webhook::admission_response(&review(probed(&[("readiness", "yes")])));

    assert_eq!(
        response
            .pointer("/response/allowed")
            .and_then(Value::as_bool),
        Some(false)
    );
}

/// The policy reaches the agent, which is the only place it is validated —
/// so the two cannot disagree about which policies exist.
#[test]
fn a_startup_policy_reaches_the_agent_as_a_flag() {
    let mut pod = annotated("sidecar");
    pod.pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations")
        .insert(
            "dynamic-config.rs/startup-policy".to_owned(),
            json!("require-fresh"),
        );

    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));

    let args = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value/args"))
        .find_map(Value::as_array)
        .expect("the agent's arguments");

    let rendered: Vec<&str> = args.iter().filter_map(Value::as_str).collect();

    assert!(
        rendered
            .windows(2)
            .any(|pair| pair == ["--startup-policy", "require-fresh"]),
        "{rendered:?}"
    );
}

/// A dynamic-secret source reaches the agent as `--dynamic`, and revoking
/// is the default — so the opt-out is what appears on the command line, not
/// the opt-in.
#[test]
fn a_dynamic_source_asks_for_a_lease_and_revokes_by_default() {
    let mut pod = annotated("sidecar");
    pod.pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations")
        .insert("dynamic-config.rs/dynamic".to_owned(), json!("true"));

    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));
    let args: Vec<String> = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value/args"))
        .find_map(Value::as_array)
        .expect("the agent's arguments")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();

    assert!(args.iter().any(|arg| arg == "--dynamic"), "{args:?}");
    assert!(
        !args.iter().any(|arg| arg == "--no-revoke-on-shutdown"),
        "revoking is the default, so nothing should say so: {args:?}"
    );
}

#[test]
fn declining_revocation_says_so_on_the_command_line() {
    let mut pod = annotated("sidecar");
    let annotations = pod
        .pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations");

    annotations.insert("dynamic-config.rs/dynamic".to_owned(), json!("true"));
    annotations.insert(
        "dynamic-config.rs/revoke-on-shutdown".to_owned(),
        json!("false"),
    );

    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));
    let args: Vec<String> = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value/args"))
        .find_map(Value::as_array)
        .expect("the agent's arguments")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();

    assert!(
        args.iter().any(|arg| arg == "--no-revoke-on-shutdown"),
        "{args:?}"
    );
}

/// Declining to revoke a lease nobody holds is a configuration mistake, not
/// a no-op — and an unknown-annotation contract that catches typos should
/// catch this too.
#[test]
fn revoke_on_shutdown_without_a_dynamic_source_is_refused() {
    let mut pod = annotated("sidecar");
    pod.pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations")
        .insert(
            "dynamic-config.rs/revoke-on-shutdown".to_owned(),
            json!("false"),
        );

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(
        response
            .pointer("/response/allowed")
            .and_then(Value::as_bool),
        Some(false)
    );
}

// ---------------------------------------------------------------------------
// Warnings: everything worth saying that is not worth refusing over
// ---------------------------------------------------------------------------

fn warnings_for(extra: &[(&str, &str)]) -> Vec<String> {
    let mut pod = annotated("sidecar");
    let annotations = pod
        .pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations");

    for (key, value) in extra {
        annotations.insert(format!("dynamic-config.rs/{key}"), json!(value));
    }

    dynamic_config_webhook::admission_response(&review(pod))
        .pointer("/response/warnings")
        .and_then(Value::as_array)
        .map(|warnings| {
            warnings
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The bar is deliberately high: a warning on every admission is a warning
/// nobody reads. The canonical pod earns none.
#[test]
fn an_ordinary_pod_is_admitted_without_comment() {
    assert!(warnings_for(&[]).is_empty());
}

/// A rendered secret on node-backed storage outlives the pod and survives a
/// reboot. Legal, occasionally necessary, and worth saying out loud.
#[test]
fn disk_backed_storage_is_worth_a_word() {
    let warnings = warnings_for(&[("volume-medium", "disk")]);

    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("outlives the pod"), "{warnings:?}");
}

#[test]
fn a_world_readable_file_mode_is_worth_a_word() {
    let warnings = warnings_for(&[("file-mode", "0644")]);

    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("world-readable")),
        "{warnings:?}"
    );
}

#[test]
fn an_aggressive_poll_is_worth_a_word() {
    let warnings = warnings_for(&[("watch-seconds", "1")]);

    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("watch-seconds")),
        "{warnings:?}"
    );
}

/// A lease nobody hands back stays valid, held by a pod that no longer
/// exists. The pod is still admitted — somebody may have a reason — and the
/// reason had better be a good one.
#[test]
fn declining_to_revoke_a_lease_is_worth_a_word() {
    let warnings = warnings_for(&[("dynamic", "true"), ("revoke-on-shutdown", "false")]);

    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("after \nthis pod is gone")
                || warning.contains("after this pod is gone")),
        "{warnings:?}"
    );
}

/// A warning is not a refusal: the pod is admitted, and the patch is still
/// there.
#[test]
fn a_warned_pod_is_still_injected() {
    let mut pod = annotated("sidecar");
    pod.pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations")
        .insert("dynamic-config.rs/volume-medium".to_owned(), json!("disk"));

    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(
        response
            .pointer("/response/allowed")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(response.pointer("/response/patch").is_some());
}

// ---------------------------------------------------------------------------
// Named renders, and their own metrics port
// ---------------------------------------------------------------------------

/// A pod with a second render called `db`.
fn with_named_render(extra: &[(&str, &str)]) -> Value {
    let mut pod = annotated("sidecar");
    let annotations = pod
        .pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations");

    annotations.insert("dynamic-config.rs/source.db".to_owned(), json!("consul"));
    annotations.insert(
        "dynamic-config.rs/endpoint.db".to_owned(),
        json!("http://consul:8500"),
    );
    annotations.insert(
        "dynamic-config.rs/key.db".to_owned(),
        json!("myapp/db.json"),
    );
    annotations.insert(
        "dynamic-config.rs/path.db".to_owned(),
        json!("/config/db.toml"),
    );

    for (key, value) in extra {
        annotations.insert(format!("dynamic-config.rs/{key}"), json!(value));
    }

    pod
}

fn container_named(patches: &[Value], name: &str) -> Option<Value> {
    patches
        .iter()
        .filter_map(|patch| patch.pointer("/value"))
        .find(|value| value.pointer("/name").and_then(Value::as_str) == Some(name))
        .cloned()
}

/// Named renders were unobservable: the container had no metrics block at
/// all, so `dynamic-config-agent-db` reported nothing whatever went wrong
/// in it.
#[test]
fn a_named_render_can_have_its_own_metrics_port() {
    let pod = with_named_render(&[("metrics-port.db", "9111")]);
    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));

    let container =
        container_named(&patches, "dynamic-config-agent-db").expect("the named render's container");

    let args: Vec<&str> = container
        .pointer("/args")
        .and_then(Value::as_array)
        .expect("args")
        .iter()
        .filter_map(Value::as_str)
        .collect();

    assert!(
        args.windows(2)
            .any(|pair| pair == ["--metrics-addr", "0.0.0.0:9111"]),
        "{args:?}"
    );

    // Unnamed, and the probe uses the number: a Kubernetes port name is
    // at most fifteen characters and a render suffix may be thirty-two, so
    // naming it would refuse a pod for the longer half of the suffixes this
    // contract allows.
    assert!(container.pointer("/ports/0/name").is_none());
    assert_eq!(
        container
            .pointer("/readinessProbe/httpGet/port")
            .and_then(Value::as_u64),
        Some(9111)
    );
}

/// No allocation scheme: `port + n` reads as tidy right up to the afternoon
/// it lands on whatever the application is listening on. A named render is
/// observable when somebody names its port and not before.
#[test]
fn a_named_render_gets_no_port_unless_one_is_named() {
    let pod = with_named_render(&[("metrics-port", "9110")]);
    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));

    let default =
        container_named(&patches, "dynamic-config-agent").expect("the default render's container");
    let named =
        container_named(&patches, "dynamic-config-agent-db").expect("the named render's container");

    assert!(
        default.pointer("/ports").is_some(),
        "the default one has it"
    );
    assert!(
        named.pointer("/ports").is_none(),
        "the named one did not ask for a port and must not inherit one"
    );
}

#[test]
fn a_named_renders_port_must_be_a_port() {
    let pod = with_named_render(&[("metrics-port.db", "not-a-port")]);
    let response = dynamic_config_webhook::admission_response(&review(pod));

    assert_eq!(
        response
            .pointer("/response/allowed")
            .and_then(Value::as_bool),
        Some(false)
    );
}

/// A schema arrives as a mounted ConfigMap, and the agent checks the
/// resolved document against it before the write — so a document that does
/// not satisfy it never reaches the application.
#[test]
fn a_schema_configmap_is_mounted_and_named_on_the_command_line() {
    let mut pod = annotated("sidecar");
    pod.pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations")
        .insert(
            "dynamic-config.rs/schema-configmap".to_owned(),
            json!("billing-schema"),
        );

    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));

    // The volume.
    assert!(
        patches.iter().any(|patch| {
            patch.pointer("/value/name").and_then(Value::as_str) == Some("dynamic-config-schema")
                && patch
                    .pointer("/value/configMap/name")
                    .and_then(Value::as_str)
                    == Some("billing-schema")
        }),
        "the ConfigMap is not mounted as a volume"
    );

    let container = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value"))
        .find(|value| {
            value.pointer("/name").and_then(Value::as_str) == Some("dynamic-config-agent")
        })
        .expect("the agent container");

    // Read-only, like every other mounted reference here.
    let mount = container
        .pointer("/volumeMounts")
        .and_then(Value::as_array)
        .expect("mounts")
        .iter()
        .find(|mount| {
            mount.pointer("/name").and_then(Value::as_str) == Some("dynamic-config-schema")
        })
        .expect("the schema is mounted into the agent");

    assert_eq!(
        mount.pointer("/readOnly").and_then(Value::as_bool),
        Some(true)
    );

    // And the flag, pointing at the default key.
    let args: Vec<&str> = container
        .pointer("/args")
        .and_then(Value::as_array)
        .expect("args")
        .iter()
        .filter_map(Value::as_str)
        .collect();

    assert!(
        args.windows(2)
            .any(|pair| { pair == ["--schema", "/etc/dynamic-config/schema/schema.json"] }),
        "{args:?}"
    );
}

/// `name/key`, for a ConfigMap whose key somebody else named.
#[test]
fn a_schema_configmap_can_name_its_key() {
    let mut pod = annotated("sidecar");
    pod.pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations")
        .insert(
            "dynamic-config.rs/schema-configmap".to_owned(),
            json!("schemas/billing.v2.json"),
        );

    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));

    let args: Vec<String> = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value/args"))
        .find_map(Value::as_array)
        .expect("args")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();

    assert!(
        args.iter()
            .any(|arg| arg == "/etc/dynamic-config/schema/billing.v2.json"),
        "{args:?}"
    );
}

// ---------------------------------------------------------------------------
// Several files, one generation
// ---------------------------------------------------------------------------

fn with_also(extra: &[(&str, &str)]) -> Value {
    let mut pod = annotated("sidecar");
    let annotations = pod
        .pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations");

    for (key, value) in extra {
        annotations.insert(format!("dynamic-config.rs/{key}"), json!(value));
    }

    pod
}

fn agent_args(pod: Value) -> Vec<String> {
    decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)))
        .iter()
        .filter_map(|patch| patch.pointer("/value/args"))
        .find_map(Value::as_array)
        .expect("the agent's arguments")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// One document, several files, published together — so an application
/// reading two of them never sees one from before a change and one from
/// after it.
#[test]
fn a_rendering_reaches_the_agent_with_its_section() {
    let args = agent_args(with_also(&[
        ("also.cache", "/config/cache.json"),
        ("also-section.cache", "cache"),
    ]));

    assert!(
        args.windows(2)
            .any(|pair| pair == ["--also", "out=/config/cache.json,section=cache"]),
        "{args:?}"
    );
}

/// A section is optional: the whole document, in another format, is a
/// perfectly ordinary thing to want.
#[test]
fn a_rendering_without_a_section_is_the_whole_document() {
    let args = agent_args(with_also(&[("also.env", "/config/app.json")]));

    assert!(
        args.windows(2)
            .any(|pair| pair == ["--also", "out=/config/app.json"]),
        "{args:?}"
    );
}

/// Every rendering lands in the volume the injection mounts. A path
/// outside it would be written into the container's read-only root and
/// fail at the first render — which is a refusal worth making at admission,
/// where the message reaches whoever wrote the annotation.
#[test]
fn a_rendering_outside_the_mounted_directory_is_refused() {
    let response = dynamic_config_webhook::admission_response(&review(with_also(&[(
        "also.elsewhere",
        "/etc/app/other.json",
    )])));

    assert_eq!(
        response
            .pointer("/response/allowed")
            .and_then(Value::as_bool),
        Some(false)
    );

    let message = response
        .pointer("/response/status/message")
        .and_then(Value::as_str)
        .unwrap_or_default();

    assert!(message.contains("same directory"), "{message}");
}

/// A section without a path is a rendering with nowhere to go.
#[test]
fn a_section_without_a_path_is_refused() {
    let response = dynamic_config_webhook::admission_response(&review(with_also(&[(
        "also-section.cache",
        "cache",
    )])));

    assert_eq!(
        response
            .pointer("/response/allowed")
            .and_then(Value::as_bool),
        Some(false)
    );
}

/// `also.<name>` and a *named render* are different shapes and must not be
/// confused: `section` is already a per-render key, so the two namespaces
/// had to be kept apart. This is the test that says they are.
#[test]
fn a_rendering_and_a_named_render_can_coexist() {
    let mut pod = with_also(&[
        ("also.cache", "/config/cache.json"),
        ("also-section.cache", "cache"),
    ]);

    let annotations = pod
        .pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
        .expect("annotations");

    // A genuinely separate render: its own source, its own container, its
    // own generation — which is the truth about a second store.
    annotations.insert("dynamic-config.rs/source.db".to_owned(), json!("consul"));
    annotations.insert(
        "dynamic-config.rs/endpoint.db".to_owned(),
        json!("http://consul:8500"),
    );
    annotations.insert(
        "dynamic-config.rs/key.db".to_owned(),
        json!("myapp/db.json"),
    );
    annotations.insert(
        "dynamic-config.rs/path.db".to_owned(),
        json!("/config/db.toml"),
    );

    let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));

    let names: Vec<&str> = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value/name"))
        .filter_map(Value::as_str)
        .collect();

    assert!(names.contains(&"dynamic-config-agent"), "{names:?}");
    assert!(names.contains(&"dynamic-config-agent-db"), "{names:?}");

    // The rendering rides on the *main* agent, because it is a rendering
    // of the main agent's document.
    let main = patches
        .iter()
        .filter_map(|patch| patch.pointer("/value"))
        .find(|value| {
            value.pointer("/name").and_then(Value::as_str) == Some("dynamic-config-agent")
        })
        .expect("the main container");

    let args: Vec<&str> = main
        .pointer("/args")
        .and_then(Value::as_array)
        .expect("args")
        .iter()
        .filter_map(Value::as_str)
        .collect();

    assert!(
        args.iter()
            .any(|arg| arg.starts_with("out=/config/cache.json")),
        "{args:?}"
    );
}

/// A memory-backed `emptyDir` is charged to the **pod's** memory, so a
/// document larger than anybody expected does not fail the agent — it gets
/// the whole pod OOM-killed, application included.
///
/// With a `sizeLimit` the tmpfs is sized to it and the write fails with
/// `ENOSPC` instead, which the agent has warned about rather than died on
/// since this release: the last good file keeps serving.
#[test]
fn the_rendered_volume_has_a_size_limit() {
    for medium in ["memory", "disk"] {
        let mut pod = annotated("sidecar");
        pod.pointer_mut("/metadata/annotations")
            .and_then(Value::as_object_mut)
            .expect("annotations")
            .insert("dynamic-config.rs/volume-medium".to_owned(), json!(medium));

        let patches = decoded_patches(&dynamic_config_webhook::admission_response(&review(pod)));

        let volume = patches
            .iter()
            .filter_map(|patch| patch.pointer("/value"))
            .find(|value| value.pointer("/name").and_then(Value::as_str) == Some("dynamic-config"))
            .expect("the rendered volume");

        assert!(
            volume.pointer("/emptyDir/sizeLimit").is_some(),
            "{medium}: an unbounded volume is a pod eviction waiting to happen: {volume}"
        );
    }
}
