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

/// Every store the agent speaks — the one list the source annotation,
/// the per-store defaults and the source gates all validate against.
pub(crate) const SOURCES: &[&str] = &[
    "consul",
    "vault",
    "config-server",
    "firestore",
    "git",
    "redis",
    "etcd",
    "nats",
    "s3",
];

/// Names grouped by namespace — the shape every gate shares. Grammar:
/// semicolons between groups, an optional `namespace:` head on each
/// (absent means every namespace), commas between names. What counts
/// as a valid NAME differs per gate, so parsing takes the validator.
#[derive(Debug, Default, PartialEq)]
pub struct ScopedNames {
    /// `(namespace, names)` — namespace `*` matches every one.
    groups: Vec<(String, Vec<String>)>,
}

impl ScopedNames {
    /// The empty list.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    fn parse(spec: &str, valid: &impl Fn(&str) -> bool, kind: &str) -> Result<Self, String> {
        let mut groups = Vec::new();

        for group in spec.split(';').map(str::trim).filter(|g| !g.is_empty()) {
            let (namespace, names) = match group.split_once(':') {
                Some((namespace, names)) => (namespace.trim(), names),
                None => ("*", group),
            };

            let namespace_ok = namespace == "*"
                || (!namespace.is_empty()
                    && namespace
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));

            if !namespace_ok {
                return Err(format!(
                    "{namespace:?} is not a namespace (lowercase RFC 1123) or \"*\""
                ));
            }

            let mut list = Vec::new();

            for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                if !(name == "*" || valid(name)) {
                    return Err(format!("{name:?} is not {kind}"));
                }

                list.push(name.to_owned());
            }

            if list.is_empty() {
                return Err(format!("the group for {namespace:?} names nothing"));
            }

            groups.push((namespace.to_owned(), list));
        }

        Ok(Self { groups })
    }

    /// Environment variable names: UPPER_SNAKE, an optional trailing
    /// `*` as a prefix glob.
    pub fn env_names(spec: &str) -> Result<Self, String> {
        Self::parse(
            spec,
            &|name| {
                let stem = name.strip_suffix('*').unwrap_or(name);

                stem.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
                    && stem
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            },
            "a variable name (UPPER_SNAKE, an optional trailing \"*\")",
        )
    }

    /// Store names, each from [`SOURCES`] — a typo in a security gate
    /// must not silently gate nothing.
    pub fn sources(spec: &str) -> Result<Self, String> {
        Self::parse(
            spec,
            &|name| SOURCES.contains(&name),
            "a store this contract knows",
        )
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    #[must_use]
    pub fn allows(&self, namespace: &str, name: &str) -> bool {
        self.groups
            .iter()
            .filter(|(scope, _)| scope == "*" || scope == namespace)
            .flat_map(|(_, names)| names)
            .any(|pattern| match pattern.strip_suffix('*') {
                Some(prefix) if pattern != "*" => name.starts_with(prefix),
                Some(_) => true,
                None => pattern == name,
            })
    }

    /// What THIS namespace gets — for refusals, which should name the
    /// fix. Only the asking namespace's rules: one tenant's refusal
    /// must not enumerate another's.
    #[must_use]
    pub fn listing(&self, namespace: &str) -> String {
        let names: Vec<&str> = self
            .groups
            .iter()
            .filter(|(scope, _)| scope == "*" || scope == namespace)
            .flat_map(|(_, names)| names)
            .map(String::as_str)
            .collect();

        names.join(", ")
    }
}

/// Octal permissions, validated the same wherever they come from — the
/// context names whoever supplied the value, so the refusal lands where
/// the mistake was made.
fn octal_mode(context: &str, text: &str) -> Result<String, String> {
    let octal = text.strip_prefix("0o").unwrap_or(text);
    let value = u32::from_str_radix(octal, 8)
        .map_err(|_| format!("{context} is {text:?}: octal, like \"0640\""))?;

    if value > 0o777 {
        return Err(format!(
            "{context} is {text:?}: at most 0777 — setuid bits \
             on a configuration file answer no question"
        ));
    }

    if value & 0o400 == 0 {
        return Err(format!(
            "{context} is {text:?}: the owner must at least \
             read it, or the file is write-only noise"
        ));
    }

    Ok(format!("{value:o}"))
}

