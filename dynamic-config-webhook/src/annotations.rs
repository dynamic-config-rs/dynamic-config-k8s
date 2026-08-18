//! The annotation contract, v1 — names chosen to read like the
//! agent-injector's, because that is the UX operators already know.
//!
//! ```yaml
//! metadata:
//!   annotations:
//!     dynamic-config.rs/inject: "true"
//!     dynamic-config.rs/source: "vault"
//!     dynamic-config.rs/endpoint: "https://vault.vault:8200"
//!     dynamic-config.rs/key: "secret/myapp"
//!     dynamic-config.rs/path: "/config/rendered.toml"
//!     dynamic-config.rs/mode: "sidecar"        # init | sidecar | both
//!     dynamic-config.rs/watch-seconds: "15"
//!     dynamic-config.rs/auth: "kubernetes"     # the store's method
//!     dynamic-config.rs/auth-role: "myapp"
//!     dynamic-config.rs/ca-configmap: "vault-ca"
//! ```
//!
//! Secret material never rides an annotation. A token, a password, an
//! endpoint that carries one — each names a Secret instead
//! (`token-secret: "name/key"`), and the webhook wires it into the agent
//! as an environment variable the agent already reads.

use serde_json::Value;

pub const PREFIX: &str = "dynamic-config.rs/";

/// Where the CA configmap and the ssh-key secret land in the agent
/// container. Fixed paths: the operator names the object, the webhook
/// owns the geography.
pub const CA_MOUNT: &str = "/etc/dynamic-config/ca";
pub const SSH_MOUNT: &str = "/etc/dynamic-config/ssh";
pub const TLS_MOUNT: &str = "/etc/dynamic-config/tls";
pub const TEMPLATE_MOUNT: &str = "/etc/dynamic-config/template";

/// The injected container's resource ask. Defaults sized for "fetch a
/// document and write a file", overridable fleet-wide through the
/// chart (environment) and per pod (annotations). There is no CPU
/// limit by default: throttling a config agent buys nothing and
/// delays reloads.
#[derive(Debug, PartialEq)]
pub struct Resources {
    pub cpu_request: String,
    pub memory_request: String,
    pub cpu_limit: Option<String>,
    pub memory_limit: String,
}

/// The fleet-wide defaults, read once — an admission decision must not
/// change between two requests because the environment moved.
struct Defaults {
    cpu_request: String,
    memory_request: String,
    cpu_limit: Option<String>,
    memory_limit: String,
}

fn defaults() -> &'static Defaults {
    static DEFAULTS: std::sync::OnceLock<Defaults> = std::sync::OnceLock::new();

    DEFAULTS.get_or_init(|| {
        let var = |name: &str, fallback: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| fallback.to_owned())
        };

        Defaults {
            cpu_request: var("DYNAMIC_CONFIG_AGENT_CPU_REQUEST", "10m"),
            memory_request: var("DYNAMIC_CONFIG_AGENT_MEMORY_REQUEST", "32Mi"),
            cpu_limit: std::env::var("DYNAMIC_CONFIG_AGENT_CPU_LIMIT")
                .ok()
                .filter(|v| !v.is_empty()),
            memory_limit: var("DYNAMIC_CONFIG_AGENT_MEMORY_LIMIT", "64Mi"),
        }
    })
}

