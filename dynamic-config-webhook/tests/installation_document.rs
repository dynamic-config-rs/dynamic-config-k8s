//! The installation, written as YAML.
//!
//! What this pins: a document renders to the grammar the environment
//! carries, so the two forms cannot mean different things — and the
//! document is checked as strictly, because a setting silently ignored is
//! a default that never applied.

use std::io::Write as _;

use dynamic_config_webhook::installation_file;

fn written(name: &str, contents: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join("dynamic-config-installation");
    std::fs::create_dir_all(&directory).expect("the scratch directory is creatable");

    let path = directory.join(name);
    let mut file = std::fs::File::create(&path).expect("writable");

    file.write_all(contents.as_bytes()).expect("written");

    path
}

#[test]
fn a_document_renders_to_the_variables_it_stands_for() {
    let path = written(
        "full.yaml",
        r#"
mode: sidecar
watchSeconds: 30
nativeSidecar: true
storeDefaults:
  vault:
    overridable: false
    endpoint: https://vault:8200
  s3:
    file-mode: "0640?"
sourceAllow:
  payments: [vault, s3]
  "*": [consul]
"#,
    );

    let rendered = installation_file::read(&path).expect("the document reads");

    assert_eq!(
        rendered
            .get("DYNAMIC_CONFIG_AGENT_MODE")
            .map(String::as_str),
        Some("sidecar")
    );
    // A number and a boolean are written as YAML writes them, and reach
    // the grammar as the strings it expects.
    assert_eq!(
        rendered
            .get("DYNAMIC_CONFIG_AGENT_WATCH_SECONDS")
            .map(String::as_str),
        Some("30")
    );
    assert_eq!(
        rendered
            .get("DYNAMIC_CONFIG_AGENT_NATIVE_SIDECAR")
            .map(String::as_str),
        Some("true")
    );

    // Maps are ordered, so the rendering is stable — a chart that
    // re-renders an unchanged values file produces an unchanged pod.
    assert_eq!(
        rendered
            .get("DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS")
            .map(String::as_str),
        Some("s3: file-mode=0640?; vault: endpoint=https://vault:8200, overridable=false")
    );
    assert_eq!(
        rendered
            .get("DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW")
            .map(String::as_str),
        Some("*: consul; payments: vault, s3")
    );
}

/// The rendering is not a translation into a *different* contract: what
/// comes out is parsed by the same code an environment variable is.
#[test]
fn a_document_and_the_variables_it_renders_to_are_one_installation() {
    let path = written(
        "equivalent.yaml",
        "storeDefaults:\n  vault:\n    watch-seconds: 30\n    overridable: false\n",
    );
    let rendered = installation_file::read(&path).expect("the document reads");

    let from_document =
        dynamic_config_webhook::Installation::from_lookup(&|name| rendered.get(name).cloned())
            .expect("the rendered installation is valid");

    let written_out = dynamic_config_webhook::Installation::from_lookup(&|name| {
        (name == "DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS")
            .then(|| "vault: overridable=false, watch-seconds=30".to_owned())
    })
    .expect("the written-out installation is valid");

    // Two spellings, one installation: compared through what an
    // admission actually sees.
    let pod = serde_json::json!({
        "metadata": { "annotations": {
            "dynamic-config.rs/inject": "true",
            "dynamic-config.rs/source": "vault",
            "dynamic-config.rs/endpoint": "https://vault:8200",
            "dynamic-config.rs/key": "app/config",
            "dynamic-config.rs/path": "/config/rendered.toml",
            "dynamic-config.rs/mode": "sidecar",
        }},
        "spec": { "containers": [{ "name": "app", "image": "app:1" }] }
    });
    let review = serde_json::json!({
        "request": { "uid": "u", "namespace": "team", "object": pod },
    });

    assert_eq!(
        dynamic_config_webhook::admission_response_with(&review, &from_document),
        dynamic_config_webhook::admission_response_with(&review, &written_out),
    );
}

/// A setting nobody knows is a typo, and a typo silently ignored is a
/// default that never applied.
#[test]
fn an_unknown_setting_is_refused_and_says_what_there_is() {
    let path = written("typo.yaml", "watchSecnods: 30\n");
    let error = installation_file::read(&path).expect_err("that is not a setting");

    assert!(error.contains("watchSecnods"), "{error}");
    assert!(error.contains("watchSeconds"), "{error}");
}

/// A shape that cannot render is refused where it is written.
#[test]
fn a_setting_of_the_wrong_shape_is_refused() {
    let path = written("shape.yaml", "storeDefaults:\n  vault: not-a-map\n");
    let error = installation_file::read(&path).expect_err("a store's settings are a map");

    assert!(error.contains("storeDefaults.vault"), "{error}");
}

/// No mount is an installation that sets nothing, not a failure.
#[test]
fn an_absent_document_is_an_empty_installation() {
    let path = std::env::temp_dir().join("dynamic-config-installation/nothing-here.yaml");
    let _ = std::fs::remove_file(&path);

    assert!(installation_file::read(&path)
        .expect("absent is fine")
        .is_empty());
}

/// The document the chart renders is the document this reads.
///
/// The fixture beside this test is `helm template` output from a
/// structured values file — so a change to either side that the other
/// does not follow is a test failure rather than a deployment that
/// silently ignores half its settings.
#[test]
fn the_chart_renders_a_document_this_reads() {
    let path = written(
        "from-chart.yaml",
        include_str!("fixtures/chart-installation.yaml"),
    );
    let rendered = installation_file::read(&path).expect("the chart's document reads");

    assert_eq!(
        rendered
            .get("DYNAMIC_CONFIG_AGENT_STORE_DEFAULTS")
            .map(String::as_str),
        Some(
            "s3: agent-memory-limit=128Mi; vault: auth=kubernetes, \
             endpoint=https://vault.vault.svc:8200, overridable=false, watch-seconds=30"
        )
    );
    assert_eq!(
        rendered
            .get("DYNAMIC_CONFIG_WEBHOOK_SOURCE_ALLOW")
            .map(String::as_str),
        Some("*: consul; payments: vault, s3")
    );

    // And it is a *valid* installation, not merely a parseable one: the
    // grammar's own checks run over what this rendered.
    dynamic_config_webhook::Installation::from_lookup(&|name| rendered.get(name).cloned())
        .expect("the chart's installation is valid");
}
