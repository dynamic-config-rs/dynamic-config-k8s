//! Fetch, resolve, emit, write — the agent's whole pipeline after the
//! source is built.
//!
//! The emitted document is the *resolved* one: the engine parsed and
//! merged it exactly as an in-process consumer would, so what lands on
//! disk is what the application will read back — one resolution, not
//! two dialects of one.
//!
//! The flat emitters (`.ini`, `.properties`) are this binary's own, and
//! that is a stated boundary rather than an accident: the engine's
//! `save` refuses those formats because its contract is a typed round
//! trip, and a rendered file for a consumer is not one. Flattening rules
//! here: nested tables become dotted keys (properties) or sections
//! (INI); arrays are refused with an error naming the path, because
//! neither format has them and inventing an encoding would be a dialect
//! of one.

use std::path::Path;

use dynamic_config::{load, Format, LoadSpec, Source};

use crate::spec::Spec;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Json,
    Toml,
    Yaml,
    Ini,
    Properties,
}

impl OutputFormat {
    pub fn of(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "json" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            "ini" => Some(Self::Ini),
            "properties" => Some(Self::Properties),
            _ => None,
        }
    }
}

pub fn render_fetched(
    fetched: &dynamic_config::Fetched,
    spec: &Spec,
) -> Result<String, Box<dyn std::error::Error>> {
    let document = resolve(&fetched.text, fetched.format, spec.section.as_deref())?;

    // Before the template and before the write: a document that does not
    // satisfy the schema must not reach the application, and the caller
    // keeps serving the last good file instead. The engine's own validation
    // is a *typed* one and belongs to Rust, Python and Node; this is the
    // same guarantee for the consumers that are none of those — a Java
    // service reading `.properties`, a daemon reading YAML.
    if let Some(path) = &spec.schema {
        validate(&document, path)?;
    }

    // A template owns the bytes. The file variant is re-read at every
    // render on purpose: it is a mounted ConfigMap, and an edit there
    // should take on the next tick, not the next rollout.
    if let Some(path) = &spec.template {
        return templated(&std::fs::read_to_string(path)?, &document);
    }

    if let Some(text) = &spec.template_inline {
        return templated(text, &document);
    }

    render(
        &document,
        OutputFormat::of(&spec.out).expect("validated in Spec"),
    )
}

/// The resolved document through a minijinja template.
///
/// Undefined is STRICT: `{{ db.hots }}` is an error, not an empty
/// string — a typo that silently renders nothing would ship a broken
/// file with a clean exit code. At startup that error is fatal; during
/// a watch, the loop's keep-last-good covers it like any fetch failure.
fn templated(
    text: &str,
    document: &serde_json::Value,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut environment = minijinja::Environment::new();
    environment.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    // A file template's trailing newline is the author's, not ours to trim.
    environment.set_keep_trailing_newline(true);
    // Jinja's Python heritage prints `True`; every format this agent
    // renders spells booleans `true`, and a template should too.
    environment.set_formatter(|out, state, value| {
        if value.kind() == minijinja::value::ValueKind::Bool {
            write!(out, "{}", if value.is_true() { "true" } else { "false" })?;

            Ok(())
        } else {
            minijinja::escape_formatter(out, state, value)
        }
    });
    register_filters(&mut environment);
    environment.add_template("out", text)?;

    let rendered = environment
        .get_template("out")?
        .render(minijinja::Value::from_serialize(document))?;

    Ok(rendered)
}

/// Through the engine: the same parse, the same section semantics.
pub fn resolve(
    text: &str,
    format: Format,
    section: Option<&str>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let sources = [Source::inline(text, format)];

    let mut spec = LoadSpec::new(section.unwrap_or("document"), &sources);

    if section.is_none() {
        // No section named: the fetched document IS the configuration.
        spec = spec.with_whole_document(true);
    }

    Ok(load::<serde_json::Value>(&spec)?)
}

