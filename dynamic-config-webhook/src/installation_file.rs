//! The installation, written as YAML instead of as a grammar.
//!
//! Everything an installation sets reaches this webhook as a *string*,
//! because that is what an environment variable is — and several of those
//! strings are little grammars:
//!
//! ```text
//! DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS="vault: overridable=false, endpoint=https://vault:8200; s3: file-mode=0640?"
//! DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW="payments: vault, s3; *: consul"
//! ```
//!
//! Fine to parse and unpleasant to write, especially in a values file
//! where the reader has YAML already. So an installation may hand the
//! same thing over as a mounted document:
//!
//! ```yaml
//! storeDefaults:
//!   vault:
//!     overridable: false
//!     endpoint: https://vault:8200
//!   s3:
//!     file-mode: 0640?
//! sourceAllow:
//!   payments: [vault, s3]
//!   "*": [consul]
//! ```
//!
//! **The document is defined as what it renders to.** Every form here
//! turns into the grammar above and goes through the same parser, with
//! the same validation and the same error messages — so there is one set
//! of rules, one set of tests, and a structured installation cannot mean
//! something a written-out one could not.
//!
//! That indirection is also what makes this work for kustomize. A chart
//! could have rendered the strings in a template; a kustomize base has no
//! template engine, and a hand-written ConfigMap of YAML is the only
//! structured thing it can hand over.

use std::collections::BTreeMap;

use dynamic_config::Value;

/// The document's keys, and the variable each renders into.
///
/// Named as a values file names them rather than as the environment
/// does: somebody writing this has just written `agent.defaults.mode`
/// three lines above.
const SCALARS: &[(&str, &str)] = &[
    ("cpuRequest", "DYNAMIC_CONFIG_AGENT_CPU_REQUEST"),
    ("memoryRequest", "DYNAMIC_CONFIG_AGENT_MEMORY_REQUEST"),
    ("cpuLimit", "DYNAMIC_CONFIG_AGENT_CPU_LIMIT"),
    ("memoryLimit", "DYNAMIC_CONFIG_AGENT_MEMORY_LIMIT"),
    ("fileMode", "DYNAMIC_CONFIG_AGENT_FILE_MODE"),
    ("watchSeconds", "DYNAMIC_CONFIG_AGENT_WATCH_SECONDS"),
    ("mode", "DYNAMIC_CONFIG_AGENT_MODE"),
    ("nativeSidecar", "DYNAMIC_CONFIG_AGENT_NATIVE_SIDECAR"),
    ("volumeMedium", "DYNAMIC_CONFIG_AGENT_VOLUME_MEDIUM"),
    ("runAsUser", "DYNAMIC_CONFIG_AGENT_RUN_AS_USER"),
    ("runAsGroup", "DYNAMIC_CONFIG_AGENT_RUN_AS_GROUP"),
    ("metricsPort", "DYNAMIC_CONFIG_AGENT_METRICS_PORT"),
    ("source", "DYNAMIC_CONFIG_AGENT_SOURCE"),
    ("endpoint", "DYNAMIC_CONFIG_AGENT_ENDPOINT"),
    ("path", "DYNAMIC_CONFIG_AGENT_PATH"),
    ("agentEnv", "DYNAMIC_CONFIG_AGENT_ENV"),
    ("overridable", "DYNAMIC_CONFIG_AGENT_DEFAULTS_OVERRIDABLE"),
];

/// The `store: knob=value, …` shape.
const PER_STORE: (&str, &str) = ("storeDefaults", "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS");

/// The `namespace: name, …` shape.
const LISTS: &[(&str, &str)] = &[
    ("agentEnvAllow", "DYNAMIC_CONFIG_WEBHOOK_AGENT_ENV_ALLOW"),
    ("sourceAllow", "DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW"),
    ("sourceDeny", "DYNAMIC_CONFIG_WEBHOOK_SOURCE_DENY"),
];

/// Reads `path` and renders it into `variable → value`.
///
/// An absent file is an empty map rather than an error: the mount is
/// optional, and an installation that sets nothing is a legal
/// installation.
///
/// # Errors
///
/// If the file cannot be read, is not a document this crate parses, or
/// carries a key that is not one of the above — an unknown key is a typo,
/// and a typo silently ignored is a default that never applied.
pub fn read(path: &std::path::Path) -> Result<BTreeMap<String, String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };

    // **A document that says nothing is not a malformed document.** The
    // shipped ConfigMap is a page of commented-out examples, which is what
    // an installation with no defaults looks like — and YAML reads that as
    // *null*, which `render` rightly refuses as "not a map of settings".
    // The webhook then refused to start, so the kustomize path could not be
    // applied at all until somebody uncommented something.
    if text
        .lines()
        .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
    {
        return Ok(BTreeMap::new());
    }

    let format = dynamic_config::Format::from_path(path)
        .map_err(|_| format!("{}: the name has no format this reads", path.display()))?;
    let document = Value::parse(&text, format).map_err(|error| {
        // The parser's own message names a position and never the text,
        // which is the rule this whole project keeps: an installation
        // document carries endpoints and role names.
        format!("{}: {error}", path.display())
    })?;

    render(&document).map_err(|error| format!("{}: {error}", path.display()))
}

