//! The chart, rendered — the half of this repository that is not Rust.
//!
//! **Skipped, with a word, when `helm` is not installed.** A contributor
//! fixing Rust should not have to install a Kubernetes toolchain to run
//! `cargo test`; CI has helm and runs this for real.
//!
//! What it pins is one class of mistake rather than one instance of it: a
//! name written twice. A volume that mounts a ConfigMap the same render
//! never creates leaves every pod in `CreateContainerConfigError` — and
//! with the webhook's `failurePolicy: Ignore`, the API server then admits
//! every pod uninjected. A cluster whose gate has silently stopped gating
//! is the worst shape this repository can ship, and it costs one grep to
//! prevent.

use std::process::Command;

/// `helm template` for a values set, or `None` when helm is absent.
fn rendered(overrides: &[&str]) -> Option<String> {
    let chart = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits in the workspace")
        .join("deploy/helm");

    let mut helm = Command::new("helm");
    helm.arg("template").arg("dc").arg(&chart);

    for value in overrides {
        helm.arg("--set").arg(value);
    }

    let output = match helm.output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: helm is not installed");

            return None;
        }
        Err(error) => panic!("running helm: {error}"),
    };

    assert!(
        output.status.success(),
        "helm refused the values: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    Some(String::from_utf8(output.stdout).expect("helm renders UTF-8"))
}

/// Every name in the document, by the line that introduces it.
fn names(rendered: &str, prefix: &str) -> Vec<String> {
    rendered
        .lines()
        .filter_map(|line| line.trim().strip_prefix(prefix))
        .map(|name| name.trim().trim_matches('"').to_owned())
        .collect()
}

/// **A mounted ConfigMap is a ConfigMap this chart creates.**
///
/// The installation ConfigMap was named after the release and mounted
/// after the webhook — two expressions that agreed by coincidence until a
/// map in `perStore` made the volume appear, whereupon the mount named
/// something nobody had created.
#[test]
fn every_mounted_config_map_is_one_the_chart_creates() {
    // The map spelling, because that is what brings the installation
    // ConfigMap into the render at all.
    let Some(document) = rendered(&[
        "agent.defaults.perStore.vault.endpoint=https://vault:8200",
        "webhook.metrics.serviceMonitor.enabled=true",
        "operator.enabled=true",
    ]) else {
        return;
    };

    // `name:` under a `configMap:` key, which is how a volume names one.
    let mut mounted = Vec::new();
    let mut lines = document.lines().peekable();

    while let Some(line) = lines.next() {
        if line.trim() != "configMap:" {
            continue;
        }

        if let Some(next) = lines.peek() {
            if let Some(name) = next.trim().strip_prefix("name:") {
                mounted.push(name.trim().trim_matches('"').to_owned());
            }
        }
    }

    assert!(
        !mounted.is_empty(),
        "the values above should have mounted the installation document"
    );

    let created: Vec<String> = document
        .split("---")
        .filter(|section| section.contains("kind: ConfigMap"))
        .flat_map(|section| names(section, "name:"))
        .collect();

    for name in &mounted {
        assert!(
            created.contains(name),
            "a volume mounts {name:?}, which this chart does not create: {created:?}"
        );
    }
}

/// **A ServiceMonitor selects a Service that exists.**
///
/// The monitor selected on the component label and the Service carried
/// only the common ones, so `serviceMonitor.enabled=true` produced a
/// monitor that matched nothing and scraped nothing, with no error
/// anywhere to say so.
#[test]
fn the_service_monitor_selects_a_service_this_chart_labels() {
    let Some(document) = rendered(&["webhook.metrics.serviceMonitor.enabled=true"]) else {
        return;
    };

    let monitor = document
        .split("---")
        .find(|section| section.contains("kind: ServiceMonitor"))
        .expect("the monitor was asked for");
    let service = document
        .split("---")
        .find(|section| section.contains("kind: Service\n"))
        .expect("the webhook has a service");

    // The selector's labels, which the Service's metadata must carry.
    let wanted: Vec<&str> = monitor
        .lines()
        .skip_while(|line| !line.contains("matchLabels"))
        .skip(1)
        .take_while(|line| line.starts_with("      ") && line.contains(':'))
        .map(str::trim)
        .collect();

    assert!(!wanted.is_empty(), "the monitor selects on something");

    for label in wanted {
        assert!(
            service.contains(label),
            "the monitor selects {label:?} and the service does not carry it"
        );
    }
}
