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
    /// `file-mode`: the rendered file's octal permissions, forwarded to
    /// the agent as `--file-mode`. `None` leaves the agent's default.
    pub file_mode: Option<String>,
    /// `agent-run-as-user` / `agent-run-as-group`: the injected
    /// container's UID/GID, so the rendered file's OWNER matches what
    /// the app container runs as — the Vault-injector shape. Root is
    /// refused; the restricted posture is not negotiable.
    pub run_as_user: Option<u32>,
    pub run_as_group: Option<u32>,
    /// `env-inject`: the app container (by name) whose command gets
    /// wrapped in `set -a; . <path>; set +a; exec …` — the rendered
    /// dotenv becomes the process's real environment. Start-time only,
    /// by Kubernetes' own rule: a running process's environ never
    /// changes, so the honest pairing is `mode: init` or `both`.
    pub env_inject: Option<String>,
    /// `env-restart: "true"`: when the sidecar re-renders the dotenv,
    /// the kubelet restarts JUST the app container (a liveness probe
    /// compares the file against the fingerprint the wrapper exported
    /// at start), and the wrapper re-sources the new file. The closest
    /// thing to a live env update the kernel permits — seconds, no pod
    /// recreation, no rescheduling, no new IP.
    pub env_restart: bool,
    /// `metrics-port`: the agent serves its Prometheus text here.
    pub metrics_port: Option<u16>,
    /// The named renders beyond the default one: `source.db`,
    /// `key.db`, `path.db` … — one more agent per name, every file in
    /// the SAME directory as the default `path` (one shared volume,
    /// refused otherwise). Mode, resources, identity and volume are
    /// pod-wide; everything store-shaped is per name.
    pub extra: Vec<ExtraRender>,
}