pub fn render(
    document: &serde_json::Value,
    format: OutputFormat,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(match format {
        OutputFormat::Json => {
            let mut rendered = serde_json::to_string_pretty(document)?;
            rendered.push('\n');
            rendered
        }
        OutputFormat::Toml => toml::to_string_pretty(document)?,
        OutputFormat::Yaml => serde_yaml::to_string(document)?,
        OutputFormat::Ini => flat(document, Flat::Ini)?,
        OutputFormat::Properties => flat(document, Flat::Properties)?,
    })
}

enum Flat {
    Ini,
    Properties,
}

/// The flat emitters. Scalars render bare; a string that would widen on
/// the way back in (looks like a bool or a number) is double-quoted in
/// INI — the round half of the engine's widening rule — and properties
/// escapes its separators.
fn flat(document: &serde_json::Value, dialect: Flat) -> Result<String, Box<dyn std::error::Error>> {
    let mut lines = Vec::new();

    match dialect {
        Flat::Properties => {
            walk(document, &mut Vec::new(), &mut |path, value| {
                lines.push(format!("{} = {}", path.join("."), properties_scalar(value)));
                Ok(())
            })?;
        }
        Flat::Ini => {
            // Root scalars first, then one [section] per top-level table.
            let Some(map) = document.as_object() else {
                return Err("an INI document is a table at the top".into());
            };

            for (key, value) in map {
                if !value.is_object() {
                    lines.push(format!("{key} = {}", ini_scalar(value)?));
                }
            }

            for (key, value) in map {
                if value.is_object() {
                    lines.push(format!("\n[{key}]"));
                    walk(value, &mut Vec::new(), &mut |path, scalar| {
                        // [a] with nested b.c: INI nests through the
                        // section header's dots, so deeper tables become
                        // [a.b] — handled by re-walking below — and here
                        // only one level of keys lands per section.
                        lines.push(format!("{} = {}", path.join("."), ini_scalar(scalar)?));
                        Ok(())
                    })?;
                }
            }
        }
    }

    let mut out = lines.join("\n");
    out.push('\n');

    Ok(out)
}

/// The resolved document as environment entries: dotted paths become
/// `UPPER_SNAKE` names (`db.pool_size` → `DB_POOL_SIZE`), scalars render
/// bare, and a list or table value becomes compact JSON — an env value
/// is a string, and pretending otherwise would invent a dialect.
///
/// What the operator's `envEntries` Secret shape and an `envFrom`
/// consumer agree on.
pub fn env_entries(
    document: &serde_json::Value,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let mut entries = Vec::new();

    walk_env(document, &mut Vec::new(), &mut |path, value| {
        let name: String = path
            .join("_")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();

        let rendered = match value {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        };

        entries.push((name, rendered));
        Ok(())
    })?;

    Ok(entries)
}

/// Every leaf verbatim: dotted paths exactly as the document spells
/// them (`auth.postgres-password` stays `auth.postgres-password`) —
/// for Secret keys some OTHER chart already named, where any mangling
/// breaks the contract. Kubernetes allows `[-._a-zA-Z0-9]` in data
/// keys, which dotted paths satisfy.
pub fn verbatim_entries(
    document: &serde_json::Value,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let mut entries = Vec::new();

    walk_env(document, &mut Vec::new(), &mut |path, value| {
        let rendered = match value {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        };

        entries.push((path.join("."), rendered));
        Ok(())
    })?;

    Ok(entries)
}

/// `walk`, with one difference for the env dialect: an array is a leaf
/// (compact JSON), because an env value is a string and the flat
/// formats' "no arrays" refusal would make whole documents unmappable.
fn walk_env(
    value: &serde_json::Value,
    path: &mut Vec<String>,
    emit: &mut impl FnMut(&[String], &serde_json::Value) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                path.push(key.clone());
                walk_env(inner, path, emit)?;
                path.pop();
            }

            Ok(())
        }
        leaf => emit(path, leaf),
    }
}

fn walk(
    value: &serde_json::Value,
    path: &mut Vec<String>,
    emit: &mut impl FnMut(&[String], &serde_json::Value) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                path.push(key.clone());
                walk(inner, path, emit)?;
                path.pop();
            }

            Ok(())
        }
        serde_json::Value::Array(_) => Err(format!(
            "`{}` is an array, and neither flat format has one; render to \
             json, toml or yaml instead",
            path.join(".")
        )
        .into()),
        scalar => emit(path, scalar),
    }
}

