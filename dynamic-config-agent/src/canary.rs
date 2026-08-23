//! Rolling a configuration to some of the fleet before all of it.
//!
//! A change published to every pod at once is a change that either works or
//! is an incident. What a canary buys is the third outcome: a few pods take
//! it, somebody looks at them, and the rest follow or do not.
//!
//! # Who decides to advance it
//!
//! This is the part that kept the feature deferred, and the answer turned
//! out to be one this repository already had a mechanism for.
//!
//! The cohort is a **percentage in a file** — a mounted ConfigMap. A
//! platform team edits the ConfigMap and the cohort widens; the kubelet
//! rewrites the mount in place, the agent notices on its next pass, and
//! held pods publish. **No pod restarts**, which is what makes it a canary
//! rather than a rollout with extra steps: an annotation would have needed
//! a new pod spec, and a new pod spec is a rolling restart that discards
//! the very state a canary is watching.
//!
//! It composes with acknowledgement: `applied` and `unapplied_seconds` say
//! whether the pods that took the change are actually running it, which is
//! the question somebody has to answer before widening.
//!
//! # Which pods are in it
//!
//! A pod's own name, hashed into a bucket from 0 to 99. Deterministic, so
//! the same pods stay in the cohort as it widens — a cohort that
//! reshuffled on every step would put every pod through the new document
//! eventually and prove nothing about any of them.
//!
//! No coordination, no leader, no list: five thousand agents each answer
//! for themselves and the distribution comes out of the hash.

use sha2::{Digest, Sha256};

/// The cohort this pod is in, from 0 to 99.
///
/// From the pod's name rather than its IP or its UID: a name survives a
/// restart in a StatefulSet and is what somebody reads off `kubectl get
/// pods` when they go looking at the cohort.
#[must_use]
pub fn bucket(pod: &str) -> u8 {
    let digest = Sha256::digest(pod.as_bytes());

    // The first two bytes are plenty for a hundred buckets, and taking
    // them from a cryptographic hash means a naming scheme with a common
    // prefix — which every Deployment has — still spreads.
    let value = u16::from(digest[0]) << 8 | u16::from(digest[1]);

    u8::try_from(value % 100).unwrap_or(0)
}

/// The percentage in the file, or `None` when it says nothing usable.
///
/// A file that cannot be read, or holds something that is not a number, is
/// **not** treated as zero: zero would hold every pod on its current
/// document, which is a fleet-wide freeze caused by a typo. `None` means
/// "no canary", and the caller publishes as it always did.
#[must_use]
pub fn percent(path: &std::path::Path) -> Option<u8> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: u16 = text.trim().parse().ok()?;

    // Above a hundred is a hundred rather than an error: somebody widening
    // a cohort past the end means everybody, and refusing that reading
    // would hold the fleet on a rounding mistake.
    Some(u8::try_from(value.min(100)).unwrap_or(100))
}

/// Whether this pod may take a new document yet.
///
/// `None` for the percentage means no canary is configured, and everything
/// publishes — which is what every pod without the annotation does.
#[must_use]
pub fn admitted(bucket: u8, percent: Option<u8>) -> bool {
    match percent {
        None => true,
        // Strictly less than, so `0` holds everybody and `100` holds
        // nobody. Both ends are reachable and mean what they read as.
        Some(percent) => bucket < percent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_is_stable_for_a_name() {
        assert_eq!(bucket("billing-7d9f8-abcde"), bucket("billing-7d9f8-abcde"));
        assert_ne!(bucket("billing-7d9f8-abcde"), bucket("billing-7d9f8-fghij"));
    }

    /// Every Deployment's pods share a prefix, so a hash that keyed on the
    /// front of the name would put a whole Deployment in one bucket.
    #[test]
    fn a_common_prefix_still_spreads() {
        let buckets: std::collections::BTreeSet<u8> = (0..200)
            .map(|n| bucket(&format!("billing-7d9f8-{n:05}")))
            .collect();

        assert!(
            buckets.len() > 50,
            "two hundred pods of one Deployment landed in {} buckets",
            buckets.len()
        );
    }

    /// The two ends are reachable and read as what they mean.
    #[test]
    fn zero_holds_everybody_and_a_hundred_holds_nobody() {
        for bucket in [0, 42, 99] {
            assert!(!admitted(bucket, Some(0)), "0% admits nobody");
            assert!(admitted(bucket, Some(100)), "100% admits everybody");
        }
    }

    #[test]
    fn a_pod_below_the_line_is_in_the_cohort() {
        assert!(admitted(4, Some(5)));
        assert!(!admitted(5, Some(5)));
    }

    /// No canary configured is not a canary of zero.
    #[test]
    fn no_percentage_admits_everything() {
        assert!(admitted(0, None));
        assert!(admitted(99, None));
    }

    /// A typo must not freeze the fleet on its current document.
    #[test]
    fn an_unreadable_percentage_is_no_canary_rather_than_zero() {
        let scratch = std::env::temp_dir().join("dynamic-config-canary-test");
        std::fs::create_dir_all(&scratch).expect("a scratch directory");

        let missing = scratch.join("absent");
        let _ = std::fs::remove_file(&missing);

        assert_eq!(percent(&missing), None);

        let garbage = scratch.join("garbage");
        std::fs::write(&garbage, "twenty-five percent\n").expect("writable");

        assert_eq!(percent(&garbage), None);
    }

    #[test]
    fn a_percentage_is_read_and_clamped() {
        let scratch = std::env::temp_dir().join("dynamic-config-canary-test");
        std::fs::create_dir_all(&scratch).expect("a scratch directory");

        let file = scratch.join("percent");

        std::fs::write(&file, " 25 \n").expect("writable");
        assert_eq!(percent(&file), Some(25));

        std::fs::write(&file, "400").expect("writable");
        assert_eq!(percent(&file), Some(100), "past the end is everybody");
    }
}