#[derive(Debug, PartialEq)]
pub struct Request {
    pub source: String,
    /// Absent when `endpoint-secret` supplies it as an environment
    /// variable instead (a redis url with a password in it).
    pub endpoint: Option<String>,
    pub key: String,
    pub path: String,
    pub mode: Mode,
    pub watch_seconds: u64,
    /// Pass-through flags, already paired and ordered: the webhook
    /// forwards these to the agent verbatim and the agent validates
    /// them, so the two ends cannot drift apart on what they mean.
    pub arguments: Vec<(String, String)>,
    /// `DYNAMIC_CONFIG_AGENT_*` variables, each drawn from a Secret.
    pub secret_env: Vec<(String, SecretRef)>,
    pub ca: Option<ObjectRef>,
    pub ssh: Option<ObjectRef>,
    /// A client certificate, from a `kubernetes.io/tls` Secret — its two
    /// keys are `tls.crt` and `tls.key` by that type's own contract.
    pub tls: Option<String>,
    /// A minijinja template in a ConfigMap: the template owns the
    /// rendered bytes, and living in a ConfigMap it is reviewed and
    /// versioned like the code it effectively is.
    pub template: Option<ObjectRef>,
    /// `true` unless `volume-medium: "disk"`: rendered configuration can
    /// carry secrets, and a tmpfs-backed emptyDir keeps it off the
    /// node's disk.
    pub volume_memory: bool,
    /// `native-sidecar: "true"`: the watching agent runs as an init
    /// container with `restartPolicy: Always` — the Kubernetes 1.29+
    /// sidecar shape, which starts before the app and ends after it,
    /// and lets Jobs finish.
    pub native_sidecar: bool,
    pub resources: Resources,
}

#[derive(Debug, PartialEq)]
pub struct SecretRef {
    pub name: String,
    pub key: String,
}