fn properties_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text
            .replace('\\', "\\\\")
            .replace('=', "\\=")
            .replace(':', "\\:")
            .replace('\n', "\\n"),
        other => other.to_string(),
    }
}

fn ini_scalar(value: &serde_json::Value) -> Result<String, Box<dyn std::error::Error>> {
    Ok(match value {
        serde_json::Value::String(text) => {
            let widens = text.parse::<bool>().is_ok()
                || text.parse::<i64>().is_ok()
                || text.parse::<f64>().is_ok();

            if widens || text.contains('\n') {
                format!("\"{}\"", text.replace('"', "'"))
            } else {
                text.clone()
            }
        }
        other => other.to_string(),
    })
}

/// The filters a configuration template needs and minijinja does not ship.
///
/// Six, and the list is short on purpose. Every one of them is a *pure
/// function of the document already in hand* — which is the line this
/// crate does not cross: a template cannot read a file, reach a store or
/// see an environment variable, so the pipeline stays
/// fetch → resolve → validate → render and a rendered document is a
/// function of what was fetched. Consul Template's `secret()` is exactly
/// the feature not being copied.
///
/// `indent`, `default`, `join` and `string` are minijinja's own and are
/// not repeated here.
fn register_filters(environment: &mut minijinja::Environment<'_>) {
    // The one every Kubernetes template needs: a Secret's `data` is
    // base64, so a template writing one has to encode.
    environment.add_filter("b64encode", |value: &str| base64(value.as_bytes()));
    environment.add_filter(
        "b64decode",
        |value: &str| -> Result<String, minijinja::Error> {
            let bytes = unbase64(value).ok_or_else(|| {
                minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "b64decode: not base64",
                )
            })?;

            String::from_utf8(bytes).map_err(|_| {
                minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "b64decode: not UTF-8",
                )
            })
        },
    );

    // minijinja's `tojson` is behind a feature this build does not enable,
    // and a configuration template that cannot emit JSON is missing the
    // format half its consumers read.
    environment.add_filter("json", |value: minijinja::Value| {
        serde_json::to_string(&value).map_err(|error| {
            minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                format!("json: {error}"),
            )
        })
    });
    environment.add_filter("yaml", |value: minijinja::Value| {
        serde_yaml::to_string(&value).map_err(|error| {
            minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                format!("yaml: {error}"),
            )
        })
    });

    // A password with a `#` in it ends a line in half the formats here.
    environment.add_filter("quote", |value: minijinja::Value| {
        serde_json::to_string(&value.to_string()).unwrap_or_else(|_| String::from("\"\""))
    });

    // `Strict` already refuses an *undefined* key. This refuses one that
    // is defined and empty, which is the shape a missing secret usually
    // arrives in — and it names the field rather than rendering a document
    // with a blank password in it.
    environment.add_filter(
        "required",
        |value: minijinja::Value,
         message: Option<String>|
         -> Result<minijinja::Value, minijinja::Error> {
            let missing = value.is_none()
                || value.is_undefined()
                || value.as_str().is_some_and(str::is_empty);

            if missing {
                return Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    message.unwrap_or_else(|| "required: the value is empty".to_owned()),
                ));
            }

            Ok(value)
        },
    );
}

/// Standard-library base64, the same three-bytes-to-four this organisation
/// writes wherever it needs one rather than taking a dependency for it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let block = u32::from(chunk[0]) << 16
            | u32::from(chunk.get(1).copied().unwrap_or(0)) << 8
            | u32::from(chunk.get(2).copied().unwrap_or(0));

        for index in 0..4 {
            if index <= chunk.len() {
                let position = (block >> (18 - index * 6)) & 0x3F;
                encoded.push(char::from(ALPHABET[position as usize]));
            } else {
                encoded.push('=');
            }
        }
    }

    encoded
}