/// A Kubernetes quantity, loosely: the API server owns the grammar and
/// would reject the pod anyway; this puts the obvious nonsense where
/// whoever wrote it is looking.
fn sane_quantity(text: &str) -> bool {
    text.chars().next().is_some_and(|c| c.is_ascii_digit())
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

/// A nonzero UID or GID — the agent stays nonroot in every
/// configuration; an injector that relaxes a pod's posture is a
/// finding, not a feature.
fn nonroot_id(context: &str, text: &str) -> Result<u32, String> {
    let id: u32 = text
        .parse()
        .map_err(|_| format!("{context} is {text:?}: a numeric UID/GID"))?;

    if id == 0 {
        return Err(format!(
            "{context} is 0: the agent stays nonroot in every configuration"
        ));
    }

    Ok(id)
}

/// `NAME=value` pairs, comma-separated — the grammar `agent-env` and
/// the fleet's `DYNAMIC_CONFIG_AGENT_ENV` share.
fn env_entries(context: &str, text: &str) -> Result<Vec<(String, String)>, String> {
    let mut entries: Vec<(String, String)> = Vec::new();

    for entry in text.split(',') {
        let entry = entry.trim();

        if entry.is_empty() {
            continue;
        }

        let Some((name, value)) = entry.split_once('=') else {
            return Err(format!(
                "{context} holds {entry:?}: comma-separated NAME=value \
                 pairs (a value itself cannot contain a comma)"
            ));
        };

        let name = name.trim();
        let head = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_');

        if !head
            || !name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(format!(
                "{context} names {name:?}: variable names are UPPER_SNAKE, \
                 like \"RUST_LOG\""
            ));
        }

        if entries.iter().any(|(existing, _)| existing == name) {
            return Err(format!("{context} sets {name} twice"));
        }

        entries.push((name.to_owned(), value.to_owned()));
    }

    if entries.is_empty() {
        return Err(format!(
            "{context} is present and names nothing: drop it or fill it"
        ));
    }

    Ok(entries)
}

/// One tier of defaults — the fleet's, or one store's. Every knob a pod
/// could otherwise only set per annotation; `None` falls through to the
/// next tier (store → fleet → built-in).
#[derive(Debug, Default)]
struct KnobDefaults {
    cpu_request: Option<String>,
    memory_request: Option<String>,
    cpu_limit: Option<String>,
    memory_limit: Option<String>,
    file_mode: Option<String>,
    watch_seconds: Option<u64>,
    mode: Option<Mode>,
    volume_memory: Option<bool>,
    native_sidecar: Option<bool>,
    run_as_user: Option<u32>,
    run_as_group: Option<u32>,
    metrics_port: Option<u16>,
}

/// The knob keys, spelled exactly as the annotations spell them — one
/// vocabulary, whether a value arrives per pod or per installation.
const KNOBS: &str = "agent-cpu-request, agent-memory-request, \
     agent-cpu-limit, agent-memory-limit, file-mode, watch-seconds, \
     mode, volume-medium, native-sidecar, agent-run-as-user, \
     agent-run-as-group, metrics-port";

