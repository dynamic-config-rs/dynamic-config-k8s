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
