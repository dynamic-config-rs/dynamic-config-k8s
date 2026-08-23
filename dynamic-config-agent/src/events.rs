//! Kubernetes Events, when a pod asks for them.
//!
//! `kubectl describe pod` is where an operator looks first, and until now a
//! render failure was not there: it was in the sidecar's log, which is one
//! `kubectl logs -c dynamic-config-agent` away from the place the question
//! was asked.
//!
//! # Why this is off by default
//!
//! **It needs an API credential**, and the agent carrying none is a
//! property everything else here leans on: the webhook touches the API
//! server nowhere on the admission path, and the sidecar has held no token
//! of its own. Turning this on puts a service account beside every
//! application in the cluster to improve a diagnostic — which is a real
//! trade, worth making deliberately and not for everyone.
//!
//! So it is opt-in twice: the chart has to grant the RBAC, and the pod has
//! to ask. The grant is `create` on `events` in the pod's own namespace and
//! nothing else — an agent that could *read* events could read every
//! failure in the namespace, and it has no reason to.
//!
//! # Why it is hand-written
//!
//! `kube` and `k8s-openapi` would be the obvious answer and would roughly
//! double this image for one POST. An Event is a small JSON object sent to
//! one endpoint; the token and the CA are files the kubelet already mounted.
//! `ureq` is in this binary's tree for three of its stores, so the whole
//! thing is a request builder and a body.

use std::time::Duration;

/// Where the kubelet mounts the pod's own service-account token.
const TOKEN: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";
/// And the cluster's CA, which is what makes the API server verifiable.
const CA: &str = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt";
/// And the namespace, so an Event lands on the right object.
const NAMESPACE: &str = "/var/run/secrets/kubernetes.io/serviceaccount/namespace";

/// One attempt, short. An Event is a diagnostic: making a render wait on
/// one would be letting the commentary block the work.
const DEADLINE: Duration = Duration::from_secs(3);

/// What an Event needs to name its subject.
///
/// Read once at start rather than per Event: the pod does not move, and a
/// file read per failure is a file read exactly when something is already
/// going wrong.
#[derive(Debug, Clone)]
pub struct Reporter {
    api: String,
    namespace: String,
    pod: String,
    uid: String,
    agent: ureq::Agent,
}