impl KnobDefaults {
    /// One `key=value`, validated exactly as the matching annotation
    /// would be. The context names where the value came from.
    fn set(&mut self, context: &str, key: &str, value: &str) -> Result<(), String> {
        match key {
            "agent-cpu-request"
            | "agent-memory-request"
            | "agent-cpu-limit"
            | "agent-memory-limit" => {
                if !sane_quantity(value) {
                    return Err(format!(
                        "{context}: {key} is {value:?}: a Kubernetes quantity, \
                         like \"50m\" or \"64Mi\""
                    ));
                }

                let slot = match key {
                    "agent-cpu-request" => &mut self.cpu_request,
                    "agent-memory-request" => &mut self.memory_request,
                    "agent-cpu-limit" => &mut self.cpu_limit,
                    _ => &mut self.memory_limit,
                };
                *slot = Some(value.to_owned());
            }
            "file-mode" => {
                self.file_mode = Some(octal_mode(&format!("{context}: file-mode"), value)?);
            }
            "watch-seconds" => {
                self.watch_seconds = Some(value.parse().map_err(|_| {
                    format!("{context}: watch-seconds is {value:?}: whole seconds")
                })?);
            }
            "mode" => {
                self.mode = Some(match value {
                    "init" => Mode::Init,
                    "sidecar" => Mode::Sidecar,
                    "both" => Mode::Both,
                    other => {
                        return Err(format!(
                            "{context}: mode is {other:?}: init, sidecar or both"
                        ))
                    }
                });
            }
            "volume-medium" => {
                self.volume_memory = Some(match value {
                    "memory" => true,
                    "disk" => false,
                    other => {
                        return Err(format!(
                            "{context}: volume-medium is {other:?}: memory or disk"
                        ))
                    }
                });
            }
            "native-sidecar" => {
                self.native_sidecar = Some(match value {
                    "true" => true,
                    "false" => false,
                    other => {
                        return Err(format!(
                            "{context}: native-sidecar is {other:?}: \"true\" or \"false\""
                        ))
                    }
                });
            }
            "agent-run-as-user" => {
                self.run_as_user =
                    Some(nonroot_id(&format!("{context}: agent-run-as-user"), value)?);
            }
            "agent-run-as-group" => {
                self.run_as_group = Some(nonroot_id(
                    &format!("{context}: agent-run-as-group"),
                    value,
                )?);
            }
            "metrics-port" => {
                self.metrics_port =
                    Some(value.parse().map_err(|_| {
                        format!("{context}: metrics-port is {value:?}: a port number")
                    })?);
            }
            other => {
                return Err(format!(
                    "{context}: {other:?} is not a defaultable knob; the knobs \
                     are {KNOBS}"
                ))
            }
        }

        Ok(())
    }
}

/// The environment variable each fleet-wide knob reads from.
const FLEET_KNOBS: &[(&str, &str)] = &[
    ("DYNAMIC_CONFIG_AGENT_CPU_REQUEST", "agent-cpu-request"),
    (
        "DYNAMIC_CONFIG_AGENT_MEMORY_REQUEST",
        "agent-memory-request",
    ),
    ("DYNAMIC_CONFIG_AGENT_CPU_LIMIT", "agent-cpu-limit"),
    ("DYNAMIC_CONFIG_AGENT_MEMORY_LIMIT", "agent-memory-limit"),
    ("DYNAMIC_CONFIG_AGENT_FILE_MODE", "file-mode"),
    ("DYNAMIC_CONFIG_AGENT_WATCH_SECONDS", "watch-seconds"),
    ("DYNAMIC_CONFIG_AGENT_MODE", "mode"),
    ("DYNAMIC_CONFIG_AGENT_VOLUME_MEDIUM", "volume-medium"),
    ("DYNAMIC_CONFIG_AGENT_NATIVE_SIDECAR", "native-sidecar"),
    ("DYNAMIC_CONFIG_AGENT_RUN_AS_USER", "agent-run-as-user"),
    ("DYNAMIC_CONFIG_AGENT_RUN_AS_GROUP", "agent-run-as-group"),
    ("DYNAMIC_CONFIG_AGENT_METRICS_PORT", "metrics-port"),
];

/// Everything an installation decides: fleet defaults, per-store
/// defaults, fleet-wide agent environment, and the gates. Read once —
/// an admission decision must not change between two requests because
/// the environment moved — and validated in FULL at startup, so a
/// mistyped value stops the install, never the first admission.
#[derive(Debug)]
pub struct Installation {
    fleet: KnobDefaults,
    per_store: Vec<(String, KnobDefaults)>,
    /// `DYNAMIC_CONFIG_AGENT_ENV`: environment every injected agent
    /// gets. Installer-set, so no allowlist applies; a pod's own
    /// `agent-env` overrides it name by name.
    agent_env: Vec<(String, String)>,
    agent_env_allow: ScopedNames,
    /// Empty = every source, everywhere. Non-empty = ONLY these.
    source_allow: ScopedNames,
    /// Always subtractive, and it outranks the allowlist.
    source_deny: ScopedNames,
}