/// The other direction. `None` for anything that is not base64.
fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut held = 0;
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);

    for character in text.bytes() {
        let value = match character {
            b'A'..=b'Z' => u32::from(character - b'A'),
            b'a'..=b'z' => u32::from(character - b'a') + 26,
            b'0'..=b'9' => u32::from(character - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        };

        bits = bits << 6 | value;
        held += 6;

        if held >= 8 {
            held -= 8;
            bytes.push(u8::try_from(bits >> held & 0xFF).ok()?);
        }
    }

    Some(bytes)
}

/// Every file this document becomes, rendered and checked but not yet
/// written.
///
/// **All or none.** The whole point is that a failure in the third file
/// does not leave the first two published: an application reading two of
/// them never sees one from before a change and one from after it. So
/// everything is resolved, validated and rendered *first*, and only a
/// complete set is handed back to be written.
///
/// What this does not claim: the writes themselves are separate renames.
/// Each is atomic, the set is not, and a reader can in principle catch the
/// microseconds between two of them. That window is a rename apart rather
/// than a fetch apart, which is the difference worth having — and calling
/// it atomicity would be a promise the filesystem does not make.
///
/// # Errors
///
/// If any rendering fails. Nothing is written either way; writing is the
/// caller's, and it happens only on `Ok`.
pub fn render_all(
    fetched: &dynamic_config::Fetched,
    spec: &Spec,
) -> Result<Vec<Published>, Box<dyn std::error::Error>> {
    // Before anything is parsed. The engine enforces the same ceiling at
    // its own slot, which this agent does not use — it reads a
    // `RemoteSource` directly — so the check has to be here, and here is
    // also the right place: a document is held several times over between
    // parsing and rendering, and the container has 64Mi.
    let ceiling = spec
        .max_document_bytes
        .unwrap_or(dynamic_config::MAX_DOCUMENT_BYTES);

    if fetched.text.len() > ceiling {
        return Err(format!(
            "the store answered with {} bytes, past the {ceiling}-byte ceiling; \
             raise it with --max-document-bytes if the document is meant to be \
             this large",
            fetched.text.len()
        )
        .into());
    }

    let mut published = Vec::with_capacity(1 + spec.also.len());

    published.push(Published {
        out: spec.out.clone(),
        document: render_fetched(fetched, spec)?,
        file_mode: spec.file_mode,
    });

    for rendering in &spec.also {
        let document = resolve(&fetched.text, fetched.format, rendering.section.as_deref())
            .map_err(|error| format!("{}: {error}", rendering.out.display()))?;

        if let Some(path) = &spec.schema {
            validate(&document, path)
                .map_err(|error| format!("{}: {error}", rendering.out.display()))?;
        }

        let rendered = render(
            &document,
            OutputFormat::of(&rendering.out).expect("validated in Spec"),
        )
        .map_err(|error| format!("{}: {error}", rendering.out.display()))?;

        published.push(Published {
            out: rendering.out.clone(),
            document: rendered,
            file_mode: rendering.file_mode,
        });
    }

    Ok(published)
}

/// One file, rendered and waiting to be written.
pub struct Published {
    /// Where it goes.
    pub out: std::path::PathBuf,
    /// What goes in it.
    pub document: String,
    /// The permissions it lands with.
    pub file_mode: Option<u32>,
}

/// The compiled schema, if one was configured.
///
/// Compiled once per process rather than per render: a schema is a mounted
/// ConfigMap and compiling it is the expensive half, while a render happens
/// every time the store moves.
///
/// A schema that will not compile is held as its message and returned on
/// every render — the alternative is retrying the compilation on a tick,
/// which turns one clear failure into a stream of identical ones.
static SCHEMA: std::sync::OnceLock<Result<jsonschema::Validator, String>> =
    std::sync::OnceLock::new();