/// The document, as the variables it stands for.
fn render(document: &Value) -> Result<BTreeMap<String, String>, String> {
    let Value::Table(top) = document else {
        return Err("the installation document is a map of settings".to_owned());
    };

    let mut rendered = BTreeMap::new();

    for (key, value) in top {
        if let Some((_, variable)) = SCALARS.iter().find(|(name, _)| name == key) {
            rendered.insert((*variable).to_owned(), scalar(value)?);
            continue;
        }

        if key == PER_STORE.0 {
            rendered.insert(PER_STORE.1.to_owned(), per_store(value)?);
            continue;
        }

        if let Some((_, variable)) = LISTS.iter().find(|(name, _)| name == key) {
            rendered.insert((*variable).to_owned(), lists(value)?);
            continue;
        }

        return Err(format!(
            "{key:?} is not an installation setting; the ones there are: {}",
            names().join(", ")
        ));
    }

    Ok(rendered)
}

/// Every key this document accepts, for the message above.
fn names() -> Vec<&'static str> {
    SCALARS
        .iter()
        .map(|(name, _)| *name)
        .chain(std::iter::once(PER_STORE.0))
        .chain(LISTS.iter().map(|(name, _)| *name))
        .collect()
}

/// A leaf, as the string the variable would have carried.
///
/// A number and a boolean are written the way YAML writes them —
/// `watchSeconds: 30`, `nativeSidecar: true` — rather than quoted, which
/// is the whole point of writing a document instead of a string.
fn scalar(value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Integer(number) => Ok(number.to_string()),
        Value::Bool(boolean) => Ok(boolean.to_string()),
        other => Err(format!(
            "a setting is a word, a number or a boolean; this one is {}",
            kind(other)
        )),
    }
}

/// `{store: {knob: value}}` → `"store: knob=value, …; …"`.
fn per_store(value: &Value) -> Result<String, String> {
    let Value::Table(stores) = value else {
        return Err(format!(
            "{}: a map of store to its settings; this is {}",
            PER_STORE.0,
            kind(value)
        ));
    };

    let mut groups = Vec::new();

    for (store, knobs) in stores {
        // A store's settings may be written either way, which is what the
        // chart's values reference promises: a map, because a values file
        // has YAML already, or the same grammar as one string, because an
        // environment variable can carry that and a map cannot. Refusing
        // the string here split one setting across two mechanisms — the
        // map half travelled as a document, the string half as a variable,
        // and a variable replaces a document wholesale, so a file with one
        // of each silently lost everything the map had said.
        if let Value::String(written) = knobs {
            groups.push(format!("{store}: {written}"));

            continue;
        }

        let Value::Table(knobs) = knobs else {
            return Err(format!(
                "{}.{store}: a map of setting to value, or that grammar as one \
                 string; this is {}",
                PER_STORE.0,
                kind(knobs)
            ));
        };

        let pairs: Result<Vec<String>, String> = knobs
            .iter()
            .map(|(knob, value)| Ok(format!("{knob}={}", scalar(value)?)))
            .collect();

        groups.push(format!("{store}: {}", pairs?.join(", ")));
    }

    Ok(groups.join("; "))
}

/// `{namespace: [name, …]}` → `"namespace: name, …; …"`.
fn lists(value: &Value) -> Result<String, String> {
    let Value::Table(namespaces) = value else {
        return Err(format!(
            "a map of namespace to a list of names; this is {}",
            kind(value)
        ));
    };

    let mut groups = Vec::new();

    for (namespace, names) in namespaces {
        let Value::Array(names) = names else {
            return Err(format!(
                "{namespace}: a list of names; this is {}",
                kind(names)
            ));
        };

        let names: Result<Vec<String>, String> = names.iter().map(scalar).collect();

        groups.push(format!("{namespace}: {}", names?.join(", ")));
    }

    Ok(groups.join("; "))
}

/// What a value is, without saying what is in it.
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing",
        Value::Bool(_) => "a boolean",
        Value::Integer(_) | Value::Float(_) => "a number",
        Value::String(_) => "a word",
        Value::Array(_) => "a list",
        Value::Table(_) => "a map",
    }
}
