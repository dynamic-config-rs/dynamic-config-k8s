//! AdmissionReview in, AdmissionReview-with-JSONPatch out — pure, and
//! golden-file tested, because a webhook that can only be tested in a
//! cluster is a webhook nobody tests.

use serde_json::{json, Value};

use crate::annotations::{self, Mode};

/// The image every injected container runs. The chart sets
/// `DYNAMIC_CONFIG_AGENT_IMAGE` from its values — digest-pin it there —
/// and this constant is only the fallback a bare binary gets. Read once:
/// an admission decision must not change between two requests because
/// somebody edited the environment.
const AGENT_IMAGE: &str = "ghcr.io/dynamic-config-rs/dynamic-config-agent:0.1.0";

fn agent_image() -> &'static str {
    static IMAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    IMAGE.get_or_init(|| {
        std::env::var("DYNAMIC_CONFIG_AGENT_IMAGE").unwrap_or_else(|_| AGENT_IMAGE.to_owned())
    })
}

/// The whole webhook: a review's response, allowed or patched or refused.
pub fn admission_response(review: &Value) -> Value {
    let uid = review
        .pointer("/request/uid")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Some(pod) = review.pointer("/request/object") else {
        return respond(uid, json!({ "allowed": true }));
    };

    match annotations::of_pod(pod) {
        Ok(None) => respond(uid, json!({ "allowed": true })),
        Ok(Some(request)) => {
            let patches = patches_for(pod, &request);
            let encoded = base64(&serde_json::to_vec(&patches).expect("patches serialise"));

            respond(
                uid,
                json!({
                    "allowed": true,
                    "patchType": "JSONPatch",
                    "patch": encoded,
                }),
            )
        }
        Err(reason) => respond(
            uid,
            json!({
                "allowed": false,
                "status": { "message": reason, "code": 400 },
            }),
        ),
    }
}

fn respond(uid: &str, mut response: Value) -> Value {
    response["uid"] = json!(uid);

    json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "response": response,
    })
}

