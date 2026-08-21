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

/// The phrase every pin refusal carries, and the one thing that tells a
/// pin apart from a malformed value.
///
/// **It lives here because the messages do.** A refusal is a sentence a
/// pod's author reads, and the alternative to one shared phrase was
/// threading a second, machine-readable enum through sixty-six refusal
/// sites — or working the answer out somewhere else by reading the
/// English, which goes quietly wrong the first time a message is
/// reworded. A test walks every pin path and asserts the phrase survives.
pub(crate) const PINS: &str = "the installation pins";

/// The mark an injection leaves on the pod it patched.
///
/// **An injection has to be idempotent, and a mutating webhook is not
/// called once.** `reinvocationPolicy: IfNeeded` asks the API server to
/// call this webhook again whenever a *later* webhook changes the pod, and
/// some controllers resubmit a spec that has already been through
/// admission. Without a mark, the second pass adds the agent a second
/// time — two containers with one name, which the API server refuses, so
/// the pod never starts at all.
///
/// This is the vault-agent-injector's `agent-inject-status: injected`
/// under another name, and for the same reason.
pub const STATUS: &str = "status";

/// What [`STATUS`] is set to once a pod has been patched.
pub const INJECTED: &str = "injected";

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

/// A borrowed accessor into one tier's knobs — the type behind the
/// per-key pick tables.
type Pick<'a, T> = &'a dyn Fn(&KnobDefaults) -> Option<T>;

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
    /// The rendered file's location — fleet or per store.
    path: Option<String>,
    /// The store-shaped strings, per-store tiers only (the fleet's
    /// environment variables cannot set them): the address, the
    /// document, credentials' Secret names, auth flags, templates.
    endpoint: Option<String>,
    endpoint_secret: Option<String>,
    key: Option<String>,
    token_secret: Option<String>,
    password_secret: Option<String>,
    ca_configmap: Option<String>,
    tls_secret: Option<String>,
    ssh_secret: Option<String>,
    aws_secret: Option<String>,
    section: Option<String>,
    auth: Option<String>,
    auth_mount: Option<String>,
    auth_role: Option<String>,
    auth_username: Option<String>,
    auth_token_path: Option<String>,
    namespace: Option<String>,
    reference: Option<String>,
    api_url: Option<String>,
    template: Option<String>,
    template_configmap: Option<String>,
    /// This tier's own override mode (`overridable=false` inside a
    /// store's group): pins every value THIS tier sets, unless a
    /// per-value marker says otherwise.
    overridable: Option<bool>,
    /// Explicit `!`/`?` markers: `(knob key, overridable)`. A knob
    /// with no entry follows this tier's flag, then the installation's.
    rules: Vec<(String, bool)>,
}

/// The knob keys, spelled exactly as the annotations spell them — one
/// vocabulary, whether a value arrives per pod or per installation.
const KNOBS: &str = "agent-cpu-request, agent-memory-request, \
     agent-cpu-limit, agent-memory-limit, file-mode, watch-seconds, \
     mode, volume-medium, native-sidecar, agent-run-as-user, \
     agent-run-as-group, metrics-port, path, overridable — and, per \
     store, every \
     store-shaped annotation: endpoint, endpoint-secret, key, \
     token-secret, password-secret, ca-configmap, tls-secret, \
     ssh-secret, aws-secret, section, auth, auth-mount, auth-role, \
     auth-username, auth-token-path, namespace, ref, api-url, \
     template, template-configmap";