impl Installation {
    /// From any lookup, so the tests can feed a map where the server
    /// feeds `std::env::var`. Every refusal names the variable it is
    /// about — the reader is an installer staring at a chart.
    pub fn from_lookup(lookup: &impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |name: &str| lookup(name).filter(|v| !v.is_empty());

        let mut fleet = KnobDefaults::default();

        for (variable, knob) in FLEET_KNOBS {
            if let Some(text) = var(variable) {
                fleet.set(variable, knob, &text)?;
            }
        }

        let mut per_store: Vec<(String, KnobDefaults)> = Vec::new();

        for group in var("DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS")
            .as_deref()
            .unwrap_or_default()
            .split(';')
            .map(str::trim)
            .filter(|g| !g.is_empty())
        {
            let context = "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS";

            let Some((store, knobs)) = group.split_once(':') else {
                return Err(format!(
                    "{context}: {group:?} has no `store:` head; the form is \
                     \"vault: watch-seconds=30, file-mode=0640; s3: …\""
                ));
            };

            let store = store.trim();

            if !SOURCES.contains(&store) {
                return Err(format!(
                    "{context}: {store:?} is not a store this contract knows"
                ));
            }

            if per_store.iter().any(|(name, _)| name == store) {
                return Err(format!("{context}: {store} appears twice"));
            }

            let mut defaults = KnobDefaults::default();

            for pair in knobs.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let Some((key, value)) = pair.split_once('=') else {
                    return Err(format!(
                        "{context}: {pair:?} under {store} is not key=value"
                    ));
                };

                defaults.set(&format!("{context} ({store})"), key.trim(), value.trim())?;
            }

            per_store.push((store.to_owned(), defaults));
        }

        let agent_env = match var("DYNAMIC_CONFIG_AGENT_ENV") {
            None => Vec::new(),
            Some(text) => env_entries("DYNAMIC_CONFIG_AGENT_ENV", &text)?,
        };

        let scoped = |variable: &str,
                      parse: fn(&str) -> Result<ScopedNames, String>|
         -> Result<ScopedNames, String> {
            match var(variable) {
                None => Ok(ScopedNames::none()),
                Some(spec) => parse(&spec).map_err(|e| format!("{variable}: {e}")),
            }
        };

        Ok(Installation {
            fleet,
            per_store,
            agent_env,
            agent_env_allow: scoped(
                "DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW",
                ScopedNames::env_names,
            )?,
            source_allow: scoped("DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW", ScopedNames::sources)?,
            source_deny: scoped("DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY", ScopedNames::sources)?,
        })
    }

    fn from_environment() -> Result<Self, String> {
        Self::from_lookup(&|name| std::env::var(name).ok())
    }

    fn store(&self, source: Option<&str>) -> Option<&KnobDefaults> {
        let source = source?;

        self.per_store
            .iter()
            .find(|(name, _)| name == source)
            .map(|(_, defaults)| defaults)
    }

    /// Store default, then fleet default — the per-annotation value is
    /// resolved at the parse site, above both.
    fn knob<T: Clone>(
        &self,
        source: Option<&str>,
        pick: impl Fn(&KnobDefaults) -> Option<T>,
    ) -> Option<T> {
        self.store(source)
            .and_then(&pick)
            .or_else(|| pick(&self.fleet))
    }

    /// Is this source turned off here outright?
    #[must_use]
    pub fn source_denied(&self, namespace: &str, source: &str) -> bool {
        self.source_deny.allows(namespace, source)
    }

    /// Does the allowlist (when one exists) admit this source here?
    #[must_use]
    pub fn source_allowed(&self, namespace: &str, source: &str) -> bool {
        self.source_allow.is_empty() || self.source_allow.allows(namespace, source)
    }

    /// May a pod in this namespace set this agent-env name?
    #[must_use]
    pub fn agent_env_allowed(&self, namespace: &str, name: &str) -> bool {
        self.agent_env_allow.allows(namespace, name)
    }

    /// The agent-env names this namespace may set, for the refusal.
    #[must_use]
    pub fn agent_env_listing(&self, namespace: &str) -> String {
        self.agent_env_allow.listing(namespace)
    }

    /// The sources this namespace may use, for the refusal.
    #[must_use]
    pub fn source_listing(&self, namespace: &str) -> String {
        self.source_allow.listing(namespace)
    }
}

