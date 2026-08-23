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

use dynamic_config_agent::spec::Spec;

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

/// One pod volume reading a document.
///
/// The spec travels with the path because the render is **not** shared —
/// two pods on one Consul key may want different formats, different file
/// modes and different templates out of the same bytes. The fetch is what
/// is shared; everything downstream of it is each pod's own.
#[derive(Clone)]
pub struct Reader {
    /// The volume directory the kubelet gave us.
    pub path: String,
    /// How this pod wants the document rendered.
    pub spec: Arc<Spec>,
}

/// A document being watched, and who wants it.
struct Watched {
    /// The pod volumes this is currently published into.
    ///
    /// A `Vec` rather than a count: unpublishing names a path, and a count
    /// cannot tell which of two pods went away.
    targets: Vec<Reader>,
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
    pub fn claim(&self, document: Document, target: &str, spec: Arc<Spec>) -> Claim {
        let mut watched = self
            .watched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let reader = Reader {
            path: target.to_owned(),
            spec,
        };

        if let Some(held) = watched.get_mut(&document) {
            if !held.targets.iter().any(|held| held.path == target) {
                held.targets.push(reader);
            }

            return Claim::Joined;
        }

        let (stop, ends) = tokio::sync::watch::channel(false);

        watched.insert(
            document,
            Watched {
                targets: vec![reader],
                stop,
            },
        );

        Claim::Started(ends)
    }

    /// Everyone currently reading `document`.
    ///
    /// The watch calls this on every document it accepts: one fetch, one
    /// render each. Without it the pod that happened to start the watch
    /// was the only one whose file ever moved again — the others got their
    /// first render from `NodePublishVolume` and then nothing, which is
    /// the opposite of what a watch is for.
    #[must_use]
    pub fn readers(&self, document: &Document) -> Vec<Reader> {
        self.watched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(document)
            .map(|held| held.targets.clone())
            .unwrap_or_default()
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
            held.targets.retain(|held| held.path != target);

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

    /// Any spec at all: these tests are about who is reading what, and
    /// the spec only travels so the render can be each pod's own.
    fn spec(out: &str) -> Arc<Spec> {
        Arc::new(
            Spec::from_args(
                [
                    "--source",
                    "consul",
                    "--endpoint",
                    "http://consul:8500",
                    "--key",
                    "myapp/config.json",
                    "--out",
                    out,
                ]
                .into_iter()
                .map(str::to_owned),
            )
            .expect("a spec the agent accepts"),
        )
    }

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
            registry.claim(
                document("myapp/config.json", ""),
                "/pods/a/vol/rendered.toml",
                spec("/pods/a/vol/rendered.toml")
            ),
            Claim::Started(_)
        ));
        assert!(matches!(
            registry.claim(
                document("myapp/config.json", ""),
                "/pods/b/vol/rendered.toml",
                spec("/pods/b/vol/rendered.toml")
            ),
            Claim::Joined
        ));

        assert_eq!(registry.counts(), (1, 2));
    }

    /// The failure this component would be worth abandoning over: two pods
    /// reading one key under different tokens are two reads.
    #[test]
    fn a_different_credential_is_a_different_document() {
        let registry = Registry::new();

        registry.claim(
            document("shared/key", "token-a"),
            "/pods/a/vol/rendered.toml",
            spec("/pods/a/vol/rendered.toml"),
        );

        assert!(matches!(
            registry.claim(
                document("shared/key", "token-b"),
                "/pods/b/vol/rendered.toml",
                spec("/pods/b/vol/rendered.toml")
            ),
            Claim::Started(_)
        ));

        assert_eq!(registry.counts(), (2, 2));
    }

    /// The kubelet retries a failed publish, and a retry must not leave two
    /// entries behind.
    #[test]
    fn the_same_target_claiming_twice_is_idempotent() {
        let registry = Registry::new();

        registry.claim(
            document("myapp/config.json", ""),
            "/pods/a/vol/rendered.toml",
            spec("/pods/a/vol/rendered.toml"),
        );
        registry.claim(
            document("myapp/config.json", ""),
            "/pods/a/vol/rendered.toml",
            spec("/pods/a/vol/rendered.toml"),
        );

        assert_eq!(registry.counts(), (1, 1));
    }

    #[test]
    fn the_watch_ends_when_the_last_reader_goes() {
        let registry = Registry::new();

        let Claim::Started(mut ends) = registry.claim(
            document("k", ""),
            "/pods/a/vol/rendered.toml",
            spec("/pods/a/vol/rendered.toml"),
        ) else {
            panic!("the first claim starts it");
        };

        registry.claim(
            document("k", ""),
            "/pods/b/vol/rendered.toml",
            spec("/pods/b/vol/rendered.toml"),
        );

        assert!(
            !registry.release("/pods/a/vol/rendered.toml"),
            "one reader is left"
        );
        assert!(!*ends.borrow_and_update(), "so the watch runs on");

        assert!(
            registry.release("/pods/b/vol/rendered.toml"),
            "that was the last"
        );
        assert!(*ends.borrow_and_update(), "and the watch was told to end");
        assert_eq!(registry.counts(), (0, 0));
    }

    /// Unpublishing a path nobody claimed is not an error: the kubelet
    /// calls `NodeUnpublishVolume` again after a partial failure, and
    /// answering anything but success would wedge the pod's deletion.
    #[test]
    fn releasing_something_unknown_is_quiet() {
        let registry = Registry::new();

        assert!(!registry.release("/pods/never/vol/rendered.toml"));
    }

    /// The fetch is shared; the render is not. Every reader comes back
    /// with its own spec, because the pod that started the watch is not
    /// the only one whose file has to move when the store does — that was
    /// the bug the kind leg caught.
    #[test]
    fn every_reader_comes_back_with_its_own_spec() {
        let registry = Registry::new();
        let key = document("myapp/config.json", "");

        registry.claim(
            key.clone(),
            "/pods/a/vol/rendered.toml",
            spec("/pods/a/vol/rendered.toml"),
        );
        registry.claim(
            key.clone(),
            "/pods/b/vol/rendered.yaml",
            spec("/pods/b/vol/rendered.yaml"),
        );

        let readers = registry.readers(&key);

        assert_eq!(readers.len(), 2, "both pods are reading it");

        let outs: Vec<_> = readers
            .iter()
            .map(|reader| reader.spec.out.display().to_string())
            .collect();

        assert!(outs.contains(&"/pods/a/vol/rendered.toml".to_owned()));
        assert!(
            outs.contains(&"/pods/b/vol/rendered.yaml".to_owned()),
            "the joined reader kept its own format, not the starter's"
        );

        registry.release("/pods/a/vol/rendered.toml");

        assert_eq!(
            registry.readers(&key).len(),
            1,
            "a released target stops being written to"
        );

        // A document nobody holds has no readers rather than a panic.
        assert!(registry.readers(&document("nobody/wants", "")).is_empty());
    }
}