impl KnobDefaults {
    /// One `key=value`, validated exactly as the matching annotation
    /// would be. The context names where the value came from. A value
    /// ending in `!` is PINNED (a differing annotation is refused); one
    /// ending in `?` stays overridable even under `overridable:
    /// "false"`; unmarked values follow that flag.
    fn set(&mut self, context: &str, key: &str, value: &str) -> Result<(), String> {
        let (value, rule) = match value.strip_suffix('!') {
            Some(value) => (value.trim_end(), Some(false)),
            None => match value.strip_suffix('?') {
                Some(value) => (value.trim_end(), Some(true)),
                None => (value, None),
            },
        };

        if let Some(overridable) = rule {
            self.rules.push((key.to_owned(), overridable));
        }

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
            "path" => {
                if !value.starts_with('/') {
                    return Err(format!("{context}: path is {value:?}: an absolute path"));
                }

                self.path = Some(value.to_owned());
            }
            "overridable" => {
                self.overridable = Some(match value {
                    "true" => true,
                    "false" => false,
                    other => {
                        return Err(format!(
                            "{context}: overridable is {other:?}: \"true\" or \"false\""
                        ))
                    }
                });
            }
            "endpoint" | "endpoint-secret" | "key" | "token-secret" | "password-secret"
            | "ca-configmap" | "tls-secret" | "ssh-secret" | "aws-secret" | "section" | "auth"
            | "auth-mount" | "auth-role" | "auth-username" | "auth-token-path" | "namespace"
            | "ref" | "api-url" | "template" | "template-configmap" => {
                if value.is_empty() {
                    return Err(format!("{context}: {key} is empty"));
                }

                if matches!(key, "token-secret" | "password-secret" | "endpoint-secret")
                    && !value.contains('/')
                {
                    return Err(format!(
                        "{context}: {key} is {value:?}: the form is <secret-name>/<key>"
                    ));
                }

                let slot = match key {
                    "endpoint" => &mut self.endpoint,
                    "endpoint-secret" => &mut self.endpoint_secret,
                    "key" => &mut self.key,
                    "token-secret" => &mut self.token_secret,
                    "password-secret" => &mut self.password_secret,
                    "ca-configmap" => &mut self.ca_configmap,
                    "tls-secret" => &mut self.tls_secret,
                    "ssh-secret" => &mut self.ssh_secret,
                    "aws-secret" => &mut self.aws_secret,
                    "section" => &mut self.section,
                    "auth" => &mut self.auth,
                    "auth-mount" => &mut self.auth_mount,
                    "auth-role" => &mut self.auth_role,
                    "auth-username" => &mut self.auth_username,
                    "auth-token-path" => &mut self.auth_token_path,
                    "namespace" => &mut self.namespace,
                    "ref" => &mut self.reference,
                    "api-url" => &mut self.api_url,
                    "template" => &mut self.template,
                    _ => &mut self.template_configmap,
                };
                *slot = Some(value.to_owned());
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
    ("DYNAMIC_CONFIG_AGENT_PATH", "path"),
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
    /// A fleet default SOURCE: a pod may say only `key` and `path`,
    /// and the installation answers where they live. Covers the
    /// default render only — a named render is an explicit construct
    /// and keeps naming its own store.
    source: Option<String>,
    /// The source's own `!`/`?` marker, when one was given.
    source_rule: Option<bool>,
    /// The default override mode: `true` (the default) lets pod
    /// annotations override installation values; `false` pins every
    /// installation-SET value. Either way, per-value `!`/`?` markers
    /// win, and knobs the installation never set stay the pod's.
    overridable: bool,
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

        let overridable = match var("DYNAMIC_CONFIG_AGENT_DEFAULTS_OVERRIDABLE").as_deref() {
            None | Some("true") => true,
            Some("false") => false,
            Some(other) => {
                return Err(format!(
                    "DYNAMIC_CONFIG_AGENT_DEFAULTS_OVERRIDABLE is {other:?}: \
                     \"true\" or \"false\""
                ))
            }
        };

        let (source, source_rule) = match var("DYNAMIC_CONFIG_AGENT_SOURCE") {
            None => (None, None),
            Some(text) => {
                let (name, rule) = match text.strip_suffix('!') {
                    Some(name) => (name.trim_end(), Some(false)),
                    None => match text.strip_suffix('?') {
                        Some(name) => (name.trim_end(), Some(true)),
                        None => (text.as_str(), None),
                    },
                };

                if !SOURCES.contains(&name) {
                    return Err(format!(
                        "DYNAMIC_CONFIG_AGENT_SOURCE is {name:?}: not a store \
                         this contract knows"
                    ));
                }

                (Some(name.to_owned()), rule)
            }
        };

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
            source,
            source_rule,
            overridable,
            agent_env,
            agent_env_allow: scoped(
                "DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW",
                ScopedNames::env_names,
            )?,
            source_allow: scoped("DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW", ScopedNames::sources)?,
            source_deny: scoped("DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY", ScopedNames::sources)?,
        })
    }

    /// The installation as this process was given it: the environment,
    /// over the mounted document if there is one.
    ///
    /// **The environment wins.** A document is the installation written
    /// down once, in a values file or a ConfigMap; a variable is somebody
    /// standing in front of it for this deployment, which is the more
    /// specific statement of the two — the same rule the layers below
    /// follow.
    fn from_environment() -> Result<Self, String> {
        let mounted = crate::installation_file::read(&document_path())?;

        // **Set-and-empty is an answer, not an absence.** An empty
        // `..._AGENT_ENV_ALLOW` means *allow nothing*, which is exactly
        // what somebody reaches for to revoke what a document granted —
        // and treating it as unset handed the document back, inverting
        // the precedence this function exists to state. Only a variable
        // that is not set at all falls through.
        Self::from_lookup(&|name| {
            std::env::var(name)
                .ok()
                .or_else(|| mounted.get(name).cloned())
        })
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
        self.supplied(source, "", pick).map(|(value, _)| value)
    }

    /// The value the installation supplies for a knob, and whether it
    /// is PINNED against a differing annotation. The rule comes from
    /// the tier that supplied the value: its own `!`/`?` marker when
    /// one was given, the `overridable` flag otherwise.
    fn supplied<T: Clone>(
        &self,
        source: Option<&str>,
        key: &str,
        pick: impl Fn(&KnobDefaults) -> Option<T>,
    ) -> Option<(T, bool)> {
        let tier = |knobs: &KnobDefaults| {
            pick(knobs).map(|value| {
                // Per-value marker, then the tier's own overridable
                // flag, then the installation's — closest word wins.
                let overridable = knobs
                    .rules
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, overridable)| *overridable)
                    .or(knobs.overridable)
                    .unwrap_or(self.overridable);

                (value, !overridable)
            })
        };

        self.store(source)
            .and_then(tier)
            .or_else(|| tier(&self.fleet))
    }

    /// Every string-typed store-shaped default this installation
    /// supplies for a source, with each value's pin: the raw material
    /// for the annotation-else-default lookup and the conflict walk.
    fn store_strings(&self, source: Option<&str>) -> Vec<(&'static str, String, bool)> {
        let mut out: Vec<(&'static str, String, bool)> = Vec::new();

        let picks: &[(&'static str, Pick<String>)] = &[
            ("endpoint", &|k| k.endpoint.clone()),
            ("endpoint-secret", &|k| k.endpoint_secret.clone()),
            ("key", &|k| k.key.clone()),
            ("path", &|k| k.path.clone()),
            ("token-secret", &|k| k.token_secret.clone()),
            ("password-secret", &|k| k.password_secret.clone()),
            ("ca-configmap", &|k| k.ca_configmap.clone()),
            ("tls-secret", &|k| k.tls_secret.clone()),
            ("ssh-secret", &|k| k.ssh_secret.clone()),
            ("aws-secret", &|k| k.aws_secret.clone()),
            ("section", &|k| k.section.clone()),
            ("auth", &|k| k.auth.clone()),
            ("auth-mount", &|k| k.auth_mount.clone()),
            ("auth-role", &|k| k.auth_role.clone()),
            ("auth-username", &|k| k.auth_username.clone()),
            ("auth-token-path", &|k| k.auth_token_path.clone()),
            ("namespace", &|k| k.namespace.clone()),
            ("ref", &|k| k.reference.clone()),
            ("api-url", &|k| k.api_url.clone()),
            ("template", &|k| k.template.clone()),
            ("template-configmap", &|k| k.template_configmap.clone()),
        ];

        for (key, pick) in picks {
            if let Some((value, pinned)) = self.supplied(source, key, pick) {
                out.push((key, value, pinned));
            }
        }

        out
    }

    /// The fleet source and whether IT is pinned.
    fn supplied_source(&self) -> Option<(&str, bool)> {
        self.source
            .as_deref()
            .map(|source| (source, !self.source_rule.unwrap_or(self.overridable)))
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
/// Where a mounted installation document lives.
///
/// A path rather than a fixed mount so that a chart, a kustomize base and
/// a test can each put it where they want; absent means there is none,
/// which is an ordinary installation.
fn document_path() -> std::path::PathBuf {
    std::env::var("DYNAMIC_CONFIG_WEBHOOK_DEFAULTS_FILE")
        .unwrap_or_else(|_| "/etc/dynamic-config/installation.yaml".to_owned())
        .into()
}

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

/// Whether the pod already carries what an injection puts there.
///
/// The agent's container, under either of the two names it takes: the
/// sidecar's and the init container's. Checked rather than trusted,
/// because the mark that says "already injected" travels with a copied
/// manifest and the containers do not.
fn already_injected(pod: &Value) -> bool {
    let named = |list: Option<&Vec<Value>>| {
        list.is_some_and(|containers| {
            containers.iter().any(|container| {
                matches!(
                    container.pointer("/name").and_then(Value::as_str),
                    Some("dynamic-config-agent" | "dynamic-config-init")
                )
            })
        })
    };

    named(pod.pointer("/spec/containers").and_then(Value::as_array))
        || named(
            pod.pointer("/spec/initContainers")
                .and_then(Value::as_array),
        )
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

    // Before anything else, including the shape checks: a pod this
    // webhook has already patched is not a pod to patch again, and it is
    // not a pod to refuse either — the first pass already decided, and
    // repeating that decision is the whole of what this guard is for.
    match get(STATUS) {
        None => {}
        // The mark says a previous pass patched this pod. Believed only
        // when the patch is *there*: a manifest copied out of a cluster
        // and applied elsewhere — `kubectl get pod -o yaml | kubectl apply
        // -f -` — carries the mark without the containers, and this guard
        // was the only thing between that pod and running with no
        // configuration at all, silently.
        Some(INJECTED) if already_injected(pod) => return Ok(None),
        Some(INJECTED) => {
            return Err(format!(
                "{PREFIX}{STATUS} is {INJECTED:?} but this pod carries no                  injected container: the mark is this webhook's to write,                  and a pod copied from another cluster keeps the mark                  without what it stood for"
            ))
        }
        Some(other) => {
            return Err(format!(
                "{PREFIX}{STATUS} is {other:?}, and it is not a pod's to set: \
                 this webhook writes it as {INJECTED:?} on a pod it has \
                 patched, so that a second admission does not patch it again"
            ))
        }
    }

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
        // Written by this webhook, never by a pod's author — a value
        // other than `injected` is refused below rather than ignored.
        STATUS,
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

    // Every value below resolves annotation → this store's installation
    // default → the fleet's → the built-in. Pod-wide knobs (mode,
    // volume, resources, identity) take the DEFAULT render's store.
    // A pinned installation value (`!`, or `overridable: "false"`)
    // refuses a DIFFERING annotation — never silently outvotes it,
    // because a value the author wrote and did not get is a debugging
    // session; the SAME value restated passes.
    let pinned = |key: &str, pod: &str, tier: &str| -> String {
        format!(
            "{PREFIX}{key} is {pod:?}, but the installation pins it to \
             {tier:?} — match it or drop the annotation (the installer \
             opens the value with a \"?\" marker, or everything with \
             overridable \"true\")"
        )
    };

    // The pod's source, else the fleet's; a pinned fleet source
    // refuses a pod that names a different one.
    if let (Some(pod_source), Some((fleet_source, true))) =
        (get("source"), install.supplied_source())
    {
        if pod_source != fleet_source {
            return Err(pinned("source", pod_source, fleet_source));
        }
    }

    let source_name = get("source").or_else(|| install.supplied_source().map(|(s, _)| s));

    // The store-shaped strings this installation supplies for that
    // source, resolved once: the lookup below reads annotation first,
    // these second — and the walk here refuses a pinned conflict.
    let supplied_strings = install.store_strings(source_name);

    for (key, tier, is_pinned) in &supplied_strings {
        if !is_pinned {
            continue;
        }

        if let Some(pod_value) = get(key) {
            if pod_value != tier {
                return Err(pinned(key, pod_value, tier));
            }
        }
    }

    let defaulted = |name: &str| -> Option<&str> {
        get(name).or_else(|| {
            supplied_strings
                .iter()
                .find(|(key, _, _)| *key == name)
                .map(|(_, value, _)| value.as_str())
        })
    };

    // The two either-or pairs resolve as a LEVEL, so a pinned half must
    // also refuse the pod answering with the OTHER half — otherwise a
    // pinned endpoint is sidestepped by an endpoint-secret.
    for (a, b) in [
        ("endpoint", "endpoint-secret"),
        ("endpoint-secret", "endpoint"),
        ("template", "template-configmap"),
        ("template-configmap", "template"),
    ] {
        let a_pinned = supplied_strings
            .iter()
            .any(|(key, _, is_pinned)| *key == a && *is_pinned);

        if a_pinned && get(a).is_none() && get(b).is_some() {
            return Err(format!(
                "{PREFIX}{b} is set, but the installation pins {PREFIX}{a} — \
                 the pair answers as one, and this half is not the pod's \
                 to choose"
            ));
        }
    }

    let mode = match get("mode") {
        Some(text) => {
            let mode = match text {
                "init" => Mode::Init,
                "sidecar" => Mode::Sidecar,
                "both" => Mode::Both,
                other => return Err(format!("{PREFIX}mode is {other:?}: init, sidecar or both")),
            };

            if let Some((tier, true)) = install.supplied(source_name, "mode", |k| k.mode) {
                if tier != mode {
                    let name = |m: Mode| match m {
                        Mode::Init => "init",
                        Mode::Sidecar => "sidecar",
                        Mode::Both => "both",
                    };

                    return Err(pinned("mode", name(mode), name(tier)));
                }
            }

            mode
        }
        None => install
            .knob(source_name, |k| k.mode)
            .unwrap_or(Mode::Sidecar),
    };

    let watch_seconds = match get("watch-seconds") {
        None => install.knob(source_name, |k| k.watch_seconds).unwrap_or(15),
        Some(text) => {
            let seconds: u64 = text
                .parse()
                .map_err(|_| format!("{PREFIX}watch-seconds is {text:?}: whole seconds"))?;

            if let Some((tier, true)) =
                install.supplied(source_name, "watch-seconds", |k| k.watch_seconds)
            {
                if tier != seconds {
                    return Err(pinned("watch-seconds", text, &tier.to_string()));
                }
            }

            seconds
        }
    };

    // "name/key", split once; the slash the form needs is the first one.
    let secret_ref = |text: Option<&str>, name: &str| -> Result<Option<SecretRef>, String> {
        match text {
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
        defaulted(name).map(|text| match text.split_once('/') {
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

    // The address pair resolves as a LEVEL: any pod-side answer mutes
    // both installation defaults — a pod that chose endpoint-secret
    // must not also inherit the store's plain endpoint.
    let (endpoint_text, endpoint_secret_text) =
        if get("endpoint").is_some() || get("endpoint-secret").is_some() {
            (get("endpoint"), get("endpoint-secret"))
        } else {
            (defaulted("endpoint"), defaulted("endpoint-secret"))
        };

    let mut secret_env = Vec::new();

    for (text, annotation, variable) in [
        (
            defaulted("token-secret"),
            "token-secret",
            "DYNAMIC_CONFIG_AGENT_TOKEN",
        ),
        (
            defaulted("password-secret"),
            "password-secret",
            "DYNAMIC_CONFIG_AGENT_PASSWORD",
        ),
        (
            endpoint_secret_text,
            "endpoint-secret",
            "DYNAMIC_CONFIG_AGENT_ENDPOINT",
        ),
    ] {
        if let Some(reference) = secret_ref(text, annotation)? {
            secret_env.push((variable.to_owned(), reference));
        }
    }

    let endpoint = endpoint_text.map(str::to_owned);
    let endpoint_from_secret = endpoint_secret_text.is_some();

    if endpoint.is_none() && !endpoint_from_secret {
        return Err(format!(
            "{PREFIX}inject is true, so {PREFIX}endpoint is required \
             (or {PREFIX}endpoint-secret when the address carries a \
             password, or a per-store default endpoint)"
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
        if let Some(value) = defaulted(annotation) {
            arguments.push((flag.to_owned(), value.to_owned()));
        }
    }

    if let Some(ssh) = &ssh {
        match defaulted("auth") {
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
        Some(text) => {
            let memory = match text {
                "memory" => true,
                "disk" => false,
                other => {
                    return Err(format!(
                        "{PREFIX}volume-medium is {other:?}: memory (default) or disk"
                    ))
                }
            };

            if let Some((tier, true)) =
                install.supplied(source_name, "volume-medium", |k| k.volume_memory)
            {
                if tier != memory {
                    return Err(pinned(
                        "volume-medium",
                        text,
                        if tier { "memory" } else { "disk" },
                    ));
                }
            }

            memory
        }
        None => install
            .knob(source_name, |k| k.volume_memory)
            .unwrap_or(true),
    };

    let native_sidecar = match get("native-sidecar") {
        Some(text) => {
            let native = match text {
                "true" => true,
                "false" => false,
                other => {
                    return Err(format!(
                        "{PREFIX}native-sidecar is {other:?}: \"true\" or \"false\""
                    ))
                }
            };

            if let Some((tier, true)) =
                install.supplied(source_name, "native-sidecar", |k| k.native_sidecar)
            {
                if tier != native {
                    return Err(pinned(
                        "native-sidecar",
                        text,
                        if tier { "true" } else { "false" },
                    ));
                }
            }

            native
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

    // The quantity knobs share one pin walk: a pinned tier value
    // refuses a differing annotation before the fallback resolution.
    for (key, pick) in [
        (
            "agent-cpu-request",
            &(|k: &KnobDefaults| k.cpu_request.clone()) as Pick<String>,
        ),
        ("agent-memory-request", &|k: &KnobDefaults| {
            k.memory_request.clone()
        }),
        ("agent-cpu-limit", &|k: &KnobDefaults| k.cpu_limit.clone()),
        ("agent-memory-limit", &|k: &KnobDefaults| {
            k.memory_limit.clone()
        }),
    ] {
        if let (Some(pod_value), Some((tier, true))) =
            (get(key), install.supplied(source_name, key, pick))
        {
            if pod_value != tier {
                return Err(pinned(key, pod_value, &tier));
            }
        }
    }

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
        Some(text) => {
            let mode = octal_mode(&format!("{PREFIX}file-mode"), text)?;

            if let Some((tier, true)) =
                install.supplied(source_name, "file-mode", |k| k.file_mode.clone())
            {
                if tier != mode {
                    return Err(pinned("file-mode", text, &format!("0{tier}")));
                }
            }

            Some(mode)
        }
    };

    let identity = |name: &str| -> Result<Option<u32>, String> {
        match get(name) {
            None => Ok(None),
            Some(text) => Ok(Some(nonroot_id(&format!("{PREFIX}{name}"), text)?)),
        }
    };

    for (key, pick) in [
        (
            "agent-run-as-user",
            &(|k: &KnobDefaults| k.run_as_user) as Pick<u32>,
        ),
        ("agent-run-as-group", &|k: &KnobDefaults| k.run_as_group),
    ] {
        if let (Some(pod_value), Some((tier, true))) =
            (identity(key)?, install.supplied(source_name, key, pick))
        {
            if pod_value != tier {
                return Err(pinned(key, &pod_value.to_string(), &tier.to_string()));
            }
        }
    }

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

    let aws_secret = defaulted("aws-secret").map(str::to_owned);

    if aws_secret.is_some() && source_name != Some("s3") {
        return Err(format!(
            "{PREFIX}aws-secret is the s3 source's flag; the source here is {:?}",
            source_name.unwrap_or("unset")
        ));
    }

    let metrics_port = match get("metrics-port") {
        None => install.knob(source_name, |k| k.metrics_port),
        Some(text) => {
            let port: u16 = text
                .parse()
                .map_err(|_| format!("{PREFIX}metrics-port is {text:?}: a port number"))?;

            if let Some((tier, true)) =
                install.supplied(source_name, "metrics-port", |k| k.metrics_port)
            {
                if tier != port {
                    return Err(pinned("metrics-port", text, &tier.to_string()));
                }
            }

            // "0" is the per-pod opt-OUT of an (unpinned) default.
            if port == 0 {
                None
            } else {
                Some(port)
            }
        }
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
    let tls = defaulted("tls-secret").map(str::to_owned);

    // Output templating: a one-liner inline, or a ConfigMap for
    // anything worth reviewing. One template, one place.
    // The template pair resolves as a LEVEL too, like the address.
    let (template_inline, template_configmap_text) =
        if get("template").is_some() || get("template-configmap").is_some() {
            (get("template"), get("template-configmap"))
        } else {
            (defaulted("template"), defaulted("template-configmap"))
        };

    let template = template_configmap_text.map(|text| match text.split_once('/') {
        Some((object, key)) => ObjectRef {
            name: object.to_owned(),
            key: key.to_owned(),
        },
        None => ObjectRef {
            name: text.to_owned(),
            key: "template".to_owned(),
        },
    });

    match (template_inline, &template) {
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

    let resolved_path = defaulted("path");

    for suffix in &extra_names {
        extra.push(extra_render(
            suffix,
            &|name| get(&format!("{name}.{suffix}")),
            pod,
            resolved_path,
            install,
        )?);
    }

    let resolved = |name: &str| {
        defaulted(name).map(str::to_owned).ok_or_else(|| {
            format!(
                "{PREFIX}inject is true, so {PREFIX}{name} is required \
                 (per annotation, or as an installation default)"
            )
        })
    };

    let path = resolved("path")?;

    Ok(Some(Request {
        source: source_name.map(str::to_owned).ok_or_else(|| {
            format!(
                "{PREFIX}inject is true, so {PREFIX}source is required \
                 (per annotation, or DYNAMIC_CONFIG_AGENT_SOURCE — the \
                 chart's agent.defaults.source)"
            )
        })?,
        endpoint,
        key: resolved("key")?,
        path: path.clone(),
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

    // The same tiers the default render gets, against THIS render's
    // store — and the same pin walk, refusing suffixed conflicts.
    let supplied_strings = install.store_strings(Some(&source));

    let pinned = |key: &str, pod: &str, tier: &str| -> String {
        format!(
            "{} is {pod:?}, but the installation pins it to {tier:?} — \
             match it or drop the annotation",
            label(key)
        )
    };

    for (key, tier, is_pinned) in &supplied_strings {
        if !is_pinned || *key == "path" {
            continue;
        }

        if let Some(pod_value) = get(key) {
            if pod_value != tier {
                return Err(pinned(key, pod_value, tier));
            }
        }
    }

    let defaulted = |name: &str| -> Option<&str> {
        get(name).or_else(|| {
            supplied_strings
                .iter()
                .find(|(key, _, _)| *key == name)
                .map(|(_, value, _)| value.as_str())
        })
    };

    for (a, b) in [
        ("endpoint", "endpoint-secret"),
        ("endpoint-secret", "endpoint"),
        ("template", "template-configmap"),
        ("template-configmap", "template"),
    ] {
        let a_pinned = supplied_strings
            .iter()
            .any(|(key, _, is_pinned)| *key == a && *is_pinned);

        if a_pinned && get(a).is_none() && get(b).is_some() {
            return Err(format!(
                "{} is set, but the installation pins {} — the pair \
                 answers as one, and this half is not the pod's to choose",
                label(b),
                label(a)
            ));
        }
    }

    let key = defaulted("key").map(str::to_owned).ok_or_else(|| {
        format!(
            "render {suffix:?} exists, so {} is required (per annotation, \
             or as a per-store default)",
            label("key")
        )
    })?;
    // A named render's PATH stays its own: two renders sharing one
    // defaulted path would fight over one file, so no tier answers
    // for it here.
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
        Some(text) => {
            let seconds: u64 = text
                .parse()
                .map_err(|_| format!("{} is {text:?}: whole seconds", label("watch-seconds")))?;

            if let Some((tier, true)) =
                install.supplied(Some(&source), "watch-seconds", |k| k.watch_seconds)
            {
                if tier != seconds {
                    return Err(pinned("watch-seconds", text, &tier.to_string()));
                }
            }

            seconds
        }
    };

    let secret_ref = |text: Option<&str>, name: &str| -> Result<Option<SecretRef>, String> {
        match text {
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
        defaulted(name).map(|text| match text.split_once('/') {
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

    // The address pair resolves as a LEVEL, exactly as on the default
    // render: a pod-side answer mutes both installation defaults.
    let (endpoint_text, endpoint_secret_text) =
        if get("endpoint").is_some() || get("endpoint-secret").is_some() {
            (get("endpoint"), get("endpoint-secret"))
        } else {
            (defaulted("endpoint"), defaulted("endpoint-secret"))
        };

    let mut secret_env = Vec::new();

    for (text, annotation, variable) in [
        (
            defaulted("token-secret"),
            "token-secret",
            "DYNAMIC_CONFIG_AGENT_TOKEN",
        ),
        (
            defaulted("password-secret"),
            "password-secret",
            "DYNAMIC_CONFIG_AGENT_PASSWORD",
        ),
        (
            endpoint_secret_text,
            "endpoint-secret",
            "DYNAMIC_CONFIG_AGENT_ENDPOINT",
        ),
    ] {
        if let Some(reference) = secret_ref(text, annotation)? {
            secret_env.push((variable.to_owned(), reference));
        }
    }

    let endpoint = endpoint_text.map(str::to_owned);
    let endpoint_from_secret = endpoint_secret_text.is_some();

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
        if let Some(value) = defaulted(annotation) {
            arguments.push((flag.to_owned(), value.to_owned()));
        }
    }

    if let Some(ssh) = &ssh {
        match defaulted("auth") {
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
        Some(text) => {
            let mode = octal_mode(&label("file-mode"), text)?;

            if let Some((tier, true)) =
                install.supplied(Some(&source), "file-mode", |k| k.file_mode.clone())
            {
                if tier != mode {
                    return Err(pinned("file-mode", text, &format!("0{tier}")));
                }
            }

            Some(mode)
        }
    };

    if let Some(mode) = file_mode {
        arguments.push(("--file-mode".to_owned(), format!("0{mode}")));
    }

    let tls = defaulted("tls-secret").map(str::to_owned);

    let (template_inline, template_configmap_text) =
        if get("template").is_some() || get("template-configmap").is_some() {
            (get("template"), get("template-configmap"))
        } else {
            (defaulted("template"), defaulted("template-configmap"))
        };

    let template = template_configmap_text.map(|text| match text.split_once('/') {
        Some((object, key)) => ObjectRef {
            name: object.to_owned(),
            key: key.to_owned(),
        },
        None => ObjectRef {
            name: text.to_owned(),
            key: "template".to_owned(),
        },
    });

    match (template_inline, &template) {
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