/// Startup-time validation: a fleet default the installer mistyped must
/// stop the webhook BEFORE it serves. Helm and kustomize both land
/// here — the chart cannot validate an env var a kustomize patch set,
/// so the process door is the one gate every install path walks
/// through.
pub fn verify_installation() -> Result<(), String> {
    Installation::from_environment().map(|_| ())
}

pub(crate) fn installation() -> &'static Installation {
    static INSTALLATION: std::sync::OnceLock<Installation> = std::sync::OnceLock::new();

    // `verify_installation` ran before serving, so this cannot fail in
    // a server; a test with a broken environment fails loudly instead.
    INSTALLATION.get_or_init(|| Installation::from_environment().expect("verified at startup"))
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
    /// `aws-secret`: a Secret whose `AWS_ACCESS_KEY_ID` and
    /// `AWS_SECRET_ACCESS_KEY` keys become exactly those variables on
    /// the agent — static credentials for S3-compatible stores that are
    /// not AWS (MinIO, Ceph, R2). On AWS itself, IRSA needs none of it.
    pub aws_secret: Option<String>,
    /// `agent-env`: extra environment on the injected agent container
    /// (SDK knobs: proxies, `RUST_LOG`, `AWS_CA_BUNDLE`). Pod-wide —
    /// every render's agent gets it — and admitted only through the
    /// installation's allowlist, checked against the pod's namespace.
    pub agent_env: Vec<(String, String)>,
    /// The installer's fleet-wide agent environment
    /// (`DYNAMIC_CONFIG_AGENT_ENV`), already merged: pod-set names
    /// removed, `aws-secret`-owned names removed. No allowlist applies
    /// — the installer owns both the values and the gate.
    pub fleet_env: Vec<(String, String)>,
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
    of_pod_with(pod, installation())
}