/// A ConfigMap or Secret plus the key to read from it.
#[derive(Debug, PartialEq)]
pub struct ObjectRef {
    pub name: String,
    pub key: String,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Mode {
    Init,
    Sidecar,
    Both,
}

/// Reads the contract off a pod. `Ok(None)` is a pod that did not ask;
/// `Err` is a pod that asked wrongly, which must FAIL the admission —
/// silently not injecting is how a pod starts without the configuration
/// it declared it needs.
pub fn of_pod(pod: &Value) -> Result<Option<Request>, String> {
    let annotations = pod
        .pointer("/metadata/annotations")
        .and_then(Value::as_object);

    let Some(annotations) = annotations else {
        return Ok(None);
    };

    let get = |name: &str| {
        annotations
            .get(&format!("{PREFIX}{name}"))
            .and_then(Value::as_str)
    };

    match get("inject") {
        Some("true") => {}
        Some("false") | None => return Ok(None),
        Some(other) => {
            return Err(format!(
                "{PREFIX}inject is {other:?}: \"true\" or \"false\""
            ))
        }
    }

    // Every dynamic-config.rs/* key must be one this contract knows. A
    // typo'd `tokne-secret` silently ignored is a pod running without
    // the authentication it declared — the annotation prefix is claimed
    // territory, and an unknown key in it fails the admission.
    const KNOWN: &[&str] = &[
        "inject",
        "source",
        "endpoint",
        "endpoint-secret",
        "key",
        "path",
        "mode",
        "watch-seconds",
        "section",
        "auth",
        "auth-mount",
        "auth-role",
        "auth-username",
        "auth-token-path",
        "namespace",
        "ref",
        "api-url",
        "token-secret",
        "password-secret",
        "ca-configmap",
        "tls-secret",
        "ssh-secret",
        "volume-medium",
        "native-sidecar",
        "agent-cpu-request",
        "agent-memory-request",
        "agent-cpu-limit",
        "agent-memory-limit",
        "template",
        "template-configmap",
    ];

    for key in annotations.keys() {
        let Some(name) = key.strip_prefix(PREFIX) else {
            continue;
        };

        if !KNOWN.contains(&name) {
            return Err(format!(
                "{PREFIX}{name} is not part of the contract; the reference                  lists every key it takes"
            ));
        }
    }

    // The agent would refuse these too — but at container start, after
    // scheduling. The admission is earlier and the message lands where
    // the operator is already looking.
    match get("source") {
        Some("consul" | "vault" | "config-server" | "firestore" | "git" | "redis") | None => {}
        Some(waiting @ ("etcd" | "nats" | "s3")) => {
            return Err(format!(
                "{PREFIX}source {waiting:?} lands in 0.2.0 (its client is async);                  consul, vault, config-server, firestore, git and redis are the                  0.1 stores"
            ))
        }
        Some(other) => {
            return Err(format!(
                "{PREFIX}source is {other:?}: one of consul, vault,                  config-server, firestore, git, redis"
            ))
        }
    }

    let required = |name: &str| {
        get(name)
            .map(str::to_owned)
            .ok_or_else(|| format!("{PREFIX}inject is true, so {PREFIX}{name} is required"))
    };

    let mode = match get("mode").unwrap_or("sidecar") {
        "init" => Mode::Init,
        "sidecar" => Mode::Sidecar,
        "both" => Mode::Both,
        other => return Err(format!("{PREFIX}mode is {other:?}: init, sidecar or both")),
    };

    let watch_seconds = match get("watch-seconds") {
        None => 15,
        Some(text) => text
            .parse()
            .map_err(|_| format!("{PREFIX}watch-seconds is {text:?}: whole seconds"))?,
    };

    // "name/key", split once; the slash the form needs is the first one.
    let secret_ref = |name: &str| -> Result<Option<SecretRef>, String> {
        match get(name) {
            None => Ok(None),
            Some(text) => {
                let (secret, key) = text.split_once('/').ok_or(format!(
                    "{PREFIX}{name} is {text:?}: the form is <secret-name>/<key>"
                ))?;

                Ok(Some(SecretRef {
                    name: secret.to_owned(),
                    key: key.to_owned(),
                }))
            }
        }
    };

    // "name" or "name/key" — a default key exists for these.
    let object_ref = |name: &str, default_key: &str| -> Option<ObjectRef> {
        get(name).map(|text| match text.split_once('/') {
            Some((object, key)) => ObjectRef {
                name: object.to_owned(),
                key: key.to_owned(),
            },
            None => ObjectRef {
                name: text.to_owned(),
                key: default_key.to_owned(),
            },
        })
    };

    let mut secret_env = Vec::new();

    for (annotation, variable) in [
        ("token-secret", "DYNAMIC_CONFIG_AGENT_TOKEN"),
        ("password-secret", "DYNAMIC_CONFIG_AGENT_PASSWORD"),
        ("endpoint-secret", "DYNAMIC_CONFIG_AGENT_ENDPOINT"),
    ] {
        if let Some(reference) = secret_ref(annotation)? {
            secret_env.push((variable.to_owned(), reference));
        }
    }

    let endpoint = get("endpoint").map(str::to_owned);
    let endpoint_from_secret = secret_env
        .iter()
        .any(|(variable, _)| variable == "DYNAMIC_CONFIG_AGENT_ENDPOINT");

    if endpoint.is_none() && !endpoint_from_secret {
        return Err(format!(
            "{PREFIX}inject is true, so {PREFIX}endpoint is required \
             (or {PREFIX}endpoint-secret, when the address carries a password)"
        ));
    }

    if endpoint.is_some() && endpoint_from_secret {
        return Err(format!(
            "{PREFIX}endpoint and {PREFIX}endpoint-secret are both set: \
             one address, one place"
        ));
    }

    let ssh = object_ref("ssh-secret", "ssh-privatekey");

    // The pass-through flags, in one fixed order — the golden files
    // depend on it, which is the review gate a contract change deserves.
    let mut arguments = Vec::new();

    for (annotation, flag) in [
        ("section", "--section"),
        ("auth", "--auth"),
        ("auth-mount", "--auth-mount"),
        ("auth-role", "--auth-role"),
        ("auth-username", "--auth-username"),
        ("auth-token-path", "--auth-token-path"),
        ("namespace", "--namespace"),
        ("ref", "--ref"),
        ("api-url", "--api-url"),
    ] {
        if let Some(value) = get(annotation) {
            arguments.push((flag.to_owned(), value.to_owned()));
        }
    }

    if let Some(ssh) = &ssh {
        match get("auth") {
            // An ssh key was mounted and nothing else claimed the auth
            // method: the key is clearly the intent.
            None => arguments.push(("--auth".to_owned(), "ssh-key".to_owned())),
            Some("ssh-key") => {}
            Some(other) => {
                return Err(format!(
                    "{PREFIX}ssh-secret ({}) with {PREFIX}auth {other:?}: an ssh \
                     key is auth \"ssh-key\"",
                    ssh.name
                ))
            }
        }

        arguments.push(("--ssh-key".to_owned(), format!("{SSH_MOUNT}/{}", ssh.key)));
    }

    let ca = object_ref("ca-configmap", "ca.crt");

    if let Some(ca) = &ca {
        arguments.push(("--ca".to_owned(), format!("{CA_MOUNT}/{}", ca.key)));
    }

    let volume_memory = match get("volume-medium").unwrap_or("memory") {
        "memory" => true,
        "disk" => false,
        other => {
            return Err(format!(
                "{PREFIX}volume-medium is {other:?}: memory (default) or disk"
            ))
        }
    };

    let native_sidecar = match get("native-sidecar").unwrap_or("false") {
        "true" => true,
        "false" => false,
        other => {
            return Err(format!(
                "{PREFIX}native-sidecar is {other:?}: \"true\" or \"false\""
            ))
        }
    };

    // Light validation only — the API server owns the quantity grammar
    // and would reject the patched pod anyway; refusing the obvious
    // nonsense here puts the reason in the admission instead.
    let quantity = |name: &str, fallback: &str| -> Result<String, String> {
        let text = get(name).unwrap_or(fallback);
        let looks_sane = text.chars().next().is_some_and(|c| c.is_ascii_digit())
            && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '.');

        if looks_sane {
            Ok(text.to_owned())
        } else {
            Err(format!(
                "{PREFIX}{name} is {text:?}: a Kubernetes quantity, like \"50m\" or \"64Mi\""
            ))
        }
    };