/// Checks the resolved document against the schema at `path`.
///
/// # Errors
///
/// If the schema is unreadable or will not compile, or if the document does
/// not satisfy it.
fn validate(
    document: &serde_json::Value,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let compiled = SCHEMA.get_or_init(|| {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("the schema at {} is unreadable: {error}", path.display()))?;

        let schema: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("the schema at {} is not JSON: {error}", path.display()))?;

        jsonschema::validator_for(&schema)
            .map_err(|error| format!("the schema at {} will not compile: {error}", path.display()))
    });

    let compiled = compiled.as_ref().map_err(Clone::clone)?;

    let mut refusals = compiled.iter_errors(document).peekable();

    if refusals.peek().is_none() {
        return Ok(());
    }

    // **The path and the rule, never the value.** A schema failure names the
    // field that was wrong, and the field that is wrong is on a bad day the
    // one holding the password — the same rule the engine's parse errors
    // follow. `jsonschema`'s own `Display` prints the instance, so it is
    // deliberately not used.
    let named: Vec<String> = refusals
        .take(FIRST_FEW)
        .map(|refusal| {
            let path = refusal.instance_path().to_string();
            let at = if path.is_empty() {
                "the document".to_owned()
            } else {
                path
            };

            format!("{at} does not satisfy {}", refusal.schema_path())
        })
        .collect();

    Err(format!(
        "the document does not satisfy the schema: {}",
        named.join("; ")
    )
    .into())
}

/// How many schema refusals to name before stopping.
///
/// A document that is wrong everywhere produces a message nobody reads, and
/// the first few are the ones somebody fixes first.
const FIRST_FEW: usize = 5;

/// The digest of a rendered document, for the meta file beside it.
///
/// Over the **bytes as written**, which is what makes it checkable: an
/// operator can run `sha256sum` on the file and get this number back.
/// Deliberately not the same thing as the engine's
/// `Config::fingerprint()`, which digests the *resolved tree* with secrets
/// masked — that answers "is this process's configuration the same as that
/// one's", and this answers "is this file the same as that file".
///
/// It therefore *does* cover secret values, which is why it goes in a file
/// beside the document rather than into a log line or a metric label.
#[must_use]
pub fn digest(document: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(document.as_bytes());

    let mut rendered = String::from("sha256:");

    for byte in hasher.finalize() {
        use std::fmt::Write;

        let _ = write!(rendered, "{byte:02x}");
    }

    rendered
}

/// What was rendered, beside what was rendered.
///
/// A sibling `.<name>.meta` answering the question an application cannot
/// otherwise ask: *which configuration am I actually running?* Two pods
/// holding the same file is a claim nobody can check from inside either of
/// them; two pods printing the same fingerprint is one anybody can.
///
/// **Never a value.** The fingerprint has secrets masked by position, the
/// revision is what the store called this version, and the rest is a clock
/// — nothing here is the document, which is the whole reason this can sit
/// beside a file mounted into an application that is not trusted with it.
///
/// Written through the same rename as the document, and *after* it: a meta
/// file describing a document that is not there yet would be worse than no
/// meta file at all.
pub fn write_meta(
    path: &Path,
    generation: Option<&dynamic_config::Revision>,
    fingerprint: &str,
    mode: Option<u32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(meta) = meta_path(path) else {
        return Ok(());
    };

    let revision =
        generation.map_or_else(|| "null".to_owned(), |revision| format!("\"{revision}\""));

    let rendered_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());

    let document = format!(
        "{{\n  \"fingerprint\": \"{fingerprint}\",\n  \"revision\": {revision},\n  \
         \"rendered_at\": {rendered_at}\n}}\n"
    );

    write_atomically(&meta, &document, mode)
}