impl Reporter {
    /// Reads the pod's identity from the downward API and the kubelet's
    /// mounts.
    ///
    /// # Errors
    ///
    /// If the service account is not mounted, if the CA cannot be read, or
    /// if the pod's own name was not passed down. Every message names the
    /// thing that is missing, because each has a different fix and a pod
    /// that asked for Events and got none should find out why at start
    /// rather than at the first failure.
    pub fn new() -> Result<Self, String> {
        let namespace = std::fs::read_to_string(NAMESPACE)
            .map_err(|error| format!("{NAMESPACE}: {error}; is a service account mounted?"))?
            .trim()
            .to_owned();

        // From the downward API rather than from the hostname: a hostname
        // is the pod's name today and is not promised to be.
        let pod = std::env::var("DYNAMIC_CONFIG_POD_NAME").map_err(|_| {
            "DYNAMIC_CONFIG_POD_NAME is not set; the webhook sets it from the \
             downward API when events are asked for"
                .to_owned()
        })?;

        // The uid makes the Event point at *this* pod rather than at the
        // next one to carry the name. Optional: without it the Event still
        // lands, and `kubectl describe` still shows it.
        let uid = std::env::var("DYNAMIC_CONFIG_POD_UID").unwrap_or_default();

        let ca = std::fs::read(CA).map_err(|error| format!("{CA}: {error}"))?;

        // The cluster's CA **replaces** the trust store rather than adding
        // to it: the API server's certificate is issued by that authority
        // and by no public one, and a public root has no business being
        // accepted for this connection.
        let roots = ureq::tls::parse_pem(&ca)
            .filter_map(Result::ok)
            .filter_map(|item| match item {
                ureq::tls::PemItem::Certificate(certificate) => Some(certificate),
                _ => None,
            })
            .collect::<Vec<_>>();

        if roots.is_empty() {
            return Err(format!("{CA}: holds no certificate"));
        }

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(DEADLINE))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::new_with_certs(&roots))
                    .build(),
            )
            .build()
            .new_agent();

        Ok(Self {
            // In-cluster, always: this is the address the kubelet
            // guarantees and the one the CA above is issued for.
            api: "https://kubernetes.default.svc".to_owned(),
            namespace,
            pod,
            uid,
            agent,
        })
    }

    /// Posts a `Warning` event about this pod.
    ///
    /// `reason` is the machine-readable half — `RenderFailed`,
    /// `DocumentAbsent` — and is API on the same terms as the operator's:
    /// a dashboard aggregates on it.
    ///
    /// Never fatal, and never awaited by anything that matters: the caller
    /// spawns it, and a failure here is a diagnostic that did not arrive
    /// rather than a render that did not happen.
    pub fn warn(&self, reason: &str, message: &str) -> Result<(), String> {
        // Re-read per Event on purpose: the projected token rotates, and a
        // token cached at start is one that stops working after an hour —
        // the same rule the Vault store follows for the same file.
        let token = std::fs::read_to_string(TOKEN)
            .map_err(|error| format!("{TOKEN}: {error}"))?
            .trim()
            .to_owned();

        let now = time_now();

        let event = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Event",
            "metadata": {
                // Generated rather than named: two failures a second apart
                // must not collide, and Kubernetes does the suffix.
                "generateName": format!("{}.", self.pod),
                "namespace": self.namespace,
            },
            "involvedObject": {
                "apiVersion": "v1",
                "kind": "Pod",
                "name": self.pod,
                "namespace": self.namespace,
                "uid": self.uid,
            },
            "reason": reason,
            // The message is the agent's own sentence, which every path
            // here already builds without values in it.
            "message": message,
            "type": "Warning",
            "source": { "component": "dynamic-config-agent" },
            "firstTimestamp": now,
            "lastTimestamp": now,
            "count": 1,
        });

        let url = format!("{}/api/v1/namespaces/{}/events", self.api, self.namespace);

        self.agent
            .post(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .send_json(event)
            .map_err(|error| format!("posting the event: {error}"))?;

        Ok(())
    }
}

/// RFC 3339, which is what the API server takes.
///
/// Hand-formatted rather than through a date crate: this binary has none,
/// and the only thing being formatted is now.
fn time_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());

    // Days since the epoch to a civil date, by the algorithm in Howard
    // Hinnant's `chrono` paper — the same one every date library uses, and
    // forty lines shorter than taking one as a dependency.
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let time = seconds % 86_400;

    let era_days = days + 719_468;
    let era = era_days.div_euclid(146_097);
    let day_of_era = era_days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = if month <= 2 { year + 1 } else { year };

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3_600,
        (time % 3_600) / 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The API server refuses a timestamp it cannot parse, and a refused
    /// Event is a diagnostic that silently never arrives.
    #[test]
    fn the_timestamp_is_rfc_3339() {
        let stamp = time_now();

        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.as_bytes()[4], b'-', "{stamp}");
        assert_eq!(stamp.as_bytes()[7], b'-', "{stamp}");
        assert_eq!(stamp.as_bytes()[10], b'T', "{stamp}");

        let year: i64 = stamp[..4].parse().expect("a year");

        assert!((2020..2200).contains(&year), "{stamp}");
    }

    /// Without a service account there is nothing to authenticate with, and
    /// the failure says which file is missing rather than surfacing as a
    /// 401 at the first render failure.
    #[test]
    fn a_pod_without_a_service_account_says_so() {
        if std::path::Path::new(NAMESPACE).exists() {
            return; // running inside a cluster; nothing to assert
        }

        let error = Reporter::new().expect_err("no service account here");

        assert!(error.contains("service account"), "{error}");
    }
}