/// The JSONPatch that injects the agent: a shared `emptyDir`, a mount on
/// every existing container, and the agent as init container, sidecar,
/// or both.
pub fn patches_for(pod: &Value, request: &annotations::Request) -> Vec<Value> {
    let mut patches = Vec::new();

    // 1. The volume the rendered file lives on.
    let has_volumes = pod.pointer("/spec/volumes").is_some();

    if !has_volumes {
        patches.push(json!({ "op": "add", "path": "/spec/volumes", "value": [] }));
    }

    // tmpfs unless the pod asked for disk: rendered configuration can
    // carry secrets, and memory-backed emptyDir keeps them off the node.
    let empty_dir = if request.volume_memory {
        json!({ "medium": "Memory" })
    } else {
        json!({})
    };

    patches.push(json!({
        "op": "add",
        "path": "/spec/volumes/-",
        "value": { "name": "dynamic-config", "emptyDir": empty_dir },
    }));

    // A private CA rides a ConfigMap; an ssh key rides a Secret with the
    // permissions ssh insists on. Both are the webhook's volumes, mounted
    // only into the agent — the application containers have no business
    // reading either.
    if let Some(ca) = &request.ca {
        patches.push(json!({
            "op": "add",
            "path": "/spec/volumes/-",
            "value": {
                "name": "dynamic-config-ca",
                "configMap": { "name": ca.name },
            },
        }));
    }

    if let Some(ssh) = &request.ssh {
        patches.push(json!({
            "op": "add",
            "path": "/spec/volumes/-",
            "value": {
                "name": "dynamic-config-ssh",
                "secret": { "secretName": ssh.name, "defaultMode": 0o400 },
            },
        }));
    }

    if let Some(tls) = &request.tls {
        patches.push(json!({
            "op": "add",
            "path": "/spec/volumes/-",
            "value": {
                "name": "dynamic-config-client-tls",
                "secret": { "secretName": tls, "defaultMode": 0o400 },
            },
        }));
    }

    if let Some(template) = &request.template {
        patches.push(json!({
            "op": "add",
            "path": "/spec/volumes/-",
            "value": {
                "name": "dynamic-config-template",
                "configMap": { "name": template.name },
            },
        }));
    }

    // 2. Mount it into every application container, at the directory of
    //    the requested path.
    let mount_dir = std::path::Path::new(&request.path)
        .parent()
        .and_then(|p| p.to_str())
        .filter(|p| !p.is_empty())
        .unwrap_or("/config");

    let containers = pod
        .pointer("/spec/containers")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    for index in 0..containers {
        if pod
            .pointer(&format!("/spec/containers/{index}/volumeMounts"))
            .is_none()
        {
            patches.push(json!({
                "op": "add",
                "path": format!("/spec/containers/{index}/volumeMounts"),
                "value": [],
            }));
        }

        patches.push(json!({
            "op": "add",
            "path": format!("/spec/containers/{index}/volumeMounts/-"),
            "value": { "name": "dynamic-config", "mountPath": mount_dir },
        }));
    }

    // 3. The agent itself.
    let agent = |watch: bool| {
        let mut arguments = vec!["--source".to_owned(), request.source.clone()];

        // Absent when the endpoint travels as a Secret-backed environment
        // variable instead — a redis url with a password in it.
        if let Some(endpoint) = &request.endpoint {
            arguments.push("--endpoint".to_owned());
            arguments.push(endpoint.clone());
        }

        arguments.push("--key".to_owned());
        arguments.push(request.key.clone());
        arguments.push("--out".to_owned());
        arguments.push(request.path.clone());

        for (flag, value) in &request.arguments {
            arguments.push(flag.clone());
            arguments.push(value.clone());
        }

        if watch {
            arguments.push("--watch".to_owned());
            arguments.push(request.watch_seconds.to_string());
        }

        let mut mounts = vec![json!({ "name": "dynamic-config", "mountPath": mount_dir })];

        if request.ca.is_some() {
            mounts.push(json!({
                "name": "dynamic-config-ca",
                "mountPath": annotations::CA_MOUNT,
                "readOnly": true,
            }));
        }

        if request.ssh.is_some() {
            mounts.push(json!({
                "name": "dynamic-config-ssh",
                "mountPath": annotations::SSH_MOUNT,
                "readOnly": true,
            }));
        }

        if request.tls.is_some() {
            mounts.push(json!({
                "name": "dynamic-config-client-tls",
                "mountPath": annotations::TLS_MOUNT,
                "readOnly": true,
            }));
        }

        if request.template.is_some() {
            mounts.push(json!({
                "name": "dynamic-config-template",
                "mountPath": annotations::TEMPLATE_MOUNT,
                "readOnly": true,
            }));
        }

        let mut limits = json!({ "memory": request.resources.memory_limit });

        if let Some(cpu) = &request.resources.cpu_limit {
            limits["cpu"] = json!(cpu);
        }

        let mut container = json!({
            "name": if watch { "dynamic-config-agent" } else { "dynamic-config-init" },
            "image": agent_image(),
            "args": arguments,
            "volumeMounts": mounts,
            "resources": {
                "requests": {
                    "cpu": request.resources.cpu_request,
                    "memory": request.resources.memory_request,
                },
                "limits": limits,
            },
            // The restricted Pod Security Standard, in full, so injection
            // works in namespaces that enforce it — an injector that
            // relaxes a pod's posture is a finding, not a feature.
            "securityContext": {
                "runAsNonRoot": true,
                "runAsUser": 65532,
                "runAsGroup": 65532,
                "allowPrivilegeEscalation": false,
                "capabilities": { "drop": ["ALL"] },
                "readOnlyRootFilesystem": true,
                "seccompProfile": { "type": "RuntimeDefault" },
            },
        });

        // Secret material reaches the agent as environment, never as
        // arguments: `kubectl describe pod` prints args to anyone with
        // pod read access.
        if !request.secret_env.is_empty() {
            container["env"] = request
                .secret_env
                .iter()
                .map(|(variable, secret)| {
                    json!({
                        "name": variable,
                        "valueFrom": {
                            "secretKeyRef": { "name": secret.name, "key": secret.key },
                        },
                    })
                })
                .collect();
        }

        container
    };

    if matches!(request.mode, Mode::Init | Mode::Both) {
        if pod.pointer("/spec/initContainers").is_none() {
            patches.push(json!({
                "op": "add", "path": "/spec/initContainers", "value": [],
            }));
        }

        patches.push(json!({
            "op": "add", "path": "/spec/initContainers/-", "value": agent(false),
        }));
    }

    if matches!(request.mode, Mode::Sidecar | Mode::Both) {
        if request.native_sidecar {
            // The 1.29+ sidecar shape: an init container that restarts
            // always starts before the app containers, ends after them,
            // and does not stop a Job from finishing.
            let mut sidecar = agent(true);
            sidecar["restartPolicy"] = json!("Always");

            if pod.pointer("/spec/initContainers").is_none() && !matches!(request.mode, Mode::Both)
            {
                patches.push(json!({
                    "op": "add", "path": "/spec/initContainers", "value": [],
                }));
            }

            patches.push(json!({
                "op": "add", "path": "/spec/initContainers/-", "value": sidecar,
            }));
        } else {
            patches.push(json!({
                "op": "add", "path": "/spec/containers/-", "value": agent(true),
            }));
        }
    }

    patches
}

/// Standard-library base64: three bytes to four characters, the padding
/// rules and nothing else — a dependency would be larger than the code.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);

        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }

    out
}