/// Keeps the generation that is being replaced, for the question an
/// incident starts with.
///
/// *What was the file before?* is unanswerable today: the rename that
/// publishes a new document is the same rename that destroys the old one,
/// and by the time anybody asks, the store has moved on too.
///
/// **What this is not.** It is not a rollback. Putting an old document back
/// needs the application to say the new one is bad, and nothing here can
/// hear that — half a feature that implies the other half is worse than
/// neither. This keeps files; a person reads them.
///
/// `/config/app.yaml` → `/config/.app.yaml.history/<unix>-<digest>.yaml`,
/// newest kept, oldest pruned past `keep`.
///
/// # Errors
///
/// Never: a history that cannot be written is a diagnostic that is missing,
/// not a render that failed. The caller logs what comes back and publishes
/// regardless.
pub fn keep_previous(path: &Path, keep: usize) -> Result<(), Box<dyn std::error::Error>> {
    if keep == 0 {
        return Ok(());
    }

    // Nothing to keep on the first render, which is the common case for a
    // pod that starts and never changes.
    let Ok(previous) = std::fs::read_to_string(path) else {
        return Ok(());
    };

    let directory = history_path(path).ok_or("--out needs a name")?;
    std::fs::create_dir_all(&directory)?;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());

    // The digest in the name, so two generations with the same contents —
    // a re-render that changed nothing but the store's revision — are one
    // file rather than two, and so a reader can match a name against a meta
    // file without opening it.
    let digest = digest(&previous);
    let short = digest.trim_start_matches("sha256:").get(..12).unwrap_or("");
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("txt");

    let kept = directory.join(format!("{stamp}-{short}.{extension}"));

    // The same mode as the render it is a copy of. A history file readable
    // by more than the document would be a way around the document's own
    // permissions.
    write_atomically(&kept, &previous, mode_of(path))?;
    prune(&directory, keep)?;

    Ok(())
}

/// The mode the rendered file actually has, so a copy of it does not widen
/// anything.
#[cfg(unix)]
fn mode_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn mode_of(_path: &Path) -> Option<u32> {
    None
}

/// Drops the oldest until `keep` remain.
fn prune(directory: &Path, keep: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut kept: Vec<std::path::PathBuf> = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();

    // By name, which sorts by the timestamp it starts with. Reading mtimes
    // would be the same answer through more syscalls, and a tmpfs restored
    // from anywhere has the names and not necessarily the times.
    kept.sort();

    let over = kept.len().saturating_sub(keep);

    for path in kept.into_iter().take(over) {
        let _ = std::fs::remove_file(path);
    }

    Ok(())
}

/// `/config/app.yaml` → `/config/.app.yaml.history`.
#[must_use]
pub fn history_path(path: &Path) -> Option<std::path::PathBuf> {
    let name = path.file_name()?.to_str()?;

    Some(path.with_file_name(format!(".{name}.history")))
}

/// `/config/app.yaml` → `/config/.app.yaml.meta`.
///
/// Hidden, so a directory read by an application that globs `*.yaml` does
/// not suddenly find a second file it will try to parse.
#[must_use]
pub fn meta_path(path: &Path) -> Option<std::path::PathBuf> {
    let name = path.file_name()?.to_str()?;

    Some(path.with_file_name(format!(".{name}.meta")))
}

