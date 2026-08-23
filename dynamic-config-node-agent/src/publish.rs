//! Turning a volume's attributes into a render, and back again.
//!
//! The kubelet hands a `NodePublishVolume` a target directory and a map of
//! attributes taken from the pod's volume definition. Those attributes are
//! the same vocabulary the annotations use — `source`, `endpoint`, `key`,
//! `path` — for the reason the sidecar's arguments are: **one contract**,
//! learned once, whichever shape delivers it.
//!
//! ```yaml
//! volumes:
//!   - name: config
//!     csi:
//!       driver: config.dynamic-config.rs
//!       readOnly: true
//!       volumeAttributes:
//!         source: consul
//!         endpoint: http://consul.default.svc:8500
//!         key: myapp/config.json
//!         path: rendered.toml     # relative: the kubelet owns the directory
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dynamic_config_agent::spec::Spec;

/// The attributes a volume must carry, and the ones it may.
///
/// Everything the sidecar's annotations accept, minus what only makes
/// sense beside an application: there is no `mode`, because a CSI volume is
/// mounted before the pod's containers start and stays; no `env-inject`,
/// because there is no command here to wrap; and no `metrics-port`,
/// because the metrics are the node agent's and are one endpoint for the
/// whole node.
#[derive(Debug)]
pub struct Volume {
    /// Where the kubelet wants the file: the target path, plus the
    /// attribute's own relative `path`.
    pub out: PathBuf,
    /// The spec the agent's own machinery runs on.
    pub spec: Spec,
    /// What makes this the same read as another pod's.
    pub document: crate::shared::Document,
}

impl Volume {
    /// Reads a `NodePublishVolume` into something the agent can run.
    ///
    /// # Errors
    ///
    /// If a required attribute is missing or the pair is contradictory —
    /// the same refusals the webhook makes at admission, made again here
    /// because a CSI volume never passes through the webhook at all.
    pub fn parse(target: &str, attributes: &HashMap<String, String>) -> Result<Self, String> {
        let want = |name: &str| -> Result<&String, String> {
            attributes
                .get(name)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("the volume attribute {name:?} is required"))
        };

        let source = want("source")?;
        let key = want("key")?;

        // Required, like the annotation of the same name and for the same
        // reason: the extension names the format, and there is no default
        // format to fall back on.
        //
        // Relative, and checked: the kubelet owns the target directory, and
        // a pod that could write outside it could write anywhere this
        // process can reach — which, on a node agent, is every other pod's
        // volume on the node.
        let relative = want("path")?.as_str();

        if relative.starts_with('/') || relative.split('/').any(|part| part == "..") {
            return Err(format!(
                "the volume attribute \"path\" is {relative:?}: it is a name \
                 inside the volume, and the kubelet owns the directory"
            ));
        }

        let out = Path::new(target).join(relative);

        // Built as an argument vector rather than a struct literal, so the
        // node agent and the sidecar cannot drift: every flag the agent
        // learns is a flag this accepts, and every refusal the agent makes
        // is one this makes.
        let mut arguments = vec![
            "--source".to_owned(),
            source.clone(),
            "--key".to_owned(),
            key.clone(),
            "--out".to_owned(),
            out.display().to_string(),
        ];

        for (attribute, flag) in [
            ("endpoint", "--endpoint"),
            ("section", "--section"),
            ("auth", "--auth"),
            ("auth-mount", "--auth-mount"),
            ("auth-role", "--auth-role"),
            ("auth-username", "--auth-username"),
            ("namespace", "--namespace"),
            ("ref", "--ref"),
            ("api-url", "--api-url"),
            ("file-mode", "--file-mode"),
            ("watch-seconds", "--watch"),
        ] {
            if let Some(value) = attributes.get(attribute).filter(|v| !v.is_empty()) {
                arguments.push(flag.to_owned());
                arguments.push(value.clone());
            }
        }

        // A watch, always. A CSI volume outlives the call that created it,
        // so "fetch once and exit" has no meaning here — the pod is still
        // running and still reading the file.
        if !arguments.iter().any(|argument| argument == "--watch") {
            arguments.push("--watch".to_owned());
            arguments.push("15".to_owned());
        }

        let spec = Spec::from_args(arguments.into_iter())?;

        Ok(Self {
            document: crate::shared::Document {
                source: source.clone(),
                endpoint: spec.endpoint.clone(),
                key: key.clone(),
                // The token as the kubelet passed it, which for a pod
                // using a Secret is the Secret's name — enough to keep two
                // namespaces' reads apart without holding the value.
                credential: attributes.get("token-secret").cloned().unwrap_or_default(),
            },
            out,
            spec,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attributes(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn consul() -> HashMap<String, String> {
        attributes(&[
            ("source", "consul"),
            ("endpoint", "http://consul:8500"),
            ("key", "myapp/config.json"),
            ("path", "rendered.toml"),
        ])
    }

    #[test]
    fn a_volume_becomes_a_spec_the_agent_can_run() {
        let volume = Volume::parse("/var/lib/kubelet/pods/a/volumes/x", &consul())
            .expect("a well-formed volume");

        assert_eq!(
            volume.out,
            Path::new("/var/lib/kubelet/pods/a/volumes/x/rendered.toml")
        );
        assert_eq!(volume.document.key, "myapp/config.json");
        assert!(
            volume.spec.watch.is_some(),
            "a CSI volume outlives its call"
        );
    }

    #[test]
    fn a_missing_attribute_is_refused_by_name() {
        let mut attributes = consul();
        attributes.remove("key");

        let error = Volume::parse("/target", &attributes).expect_err("no key");

        assert!(error.contains("\"key\""), "{error}");
    }

    /// The kubelet owns the target directory. A pod that could write
    /// outside it could write into every other pod's volume on the node,
    /// because this process can reach all of them.
    #[test]
    fn a_path_that_escapes_the_volume_is_refused() {
        for escape in ["../../etc/passwd", "/etc/passwd", "nested/../../out"] {
            let mut attributes = consul();
            attributes.insert("path".to_owned(), escape.to_owned());

            let error = Volume::parse("/target", &attributes).expect_err(escape);

            assert!(
                error.contains("the kubelet owns the directory"),
                "{escape}: {error}"
            );
        }
    }

    /// The extension names the format, so there is nothing to default to.
    #[test]
    fn a_missing_path_is_refused_rather_than_guessed() {
        let mut attributes = consul();
        attributes.remove("path");

        let error = Volume::parse("/target", &attributes).expect_err("no path");

        assert!(error.contains("\"path\""), "{error}");
    }
}