    let fleet = defaults();

    let resources = Resources {
        cpu_request: quantity("agent-cpu-request", &fleet.cpu_request)?,
        memory_request: quantity("agent-memory-request", &fleet.memory_request)?,
        cpu_limit: match get("agent-cpu-limit").or(fleet.cpu_limit.as_deref()) {
            None => None,
            Some(_) => Some(quantity(
                "agent-cpu-limit",
                fleet.cpu_limit.as_deref().unwrap_or(""),
            )?),
        },
        memory_limit: quantity("agent-memory-limit", &fleet.memory_limit)?,
    };

    // A whole `kubernetes.io/tls` Secret: no key to choose, that type
    // fixed its two names years ago.
    let tls = get("tls-secret").map(str::to_owned);

    // Output templating: a one-liner inline, or a ConfigMap for
    // anything worth reviewing. One template, one place.
    let template = object_ref("template-configmap", "template");

    match (get("template"), &template) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "{PREFIX}template and {PREFIX}template-configmap are both set: \
                 one template, one place"
            ))
        }
        (Some(inline), None) => {
            arguments.push(("--template-inline".to_owned(), inline.to_owned()));
        }
        (None, Some(reference)) => {
            arguments.push((
                "--template".to_owned(),
                format!("{TEMPLATE_MOUNT}/{}", reference.key),
            ));
        }
        (None, None) => {}
    }

    if tls.is_some() {
        arguments.push(("--tls-cert".to_owned(), format!("{TLS_MOUNT}/tls.crt")));
        arguments.push(("--tls-key".to_owned(), format!("{TLS_MOUNT}/tls.key")));
    }

    Ok(Some(Request {
        source: required("source")?,
        endpoint,
        key: required("key")?,
        path: required("path")?,
        mode,
        watch_seconds,
        arguments,
        secret_env,
        ca,
        ssh,
        tls,
        template,
        volume_memory,
        native_sidecar,
        resources,
    }))
}
