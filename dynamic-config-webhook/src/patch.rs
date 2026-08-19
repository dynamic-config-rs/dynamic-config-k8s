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
const AGENT_IMAGE: &str = "ghcr.io/dynamic-config-rs/dynamic-config-agent:v0.1.1";

fn agent_image() -> &'static str {
    static IMAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    IMAGE.get_or_init(|| {
        std::env::var("DYNAMIC_CONFIG_AGENT_IMAGE").unwrap_or_else(|_| AGENT_IMAGE.to_owned())
    })
}

/// The pull secret injected pods need when the agent image lives in a
/// private mirror. The Secret itself is namespaced, so it must exist in
/// every namespace that injects — the chart README says so out loud.
fn agent_pull_secret() -> Option<&'static str> {
    static SECRET: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

    SECRET
        .get_or_init(|| {
            std::env::var("DYNAMIC_CONFIG_AGENT_PULL_SECRET")
                .ok()
                .filter(|name| !name.is_empty())
        })
        .as_deref()
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
                // Owner-only ON PURPOSE, unlike the TLS pair below: ssh
                // clients refuse a key anyone else can read, so widening
                // this to fix the nonroot-read problem would break the
                // very client it feeds.
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
                // 0444, not 0400: the kubelet writes secret files as root and
                // the agent runs nonroot — owner-only would lock the agent out
                // of its own credential. Only the agent mounts this volume.
                "secret": { "secretName": tls, "defaultMode": 0o444 },
            },
        }));
    }

    // The named renders' aux material, one suffixed volume each — the
    // shared render volume is the one above; only credentials and
    // templates multiply.
    for extra in &request.extra {
        let n = &extra.name;

        if let Some(ca) = &extra.ca {
            patches.push(json!({
                "op": "add",
                "path": "/spec/volumes/-",
                "value": {
                    "name": format!("dynamic-config-ca-{n}"),
                    "configMap": { "name": ca.name },
                },
            }));
        }

        if let Some(ssh) = &extra.ssh {
            patches.push(json!({
                "op": "add",
                "path": "/spec/volumes/-",
                "value": {
                    "name": format!("dynamic-config-ssh-{n}"),
                    "secret": { "secretName": ssh.name, "defaultMode": 0o400 },
                },
            }));
        }

        if let Some(tls) = &extra.tls {
            patches.push(json!({
                "op": "add",
                "path": "/spec/volumes/-",
                "value": {
                    "name": format!("dynamic-config-client-tls-{n}"),
                    "secret": { "secretName": tls, "defaultMode": 0o444 },
                },
            }));
        }

        if let Some(template) = &extra.template {
            patches.push(json!({
                "op": "add",
                "path": "/spec/volumes/-",
                "value": {
                    "name": format!("dynamic-config-template-{n}"),
                    "configMap": { "name": template.name },
                },
            }));
        }
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

    // 2a½. The agent image's pull secret, when the fleet mirrors from a
    // private registry: appended, never replaced — the pod's own pull
    // secrets keep working.
    if let Some(name) = agent_pull_secret() {
        if pod.pointer("/spec/imagePullSecrets").is_none() {
            patches.push(json!({
                "op": "add",
                "path": "/spec/imagePullSecrets",
                "value": [],
            }));
        }

        patches.push(json!({
            "op": "add",
            "path": "/spec/imagePullSecrets/-",
            "value": { "name": name },
        }));
    }

    // 2b. The env wrapper: the named container's command becomes
    // `sh -c 'set -a; . <path>; set +a; exec "$@"' dynamic-config-env
    // <original command and args…>` — the rendered dotenv is the
    // process's REAL environment, loaded once at start (Kubernetes'
    // rule: a running environ never changes; the init agent guarantees
    // the file exists first). `args` folds into `command`, so it is
    // removed where present.
    if let Some(name) = &request.env_inject {
        let containers = pod
            .pointer("/spec/containers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for (index, container) in containers.iter().enumerate() {
            if container["name"].as_str() != Some(name.as_str()) {
                continue;
            }

            // With env-restart, the wrapper also exports the file's
            // fingerprint — what the liveness probe below compares the
            // CURRENT file against. File moves → probe fails → the
            // kubelet restarts this one container → the wrapper runs
            // again and sources the new file. The kernel's env-freeze
            // rule is honoured, not fought.
            let script = if request.env_restart {
                format!(
                    "set -a; . {path}; set +a; \
                     export DYNAMIC_CONFIG_ENV_FINGERPRINT=\"$(cksum < {path})\"; \
                     exec \"$@\"",
                    path = request.path
                )
            } else {
                format!("set -a; . {}; set +a; exec \"$@\"", request.path)
            };

            let mut wrapped = vec![
                json!("/bin/sh"),
                json!("-c"),
                json!(script),
                json!("dynamic-config-env"),
            ];

            wrapped.extend(container["command"].as_array().cloned().unwrap_or_default());
            wrapped.extend(container["args"].as_array().cloned().unwrap_or_default());

            patches.push(json!({
                "op": "replace",
                "path": format!("/spec/containers/{index}/command"),
                "value": wrapped,
            }));

            if container.get("args").is_some() {
                patches.push(json!({
                    "op": "remove",
                    "path": format!("/spec/containers/{index}/args"),
                }));
            }

            if request.env_restart {
                // Admission already refused a container that has its own
                // livenessProbe. Cadence follows the sidecar's: there is
                // no point probing faster than the file can change.
                let period = request.watch_seconds.clamp(10, 120);

                patches.push(json!({
                    "op": "add",
                    "path": format!("/spec/containers/{index}/livenessProbe"),
                    "value": {
                        "exec": { "command": [
                            "/bin/sh", "-c",
                            format!(
                                "test \"$(cksum < {})\" = \"$DYNAMIC_CONFIG_ENV_FINGERPRINT\"",
                                request.path
                            ),
                        ]},
                        "periodSeconds": period,
                        "failureThreshold": 1,
                    },
                }));
            }
        }
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
                // Overridable UID/GID (never root — admission refuses 0):
                // the rendered file's owner should match what the APP
                // container runs as, which is the whole reason a
                // tighter-than-default file-mode can still be read.
                "runAsUser": request.run_as_user.unwrap_or(65532),
                "runAsGroup": request.run_as_group.unwrap_or(65532),
                "allowPrivilegeEscalation": false,
                "capabilities": { "drop": ["ALL"] },
                "readOnlyRootFilesystem": true,
                "seccompProfile": { "type": "RuntimeDefault" },
            },
        });

        // Only the WATCHING agent serves metrics: a one-shot init has
        // nothing long-lived to scrape.
        if let (Some(port), true) = (request.metrics_port, watch) {
            container["args"]
                .as_array_mut()
                .expect("args is the array built above")
                .extend([json!("--metrics-addr"), json!(format!("0.0.0.0:{port}"))]);
            container["ports"] = json!([{ "containerPort": port, "name": "metrics" }]);
        }

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

    // A named render's agent: the same restricted container, its own
    // store flags, its own aux mounts — mode, resources and identity
    // stay pod-wide.
    let extra_agent = |extra: &annotations::ExtraRender, watch: bool| {
        let n = &extra.name;
        let mut arguments = vec!["--source".to_owned(), extra.source.clone()];

        if let Some(endpoint) = &extra.endpoint {
            arguments.push("--endpoint".to_owned());
            arguments.push(endpoint.clone());
        }

        arguments.push("--key".to_owned());
        arguments.push(extra.key.clone());
        arguments.push("--out".to_owned());
        arguments.push(extra.path.clone());

        for (flag, value) in &extra.arguments {
            arguments.push(flag.clone());
            arguments.push(value.clone());
        }

        if watch {
            arguments.push("--watch".to_owned());
            arguments.push(extra.watch_seconds.to_string());
        }

        let mut mounts = vec![json!({ "name": "dynamic-config", "mountPath": mount_dir })];

        if extra.ca.is_some() {
            mounts.push(json!({
                "name": format!("dynamic-config-ca-{n}"),
                "mountPath": format!("{}-{n}", annotations::CA_MOUNT),
                "readOnly": true,
            }));
        }

        if extra.ssh.is_some() {
            mounts.push(json!({
                "name": format!("dynamic-config-ssh-{n}"),
                "mountPath": format!("{}-{n}", annotations::SSH_MOUNT),
                "readOnly": true,
            }));
        }

        if extra.tls.is_some() {
            mounts.push(json!({
                "name": format!("dynamic-config-client-tls-{n}"),
                "mountPath": format!("{}-{n}", annotations::TLS_MOUNT),
                "readOnly": true,
            }));
        }

        if extra.template.is_some() {
            mounts.push(json!({
                "name": format!("dynamic-config-template-{n}"),
                "mountPath": format!("{}-{n}", annotations::TEMPLATE_MOUNT),
                "readOnly": true,
            }));
        }

        let mut limits = json!({ "memory": request.resources.memory_limit });

        if let Some(cpu) = &request.resources.cpu_limit {
            limits["cpu"] = json!(cpu);
        }

        let mut container = json!({
            "name": if watch {
                format!("dynamic-config-agent-{n}")
            } else {
                format!("dynamic-config-init-{n}")
            },
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
            "securityContext": {
                "runAsNonRoot": true,
                "runAsUser": request.run_as_user.unwrap_or(65532),
                "runAsGroup": request.run_as_group.unwrap_or(65532),
                "allowPrivilegeEscalation": false,
                "capabilities": { "drop": ["ALL"] },
                "readOnlyRootFilesystem": true,
                "seccompProfile": { "type": "RuntimeDefault" },
            },
        });

        if !extra.secret_env.is_empty() {
            container["env"] = extra
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

        for extra in &request.extra {
            patches.push(json!({
                "op": "add", "path": "/spec/initContainers/-", "value": extra_agent(extra, false),
            }));
        }
    }

    if matches!(request.mode, Mode::Sidecar | Mode::Both) {
        if request.native_sidecar {
            // The 1.29+ sidecar shape: an init container that restarts
            // always starts before the app containers, ends after them,
            // and does not stop a Job from finishing.
            if pod.pointer("/spec/initContainers").is_none() && !matches!(request.mode, Mode::Both)
            {
                patches.push(json!({
                    "op": "add", "path": "/spec/initContainers", "value": [],
                }));
            }

            let mut sidecar = agent(true);
            sidecar["restartPolicy"] = json!("Always");

            patches.push(json!({
                "op": "add", "path": "/spec/initContainers/-", "value": sidecar,
            }));

            for extra in &request.extra {
                let mut sidecar = extra_agent(extra, true);
                sidecar["restartPolicy"] = json!("Always");

                patches.push(json!({
                    "op": "add", "path": "/spec/initContainers/-", "value": sidecar,
                }));
            }
        } else {
            patches.push(json!({
                "op": "add", "path": "/spec/containers/-", "value": agent(true),
            }));

            for extra in &request.extra {
                patches.push(json!({
                    "op": "add", "path": "/spec/containers/-", "value": extra_agent(extra, true),
                }));
            }
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