/// [`of_pod`] with the installation made explicit — the server feeds
/// its own; tests construct theirs through
/// [`Installation::from_lookup`].
pub fn of_pod_with(pod: &Value, install: &Installation) -> Result<Option<Request>, String> {
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
        "aws-secret",
        "agent-env",
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
        Some(source) if !SOURCES.contains(&source) => {
            return Err(format!(
                "{PREFIX}source is {source:?}: one of {}",
                SOURCES.join(", ")
            ))
        }
        _ => {}
    }

    let required = |name: &str| {
        get(name)
            .map(str::to_owned)
            .ok_or_else(|| format!("{PREFIX}inject is true, so {PREFIX}{name} is required"))
    };

    // Every knob below resolves annotation → this store's installation
    // default → the fleet's → the built-in. Pod-wide knobs (mode,
    // volume, resources, identity) take the DEFAULT render's store.
    let source_name = get("source");

    let mode = match get("mode") {
        Some("init") => Mode::Init,
        Some("sidecar") => Mode::Sidecar,
        Some("both") => Mode::Both,
        Some(other) => return Err(format!("{PREFIX}mode is {other:?}: init, sidecar or both")),
        None => install
            .knob(source_name, |k| k.mode)
            .unwrap_or(Mode::Sidecar),
    };

    let watch_seconds = match get("watch-seconds") {
        None => install.knob(source_name, |k| k.watch_seconds).unwrap_or(15),
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

    let volume_memory = match get("volume-medium") {
        Some("memory") => true,
        Some("disk") => false,
        Some(other) => {
            return Err(format!(
                "{PREFIX}volume-medium is {other:?}: memory (default) or disk"
            ))
        }
        None => install
            .knob(source_name, |k| k.volume_memory)
            .unwrap_or(true),
    };

    let native_sidecar = match get("native-sidecar") {
        Some("true") => true,
        Some("false") => false,
        Some(other) => {
            return Err(format!(
                "{PREFIX}native-sidecar is {other:?}: \"true\" or \"false\""
            ))
        }
        None => install
            .knob(source_name, |k| k.native_sidecar)
            .unwrap_or(false),
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

    let cpu_request = install
        .knob(source_name, |k| k.cpu_request.clone())
        .unwrap_or_else(|| "10m".to_owned());
    let memory_request = install
        .knob(source_name, |k| k.memory_request.clone())
        .unwrap_or_else(|| "32Mi".to_owned());
    let memory_limit = install
        .knob(source_name, |k| k.memory_limit.clone())
        .unwrap_or_else(|| "64Mi".to_owned());
    let cpu_limit = install.knob(source_name, |k| k.cpu_limit.clone());

    let resources = Resources {
        cpu_request: quantity("agent-cpu-request", &cpu_request)?,
        memory_request: quantity("agent-memory-request", &memory_request)?,
        cpu_limit: match get("agent-cpu-limit").or(cpu_limit.as_deref()) {
            None => None,
            Some(_) => Some(quantity(
                "agent-cpu-limit",
                cpu_limit.as_deref().unwrap_or(""),
            )?),
        },
        memory_limit: quantity("agent-memory-limit", &memory_limit)?,
    };

    // The rendered file's permissions and the agent's identity — the
    // knobs that let a non-default app UID read a tighter-than-0644
    // file. Octal-validated here so the refusal lands at admission.
    let file_mode = match get("file-mode") {
        None => install.knob(source_name, |k| k.file_mode.clone()),
        Some(text) => Some(octal_mode(&format!("{PREFIX}file-mode"), text)?),
    };

    let identity = |name: &str| -> Result<Option<u32>, String> {
        match get(name) {
            None => Ok(None),
            Some(text) => Ok(Some(nonroot_id(&format!("{PREFIX}{name}"), text)?)),
        }
    };

    let run_as_user =
        identity("agent-run-as-user")?.or_else(|| install.knob(source_name, |k| k.run_as_user));
    let run_as_group =
        identity("agent-run-as-group")?.or_else(|| install.knob(source_name, |k| k.run_as_group));
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

    let aws_secret = get("aws-secret").map(str::to_owned);

    if aws_secret.is_some() && get("source") != Some("s3") {
        return Err(format!(
            "{PREFIX}aws-secret is the s3 source's flag; the source here is {:?}",
            get("source").unwrap_or("unset")
        ));
    }

    let metrics_port = match get("metrics-port") {
        None => install.knob(source_name, |k| k.metrics_port),
        // "0" is the per-pod opt-OUT of an installation-wide default.
        Some("0") => None,
        Some(text) => Some(
            text.parse::<u16>()
                .map_err(|_| format!("{PREFIX}metrics-port is {text:?}: a port number"))?,
        ),
    };

    // The format is validated here; whether the NAMES may pass at all
    // is the installation's allowlist, checked by the admission with
    // the namespace in hand.
    let agent_env = match get("agent-env") {
        None => Vec::new(),
        Some(text) => env_entries(&format!("{PREFIX}agent-env"), text)?,
    };

    for (name, _) in &agent_env {
        if aws_secret.is_some()
            && ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"].contains(&name.as_str())
        {
            return Err(format!(
                "{PREFIX}agent-env sets {name}, and {PREFIX}aws-secret already \
                 does: one credential, one place"
            ));
        }
    }

    // The installer's fleet-wide environment rides along under the
    // pod's own: same name, the pod wins; the aws-secret names step
    // aside for the Secret the pod chose.
    let fleet_env: Vec<(String, String)> = install
        .agent_env
        .iter()
        .filter(|(name, _)| !agent_env.iter().any(|(pod_name, _)| pod_name == name))
        .filter(|(name, _)| {
            aws_secret.is_none()
                || !["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"].contains(&name.as_str())
        })
        .cloned()
        .collect();

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
            install,
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
        aws_secret,
        agent_env,
        fleet_env,
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
    install: &Installation,
) -> Result<ExtraRender, String> {
    let label = |name: &str| format!("{PREFIX}{name}.{suffix}");

    let required = |name: &str| {
        get(name)
            .map(str::to_owned)
            .ok_or_else(|| format!("render {suffix:?} exists, so {} is required", label(name)))
    };

    let source = required("source")?;

    if !SOURCES.contains(&source.as_str()) {
        return Err(format!(
            "{} is {:?}: one of {}",
            label("source"),
            source,
            SOURCES.join(", ")
        ));
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

    // Per-render knobs resolve against the RENDER's own store.
    let watch_seconds = match get("watch-seconds") {
        None => install
            .knob(Some(&source), |k| k.watch_seconds)
            .unwrap_or(15),
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

    let file_mode = match get("file-mode") {
        None => install.knob(Some(&source), |k| k.file_mode.clone()),
        Some(text) => Some(octal_mode(&label("file-mode"), text)?),
    };

    if let Some(mode) = file_mode {
        arguments.push(("--file-mode".to_owned(), format!("0{mode}")));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn installation_defaults_hold_without_environment() {
        let install = Installation::from_lookup(&lookup(&[])).unwrap();

        assert_eq!(install.fleet.cpu_request, None);
        assert_eq!(install.fleet.file_mode, None);
        assert!(install.per_store.is_empty());
        assert!(install.agent_env.is_empty());
        assert!(install.source_allowed("anywhere", "vault"));
        assert!(!install.source_denied("anywhere", "vault"));
        assert!(!install.agent_env_allowed("anywhere", "RUST_LOG"));
    }

    #[test]
    fn installation_validates_every_knob() {
        let cases: &[(&str, &str, &str)] = &[
            ("DYNAMIC_CONFIG_AGENT_FILE_MODE", "888", "octal"),
            ("DYNAMIC_CONFIG_AGENT_FILE_MODE", "7777", "at most 0777"),
            ("DYNAMIC_CONFIG_AGENT_FILE_MODE", "044", "owner"),
            (
                "DYNAMIC_CONFIG_AGENT_WATCH_SECONDS",
                "soon",
                "whole seconds",
            ),
            ("DYNAMIC_CONFIG_AGENT_CPU_REQUEST", "lots", "quantity"),
            ("DYNAMIC_CONFIG_AGENT_CPU_LIMIT", "-1", "quantity"),
            (
                "DYNAMIC_CONFIG_AGENT_MODE",
                "detached",
                "init, sidecar or both",
            ),
            (
                "DYNAMIC_CONFIG_AGENT_VOLUME_MEDIUM",
                "tape",
                "memory or disk",
            ),
            (
                "DYNAMIC_CONFIG_AGENT_NATIVE_SIDECAR",
                "yes",
                "\"true\" or \"false\"",
            ),
            ("DYNAMIC_CONFIG_AGENT_RUN_AS_USER", "0", "nonroot"),
            ("DYNAMIC_CONFIG_AGENT_RUN_AS_GROUP", "root", "numeric"),
            ("DYNAMIC_CONFIG_AGENT_METRICS_PORT", "http", "port"),
            ("DYNAMIC_CONFIG_AGENT_ENV", "rust_log=1", "UPPER_SNAKE"),
            (
                "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
                "watch-seconds=30",
                "no `store:` head",
            ),
            (
                "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
                "sql: watch-seconds=30",
                "not a store",
            ),
            (
                "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
                "vault: color=red",
                "not a defaultable knob",
            ),
            (
                "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
                "vault: file-mode=888",
                "octal",
            ),
            (
                "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
                "vault: mode=init; vault: mode=both",
                "twice",
            ),
            (
                "DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW",
                "rust_log",
                "UPPER_SNAKE",
            ),
            (
                "DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW",
                "Payments:X",
                "namespace",
            ),
            (
                "DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW",
                "payments:",
                "names nothing",
            ),
            (
                "DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW",
                "payments: sql",
                "not a store",
            ),
            (
                "DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY",
                "payments: VAULT",
                "not a store",
            ),
        ];

        for (name, value, expected) in cases {
            let error = Installation::from_lookup(&lookup(&[(name, value)])).unwrap_err();

            assert!(
                error.contains(name) && error.contains(expected),
                "{name}={value}: {error}"
            );
        }
    }

    #[test]
    fn installation_accepts_the_documented_shapes() {
        let install = Installation::from_lookup(&lookup(&[
            ("DYNAMIC_CONFIG_AGENT_FILE_MODE", "0640"),
            ("DYNAMIC_CONFIG_AGENT_WATCH_SECONDS", "30"),
            ("DYNAMIC_CONFIG_AGENT_MODE", "both"),
            ("DYNAMIC_CONFIG_AGENT_VOLUME_MEDIUM", "disk"),
            ("DYNAMIC_CONFIG_AGENT_NATIVE_SIDECAR", "true"),
            ("DYNAMIC_CONFIG_AGENT_RUN_AS_USER", "1000"),
            ("DYNAMIC_CONFIG_AGENT_RUN_AS_GROUP", "1000"),
            ("DYNAMIC_CONFIG_AGENT_METRICS_PORT", "9102"),
            ("DYNAMIC_CONFIG_AGENT_ENV", "HTTPS_PROXY=http://egress:3128"),
            (
                "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS",
                "vault: watch-seconds=10, file-mode=0400; s3: agent-memory-limit=128Mi",
            ),
            (
                "DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW",
                "payments: HTTPS_PROXY, AWS_*; *: RUST_LOG",
            ),
            (
                "DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW",
                "payments: vault, s3; *: consul",
            ),
            ("DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY", "sandbox: git"),
        ]))
        .unwrap();

        assert_eq!(install.fleet.file_mode.as_deref(), Some("640"));
        assert_eq!(install.fleet.mode, Some(Mode::Both));
        assert_eq!(install.fleet.volume_memory, Some(false));
        assert_eq!(install.fleet.run_as_user, Some(1000));
        assert_eq!(install.fleet.metrics_port, Some(9102));
        assert_eq!(install.agent_env.len(), 1);

        // Store default outranks fleet; an unlisted store falls to fleet.
        assert_eq!(install.knob(Some("vault"), |k| k.watch_seconds), Some(10));
        assert_eq!(install.knob(Some("redis"), |k| k.watch_seconds), Some(30));
        assert_eq!(
            install
                .knob(Some("vault"), |k| k.file_mode.clone())
                .as_deref(),
            Some("400")
        );
        assert_eq!(
            install
                .knob(Some("s3"), |k| k.memory_limit.clone())
                .as_deref(),
            Some("128Mi")
        );

        assert!(install.agent_env_allowed("payments", "AWS_CA_BUNDLE"));
        assert!(!install.agent_env_allowed("elsewhere", "HTTPS_PROXY"));
        assert!(install.source_allowed("payments", "vault"));
        assert!(install.source_allowed("payments", "consul"));
        assert!(!install.source_allowed("payments", "git"));
        assert!(install.source_denied("sandbox", "git"));
        assert!(!install.source_denied("payments", "git"));
    }

    #[test]
    fn the_gates_scope_by_namespace_and_prefix() {
        let allow = ScopedNames::env_names("payments: HTTPS_PROXY, AWS_*; *: RUST_LOG").unwrap();

        assert!(allow.allows("payments", "AWS_CA_BUNDLE"));
        assert!(allow.allows("anywhere", "RUST_LOG"));
        assert!(!allow.allows("anywhere", "HTTPS_PROXY"));
        assert!(!allow.allows("payments", "LD_PRELOAD"));
        assert!(ScopedNames::none().listing("payments").is_empty());
        assert_eq!(allow.listing("payments"), "HTTPS_PROXY, AWS_*, RUST_LOG");
        assert!(ScopedNames::env_names("*: *")
            .unwrap()
            .allows("x", "ANYTHING"));
    }
}