/// One named render: the per-store subset of [`Request`], parsed from
/// `<key>.<name>` annotations.
#[derive(Debug, PartialEq)]
pub struct ExtraRender {
    pub name: String,
    pub source: String,
    pub endpoint: Option<String>,
    pub key: String,
    pub path: String,
    pub watch_seconds: u64,
    pub arguments: Vec<(String, String)>,
    pub secret_env: Vec<(String, SecretRef)>,
    pub ca: Option<ObjectRef>,
    pub ssh: Option<ObjectRef>,
    pub tls: Option<String>,
    pub template: Option<ObjectRef>,
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
        "file-mode",
        "agent-run-as-user",
        "agent-run-as-group",
        "env-inject",
        "env-restart",
        "metrics-port",
        "template",
        "template-configmap",
    ];

    /// The keys a NAMED render may carry — everything store-shaped;
    /// mode, volume, resources and identity stay pod-wide.
    const PER_RENDER: &[&str] = &[
        "source",
        "endpoint",
        "endpoint-secret",
        "key",
        "path",
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
        "template",
        "template-configmap",
        "file-mode",
    ];

    let valid_suffix = |suffix: &str| {
        !suffix.is_empty()
            && suffix.len() <= 32
            && suffix
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };

    let mut extra_names: Vec<String> = Vec::new();

    for key in annotations.keys() {
        let Some(name) = key.strip_prefix(PREFIX) else {
            continue;
        };

        if KNOWN.contains(&name) {
            continue;
        }

        if let Some((base, suffix)) = name.split_once('.') {
            if PER_RENDER.contains(&base) && valid_suffix(suffix) {
                if !extra_names.iter().any(|n| n == suffix) {
                    extra_names.push(suffix.to_owned());
                }
                continue;
            }
        }

        return Err(format!(
            "{PREFIX}{name} is not part of the contract; the reference                  lists every key it takes"
        ));
    }

    extra_names.sort();

    // The agent would refuse these too — but at container start, after
    // scheduling. The admission is earlier and the message lands where
    // the operator is already looking.
    match get("source") {
        Some(
            "consul" | "vault" | "config-server" | "firestore" | "git" | "redis" | "etcd"
            | "nats" | "s3",
        )
        | None => {}
        Some(other) => {
            return Err(format!(
                "{PREFIX}source is {other:?}: one of consul, vault,                  config-server, firestore, git, redis, etcd, nats, s3"
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

    // The rendered file's permissions and the agent's identity — the
    // knobs that let a non-default app UID read a tighter-than-0644
    // file. Octal-validated here so the refusal lands at admission.
    let file_mode = match get("file-mode") {
        None => None,
        Some(text) => {
            let octal = text.strip_prefix("0o").unwrap_or(text);
            let value = u32::from_str_radix(octal, 8)
                .map_err(|_| format!("{PREFIX}file-mode is {text:?}: octal, like \"0640\""))?;

            if value > 0o777 {
                return Err(format!(
                    "{PREFIX}file-mode is {text:?}: at most 0777 — setuid bits \
                     on a configuration file answer no question"
                ));
            }

            if value & 0o400 == 0 {
                return Err(format!(
                    "{PREFIX}file-mode is {text:?}: the owner must at least \
                     read it, or the file is write-only noise"
                ));
            }

            Some(format!("{value:o}"))
        }
    };

    let identity = |name: &str| -> Result<Option<u32>, String> {
        match get(name) {
            None => Ok(None),
            Some(text) => {
                let id: u32 = text
                    .parse()
                    .map_err(|_| format!("{PREFIX}{name} is {text:?}: a numeric UID/GID"))?;

                if id == 0 {
                    return Err(format!(
                        "{PREFIX}{name} is 0: the agent stays nonroot in every \
                         configuration — an injector that relaxes a pod's \
                         posture is a finding, not a feature"
                    ));
                }

                Ok(Some(id))
            }
        }
    };

    let run_as_user = identity("agent-run-as-user")?;
    let run_as_group = identity("agent-run-as-group")?;
    let env_inject = get("env-inject").map(str::to_owned);

    if env_inject.is_some() && mode == Mode::Sidecar {
        return Err(format!(
            "{PREFIX}env-inject needs the file to exist BEFORE the app \
             starts: set {PREFIX}mode to \"init\" or \"both\" — environment \
             variables freeze at container start, which is Kubernetes' \
             rule, not this webhook's"
        ));
    }

    let env_restart = match get("env-restart").unwrap_or("false") {
        "true" => true,
        "false" => false,
        other => {
            return Err(format!(
                "{PREFIX}env-restart is {other:?}: \"true\" or \"false\""
            ))
        }
    };

    if env_restart && env_inject.is_none() {
        return Err(format!(
            "{PREFIX}env-restart without {PREFIX}env-inject restarts nothing \
             into nothing: name the container to wrap first"
        ));
    }

    if env_restart && mode != Mode::Both {
        return Err(format!(
            "{PREFIX}env-restart needs {PREFIX}mode \"both\": the init half \
             renders the file the app starts from, the sidecar half is what \
             ever CHANGES it — with init alone there is nothing to restart for"
        ));
    }

    let metrics_port = match get("metrics-port") {
        None => None,
        Some(text) => Some(
            text.parse::<u16>()
                .map_err(|_| format!("{PREFIX}metrics-port is {text:?}: a port number"))?,
        ),
    };

    if let Some(name) = &env_inject {
        let container = pod
            .pointer("/spec/containers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|c| c["name"].as_str() == Some(name));

        let Some(container) = container else {
            return Err(format!(
                "{PREFIX}env-inject names container {name:?}, and the pod has \
                 no such container"
            ));
        };

        if container["command"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(true)
        {
            return Err(format!(
                "{PREFIX}env-inject: container {name:?} has no `command` — an \
                 image ENTRYPOINT is invisible to the webhook, so there is \
                 nothing to wrap. Set the command explicitly"
            ));
        }

        if env_restart && container.get("livenessProbe").is_some() {
            return Err(format!(
                "{PREFIX}env-restart drives the kubelet through container \
                 {name:?}'s livenessProbe, and it already has one — two \
                 probes cannot share the slot. Drop yours or drop env-restart"
            ));
        }
    }

    if let Some(mode) = &file_mode {
        arguments.push(("--file-mode".to_owned(), format!("0{mode}")));
    }

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

    let mut extra = Vec::new();

    for suffix in &extra_names {
        extra.push(extra_render(
            suffix,
            &|name| get(&format!("{name}.{suffix}")),
            pod,
            get("path"),
        )?);
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
        file_mode,
        run_as_user,
        run_as_group,
        env_inject,
        env_restart,
        metrics_port,
        extra,
    }))
}

/// One named render's annotations, parsed with the same rules the
/// default render gets — refusals name the suffixed key.
fn extra_render<'a>(
    suffix: &str,
    get: &impl Fn(&str) -> Option<&'a str>,
    _pod: &Value,
    default_path: Option<&str>,
) -> Result<ExtraRender, String> {
    let label = |name: &str| format!("{PREFIX}{name}.{suffix}");

    let required = |name: &str| {
        get(name)
            .map(str::to_owned)
            .ok_or_else(|| format!("render {suffix:?} exists, so {} is required", label(name)))
    };

    let source = required("source")?;

    match source.as_str() {
        "consul" | "vault" | "config-server" | "firestore" | "git" | "redis" | "etcd" | "nats"
        | "s3" => {}
        other => {
            return Err(format!(
                "{} is {other:?}: one of consul, vault, config-server, \
                 firestore, git, redis, etcd, nats, s3",
                label("source")
            ))
        }
    }

    let key = required("key")?;
    let path = required("path")?;

    // One shared volume, so one shared directory: every named render's
    // file lives beside the default one.
    let parent = |p: &str| {
        std::path::Path::new(p)
            .parent()
            .and_then(|d| d.to_str())
            .unwrap_or("")
            .to_owned()
    };

    if let Some(default_path) = default_path {
        if parent(&path) != parent(default_path) {
            return Err(format!(
                "{} is {path:?}: every render shares ONE volume, so every \
                 file lives in the default path's directory ({:?})",
                label("path"),
                parent(default_path)
            ));
        }
    }

    let watch_seconds = match get("watch-seconds") {
        None => 15,
        Some(text) => text
            .parse()
            .map_err(|_| format!("{} is {text:?}: whole seconds", label("watch-seconds")))?,
    };

    let secret_ref = |name: &str| -> Result<Option<SecretRef>, String> {
        match get(name) {
            None => Ok(None),
            Some(text) => {
                let (secret, key) = text.split_once('/').ok_or(format!(
                    "{} is {text:?}: the form is <secret-name>/<key>",
                    label(name)
                ))?;

                Ok(Some(SecretRef {
                    name: secret.to_owned(),
                    key: key.to_owned(),
                }))
            }
        }
    };

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
            "render {suffix:?} exists, so {} is required (or {})",
            label("endpoint"),
            label("endpoint-secret")
        ));
    }

    if endpoint.is_some() && endpoint_from_secret {
        return Err(format!(
            "{} and {} are both set: one address, one place",
            label("endpoint"),
            label("endpoint-secret")
        ));
    }

    let ssh = object_ref("ssh-secret", "ssh-privatekey");
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
            None => arguments.push(("--auth".to_owned(), "ssh-key".to_owned())),
            Some("ssh-key") => {}
            Some(other) => {
                return Err(format!(
                    "{} ({}) with {} {other:?}: an ssh key is auth \"ssh-key\"",
                    label("ssh-secret"),
                    ssh.name,
                    label("auth")
                ))
            }
        }

        arguments.push((
            "--ssh-key".to_owned(),
            format!("{SSH_MOUNT}-{suffix}/{}", ssh.key),
        ));
    }

    let ca = object_ref("ca-configmap", "ca.crt");

    if let Some(ca) = &ca {
        arguments.push(("--ca".to_owned(), format!("{CA_MOUNT}-{suffix}/{}", ca.key)));
    }

    if let Some(text) = get("file-mode") {
        let octal = text.strip_prefix("0o").unwrap_or(text);
        let value = u32::from_str_radix(octal, 8)
            .map_err(|_| format!("{} is {text:?}: octal, like \"0640\"", label("file-mode")))?;

        if value > 0o777 || value & 0o400 == 0 {
            return Err(format!(
                "{} is {text:?}: octal, at most 0777, owner-readable",
                label("file-mode")
            ));
        }

        arguments.push(("--file-mode".to_owned(), format!("0{value:o}")));
    }

    let tls = get("tls-secret").map(str::to_owned);
    let template = object_ref("template-configmap", "template");

    match (get("template"), &template) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "{} and {} are both set: one template, one place",
                label("template"),
                label("template-configmap")
            ))
        }
        (Some(inline), None) => {
            arguments.push(("--template-inline".to_owned(), inline.to_owned()));
        }
        (None, Some(reference)) => {
            arguments.push((
                "--template".to_owned(),
                format!("{TEMPLATE_MOUNT}-{suffix}/{}", reference.key),
            ));
        }
        (None, None) => {}
    }

    if tls.is_some() {
        arguments.push((
            "--tls-cert".to_owned(),
            format!("{TLS_MOUNT}-{suffix}/tls.crt"),
        ));
        arguments.push((
            "--tls-key".to_owned(),
            format!("{TLS_MOUNT}-{suffix}/tls.key"),
        ));
    }

    Ok(ExtraRender {
        name: suffix.to_owned(),
        source,
        endpoint,
        key,
        path,
        watch_seconds,
        arguments,
        secret_env,
        ca,
        ssh,
        tls,
        template,
    })
}