/// Write-then-rename, the same courtesy every atomic-save editor pays:
/// the application's watcher sees whole files, never half ones.
pub fn write_atomically(
    path: &Path,
    content: &str,
    mode: Option<u32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = path.parent().ok_or("--out needs a parent directory")?;
    let scratch = directory.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("rendered")
    ));

    std::fs::write(&scratch, content)?;

    // Permissions land on the SCRATCH file, before the rename: a reader
    // must never observe the final path in a mode it will not keep.
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(mode))?;
    }

    #[cfg(not(unix))]
    let _ = mode;

    std::fs::rename(&scratch, path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> serde_json::Value {
        serde_json::json!({
            "host": "db.internal",
            "port": 5432,
            "pool": { "max": 8, "warm": true },
        })
    }

    #[test]
    fn properties_flattens_with_dots() {
        let rendered = render(&document(), OutputFormat::Properties).expect("renders");

        assert!(rendered.contains("host = db.internal"));
        assert!(rendered.contains("pool.max = 8"));
        assert!(rendered.contains("pool.warm = true"));
    }

    #[test]
    fn ini_sections_top_level_tables() {
        let rendered = render(&document(), OutputFormat::Ini).expect("renders");

        assert!(rendered.contains("host = db.internal"));
        assert!(rendered.contains("[pool]"));
        assert!(rendered.contains("max = 8"));
    }

    #[test]
    fn a_widening_string_is_quoted_in_ini() {
        let tricky = serde_json::json!({ "version": "1.10" });
        let rendered = render(&tricky, OutputFormat::Ini).expect("renders");

        assert!(rendered.contains("version = \"1.10\""), "{rendered}");
    }

    #[test]
    fn an_array_is_refused_by_path() {
        let listy = serde_json::json!({ "hosts": ["a", "b"] });
        let error = render(&listy, OutputFormat::Properties).expect_err("refused");

        assert!(error.to_string().contains("hosts"), "{error}");
    }

    #[test]
    fn the_flat_round_trip_holds_through_the_engine() {
        // What the agent writes, the engine reads back to the same
        // document — the property the whole pipeline rests on, with the
        // documented widening as the one translation.
        let rendered = render(&document(), OutputFormat::Properties).expect("renders");

        let back = super::resolve(&rendered, Format::Properties, None).expect("loads");

        assert_eq!(back, document());
    }

    #[test]
    fn a_template_owns_the_bytes() {
        let document = serde_json::json!({
            "db": { "host": "db.internal", "port": 5432, "user": "billing" },
        });

        let rendered = templated(
            "DATABASE_URL=postgres://{{ db.user }}@{{ db.host }}:{{ db.port }}/billing\n",
            &document,
        )
        .expect("renders");

        assert_eq!(
            rendered,
            "DATABASE_URL=postgres://billing@db.internal:5432/billing\n"
        );
    }

    #[test]
    fn a_typoed_key_is_an_error_not_an_empty_string() {
        let document = serde_json::json!({ "db": { "host": "x" } });

        let error = templated("{{ db.hots }}", &document).expect_err("strict");

        assert!(error.to_string().contains("undefined"), "{error}");
    }

    #[test]
    fn a_template_can_loop_and_filter() {
        let document = serde_json::json!({
            "hosts": ["a.internal", "b.internal"],
            "flags": { "beta": true },
        });

        let rendered = templated(
            "{% for h in hosts %}server {{ h }};\n{% endfor %}beta={{ flags.beta }}",
            &document,
        )
        .expect("renders");

        assert_eq!(
            rendered,
            "server a.internal;\nserver b.internal;\nbeta=true"
        );
    }

    #[test]
    fn atomic_write_lands_whole() {
        let directory = tempfile::tempdir().expect("a directory");
        let out = directory.path().join("rendered.toml");

        write_atomically(&out, "a = 1\n", None).expect("writes");

        assert_eq!(std::fs::read_to_string(&out).expect("reads"), "a = 1\n");
        assert!(!directory.path().join(".rendered.toml.tmp").exists());
    }

    #[test]
    fn entry_shapes_map_the_same_leaves_two_ways() {
        let document = serde_json::json!({
            "auth": { "postgres-password": "s3cr3t" },
            "db": { "pool_size": 8, "replicas": [1, 2] },
        });

        let env = env_entries(&document).expect("maps");

        assert!(env.contains(&("AUTH_POSTGRES_PASSWORD".into(), "s3cr3t".into())));
        assert!(env.contains(&("DB_POOL_SIZE".into(), "8".into())));
        assert!(env.contains(&("DB_REPLICAS".into(), "[1,2]".into())));

        let verbatim = verbatim_entries(&document).expect("maps");

        // The whole point: the spelling someone else chose survives.
        assert!(verbatim.contains(&("auth.postgres-password".into(), "s3cr3t".into())));
        assert!(verbatim.contains(&("db.pool_size".into(), "8".into())));
    }

    #[cfg(unix)]
    #[test]
    fn the_asked_file_mode_survives_the_rename() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("a directory");
        let out = directory.path().join("rendered.toml");

        write_atomically(&out, "a = 1\n", Some(0o640)).expect("writes");

        let mode = std::fs::metadata(&out).expect("stats").permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o640,
            "the rendered file wears the asked mode"
        );
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn rendered(template: &str) -> Result<String, String> {
        let document = serde_json::json!({
            "user": "app",
            "password": "p@ss#word",
            "port": 5432,
            "tls": true,
            "empty": "",
            "pool": { "max": 10 },
        });

        templated(template, &document).map_err(|error| error.to_string())
    }

    /// A Secret's `data` is base64, so a template that writes one has to
    /// encode — and minijinja ships no filter for it.
    #[test]
    fn base64_goes_both_ways() {
        assert_eq!(rendered("{{ user | b64encode }}").unwrap(), "YXBw");
        assert_eq!(rendered("{{ 'YXBw' | b64decode }}").unwrap(), "app");
    }

    #[test]
    fn base64_pads_the_way_everything_else_expects() {
        // One, two and three bytes: the three padding cases.
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");

        for text in ["", "a", "ab", "abc", "hello world", "p@ss#word"] {
            assert_eq!(
                unbase64(&base64(text.as_bytes())).as_deref(),
                Some(text.as_bytes()),
                "round trip: {text:?}"
            );
        }
    }

    #[test]
    fn not_base64_is_an_error_rather_than_a_guess() {
        assert!(rendered("{{ '!!!' | b64decode }}").is_err());
    }

    /// minijinja's own `tojson` is behind a feature this build does not
    /// enable, so a configuration template could not emit JSON at all.
    #[test]
    fn json_and_yaml_are_available() {
        assert_eq!(rendered("{{ pool | json }}").unwrap(), r#"{"max":10}"#);
        assert!(rendered("{{ pool | yaml }}").unwrap().contains("max: 10"));
    }

    /// A password with a `#` in it ends a line in half the formats here.
    #[test]
    fn quote_escapes_what_would_otherwise_end_a_line() {
        assert_eq!(
            rendered("{{ password | quote }}").unwrap(),
            r#""p@ss#word""#
        );
    }

    /// `Strict` refuses an *undefined* key already. This refuses one that
    /// is defined and empty — the shape a missing secret usually arrives
    /// in — and names the field rather than rendering a blank password.
    #[test]
    fn required_refuses_an_empty_value() {
        assert!(rendered("{{ empty | required }}").is_err());
        assert_eq!(rendered("{{ user | required }}").unwrap(), "app");

        let error = rendered(r#"{{ empty | required("no password was supplied") }}"#)
            .expect_err("empty is refused");

        assert!(error.contains("no password was supplied"), "{error}");
    }

    /// The line this crate does not cross: a template is a pure function of
    /// the document already in hand. Consul Template's `secret()` is
    /// exactly the feature not being copied — so there is nothing here that
    /// reads a file, an environment variable or a store.
    #[test]
    fn a_template_cannot_reach_outside_the_document() {
        for reaching in [
            "{{ secret('secret/myapp') }}",
            "{{ env('HOME') }}",
            "{{ file('/etc/passwd') }}",
        ] {
            assert!(
                rendered(reaching).is_err(),
                "a template must not be able to call {reaching}"
            );
        }
    }

    #[test]
    fn the_builtin_filters_are_still_there() {
        assert_eq!(rendered("{{ port | string }}").unwrap(), "5432");
        assert_eq!(rendered("{{ missing | default('none') }}").unwrap(), "none");
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    /// The engine's own validation is a *typed* one and belongs to Rust,
    /// Python and Node. This is the same guarantee for the consumers that
    /// are none of those — a Java service reading `.properties`, a daemon
    /// reading YAML — so it is checked before the write and the last good
    /// file goes on serving when it fails.
    #[test]
    fn a_document_that_does_not_satisfy_the_schema_is_refused() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let schema = directory.path().join("schema.json");

        std::fs::write(
            &schema,
            r#"{
                "type": "object",
                "properties": {
                    "port": { "type": "integer", "minimum": 1 },
                    "password": { "type": "string" }
                },
                "required": ["port"]
            }"#,
        )
        .expect("the schema writes");

        let good = serde_json::json!({ "port": 5432, "password": "hunter2-planted" });
        assert!(validate(&good, &schema).is_ok());

        let bad = serde_json::json!({ "port": "not-a-number", "password": "hunter2-planted" });
        let error = validate(&bad, &schema)
            .expect_err("a string is not an integer")
            .to_string();

        // The path and the rule, never the value: a schema failure names
        // the field that was wrong, and on a bad day that is the one
        // holding the password.
        assert!(error.contains("port"), "{error}");
        assert!(
            !error.contains("hunter2-planted"),
            "the document must not reach the message: {error}"
        );
    }
}
