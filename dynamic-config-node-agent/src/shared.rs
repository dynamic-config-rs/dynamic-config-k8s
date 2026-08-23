//! One fetch per document, however many pods want it.
//!
//! This is the saving the whole component exists for. A node running a
//! hundred pods that read one Consul key opens **one** connection to
//! Consul, not a hundred; the sidecar shape cannot do that, because a
//! sidecar is by construction one process per render.
//!
//! # What counts as the same document
//!
//! The store, the address and the key — and the credential, because two
//! pods reading the same key under different tokens are two reads and must
//! stay two reads. A shared fetch keyed on the path alone would hand one
//! pod's document to another pod's namespace, which is the failure this
//! whole component would be worth abandoning over.
//!
//! # What is not shared
//!
//! The rendered file. Each pod gets its own bytes at its own path, in its
//! own format, with its own mode — a `.properties` reader and a YAML reader
//! on one node share the fetch and share nothing else.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// What makes two requests the same read.
///
/// Derived from the volume's attributes rather than from the pod: two pods
/// asking for the same key from the same store under the same credential
/// **are** asking the same question, and the answer is the same bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Document {
    /// `vault`, `consul`, `etcd`, …
    pub source: String,
    /// The store's address.
    pub endpoint: String,
    /// The key, path or object being read.
    pub key: String,
    /// Which credential this is read under.
    ///
    /// **Part of the identity, not metadata.** Two pods reading one key
    /// under different tokens are two reads: sharing them would hand one
    /// namespace's document to another, under a credential it was never
    /// granted.
    pub credential: String,
}

/// A document being watched, and who wants it.
struct Watched {
    /// The pod volumes this is currently published into.
    ///
    /// A `Vec` rather than a count: unpublishing names a path, and a count
    /// cannot tell which of two pods went away.
    targets: Vec<String>,
    /// Ends the watch when the last target goes.
    stop: tokio::sync::watch::Sender<bool>,
}

/// Every document this node is watching.
#[derive(Clone, Default)]
pub struct Registry {
    watched: Arc<Mutex<BTreeMap<Document, Watched>>>,
}

/// What [`Registry::claim`] answered.
///
/// No `PartialEq`: one variant carries a channel, and comparing two
/// channels is not a question with an answer. Callers match on it.
#[derive(Debug)]
pub enum Claim {
    /// Nobody was reading this; the caller starts the watch.
    Started(tokio::sync::watch::Receiver<bool>),
    /// Somebody already is, and this target joined them.
    Joined,
}

impl Registry {
    /// An empty registry, which is a node with no pods on it yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `target` wants `document`.
    ///
    /// Answers whether the caller has to start the watch or has joined one.
    /// The same target claiming twice is idempotent: the kubelet retries
    /// `NodePublishVolume` after any failure, and a retry must not leave
    /// two watches or two entries behind.
    pub fn claim(&self, document: Document, target: &str) -> Claim {
        let mut watched = self
            .watched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(held) = watched.get_mut(&document) {
            if !held.targets.iter().any(|path| path == target) {
                held.targets.push(target.to_owned());
            }

            return Claim::Joined;
        }

        let (stop, ends) = tokio::sync::watch::channel(false);

        watched.insert(
            document,
            Watched {
                targets: vec![target.to_owned()],
                stop,
            },
        );

        Claim::Started(ends)
    }

    /// Records that `target` is gone, and ends the watch if it was the
    /// last.
    ///
    /// Answers whether the watch was ended, which is the line the caller
    /// logs — a node whose last reader of a store went away should say so.
    pub fn release(&self, target: &str) -> bool {
        let mut watched = self
            .watched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let mut ended = false;

        watched.retain(|_, held| {
            held.targets.retain(|path| path != target);

            if held.targets.is_empty() {
                // The watch's own loop sees this and returns; nothing here
                // waits for it, because unpublishing must not block the
                // kubelet on a store that is not answering.
                let _ = held.stop.send(true);
                ended = true;

                return false;
            }

            true
        });

        ended
    }

    /// How many documents this node is watching, and how many pod volumes
    /// are reading them.
    ///
    /// The two numbers that say whether sharing is doing anything: on a
    /// node where they are equal, this component is a sidecar with extra
    /// steps.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let watched = self
            .watched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        (
            watched.len(),
            watched.values().map(|held| held.targets.len()).sum(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(key: &str, credential: &str) -> Document {
        Document {
            source: "consul".to_owned(),
            endpoint: "http://consul:8500".to_owned(),
            key: key.to_owned(),
            credential: credential.to_owned(),
        }
    }

    /// The saving: a hundred pods, one watch.
    #[test]
    fn two_pods_wanting_the_same_document_share_one_watch() {
        let registry = Registry::new();

        assert!(matches!(
            registry.claim(document("myapp/config.json", ""), "/pods/a/vol"),
            Claim::Started(_)
        ));
        assert!(matches!(
            registry.claim(document("myapp/config.json", ""), "/pods/b/vol"),
            Claim::Joined
        ));

        assert_eq!(registry.counts(), (1, 2));
    }

    /// The failure this component would be worth abandoning over: two pods
    /// reading one key under different tokens are two reads.
    #[test]
    fn a_different_credential_is_a_different_document() {
        let registry = Registry::new();

        registry.claim(document("shared/key", "token-a"), "/pods/a/vol");

        assert!(matches!(
            registry.claim(document("shared/key", "token-b"), "/pods/b/vol"),
            Claim::Started(_)
        ));

        assert_eq!(registry.counts(), (2, 2));
    }

    /// The kubelet retries a failed publish, and a retry must not leave two
    /// entries behind.
    #[test]
    fn the_same_target_claiming_twice_is_idempotent() {
        let registry = Registry::new();

        registry.claim(document("myapp/config.json", ""), "/pods/a/vol");
        registry.claim(document("myapp/config.json", ""), "/pods/a/vol");

        assert_eq!(registry.counts(), (1, 1));
    }

    #[test]
    fn the_watch_ends_when_the_last_reader_goes() {
        let registry = Registry::new();

        let Claim::Started(mut ends) = registry.claim(document("k", ""), "/pods/a/vol") else {
            panic!("the first claim starts it");
        };

        registry.claim(document("k", ""), "/pods/b/vol");

        assert!(!registry.release("/pods/a/vol"), "one reader is left");
        assert!(!*ends.borrow_and_update(), "so the watch runs on");

        assert!(registry.release("/pods/b/vol"), "that was the last");
        assert!(*ends.borrow_and_update(), "and the watch was told to end");
        assert_eq!(registry.counts(), (0, 0));
    }

    /// Unpublishing a path nobody claimed is not an error: the kubelet
    /// calls `NodeUnpublishVolume` again after a partial failure, and
    /// answering anything but success would wedge the pod's deletion.
    #[test]
    fn releasing_something_unknown_is_quiet() {
        let registry = Registry::new();

        assert!(!registry.release("/pods/never/vol"));
    }
}
