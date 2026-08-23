//! Noticing that the store's trust material changed.
//!
//! A CA bundle and a client certificate arrive as a mounted ConfigMap and
//! Secret, and the kubelet updates a mounted object **in place**: the file
//! the container sees changes without the container being told. Every
//! client in this family reads that material once, when it builds — so
//! before this, a rotated CA was a pod restart, and a rotation nobody
//! restarted for was a store that stopped answering at a moment unrelated
//! to the rotation.
//!
//! # What this is not
//!
//! It is not a *token* reload. The projected service-account token and the
//! config server's bearer file are already re-read on every use, so the
//! rotation that actually happens hourly needed nothing. This is the other
//! material: `--ca`, `--tls-cert`, `--tls-key`, and the ssh key.
//!
//! # Why it fingerprints rather than watches
//!
//! An inotify watch on a kubelet-projected file is a watch on a symlink
//! that gets replaced, which is the case filesystem watchers get wrong most
//! often. Reading three small files on the interval the store is already
//! read on is cheaper than being wrong, and the material is a few kilobytes
//! that the kubelet has in page cache anyway.

use std::path::PathBuf;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// The digest of every file the store's client was built from.
///
/// `None` for a source with no TLS material, which is the common case and
/// costs nothing: there is nothing to compare, so there is nothing to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Material {
    files: Vec<PathBuf>,
    digest: String,
}

impl Material {
    /// The material named by a spec, as it is on disk right now.
    ///
    /// `None` when the spec names none.
    #[must_use]
    pub fn of(spec: &crate::spec::Spec) -> Option<Self> {
        let files: Vec<PathBuf> = [
            spec.ca.clone(),
            spec.tls_cert.clone(),
            spec.tls_key.clone(),
            spec.ssh_key.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();

        if files.is_empty() {
            return None;
        }

        let digest = digest_of(&files);

        Some(Self { files, digest })
    }

    /// Whether what is on disk now differs from what this recorded.
    #[must_use]
    pub fn moved(&self) -> bool {
        digest_of(&self.files) != self.digest
    }

    /// Whether it has moved *and* finished moving.
    ///
    /// A rotation writes more than one file, and rebuilding a TLS client
    /// from a half-written pair is a failure that reads as a bad
    /// certificate. So a change is confirmed by reading twice a beat apart
    /// and getting the same answer, which costs 250ms once per rotation and
    /// nothing at all the rest of the time — the second read only happens
    /// when the first says something moved.
    ///
    /// Awaits, and the caller is about to leave its loop when this answers
    /// `true`, so the wait holds nothing that matters.
    pub async fn settled_change(&self) -> bool {
        if !self.moved() {
            return false;
        }

        let seen = digest_of(&self.files);
        tokio::time::sleep(SETTLE).await;

        digest_of(&self.files) == seen
    }
}

/// How long to wait for a multi-file rotation to finish writing.
const SETTLE: Duration = Duration::from_millis(250);

/// One digest over every file, in the order the spec names them.
///
/// A file that cannot be read contributes its absence rather than an error:
/// a CA that was deleted *is* a change, and the rebuild that follows will
/// fail with a message naming the path, which is the right place for that
/// failure to appear.
fn digest_of(files: &[PathBuf]) -> String {
    let mut hasher = Sha256::new();

    for file in files {
        hasher.update(file.as_os_str().as_encoded_bytes());

        match std::fs::read(file) {
            Ok(bytes) => {
                hasher.update(b"present");
                hasher.update(bytes);
            }
            Err(_) => hasher.update(b"absent"),
        }
    }

    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join("dynamic-config-agent-trust");
        std::fs::create_dir_all(&directory).expect("a scratch directory");

        directory.join(name)
    }

    fn spec_with(ca: &std::path::Path) -> crate::spec::Spec {
        crate::spec::Spec::from_args(
            [
                "--source",
                "consul",
                "--endpoint",
                "http://127.0.0.1:1",
                "--key",
                "unused",
                "--out",
                "/tmp/unused.json",
                "--ca",
                &ca.display().to_string(),
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("a valid spec")
    }

    #[test]
    fn a_spec_with_no_material_has_nothing_to_watch() {
        let spec = crate::spec::Spec::from_args(
            [
                "--source",
                "consul",
                "--endpoint",
                "http://127.0.0.1:1",
                "--key",
                "unused",
                "--out",
                "/tmp/unused.json",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("a valid spec");

        assert!(Material::of(&spec).is_none());
    }

    #[test]
    fn a_rewritten_certificate_authority_reads_as_moved() {
        let ca = scratch("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nfirst\n").expect("writable");

        let material = Material::of(&spec_with(&ca)).expect("there is material");

        assert!(!material.moved(), "nothing has happened yet");

        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nsecond\n").expect("writable");

        assert!(material.moved(), "the bundle was replaced");
    }

    /// A deleted CA is a change, not an error to swallow: the rebuild that
    /// follows fails with the path in it, which is where that belongs.
    #[test]
    fn a_deleted_certificate_authority_reads_as_moved() {
        let ca = scratch("deleted-ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nonly\n").expect("writable");

        let material = Material::of(&spec_with(&ca)).expect("there is material");

        std::fs::remove_file(&ca).expect("removable");

        assert!(material.moved());
    }

    /// Two files whose contents were swapped between them must not hash the
    /// same — the path is part of the digest for exactly that reason.
    #[test]
    fn swapping_two_files_contents_is_a_change() {
        let first = scratch("swap-a.pem");
        let second = scratch("swap-b.pem");

        std::fs::write(&first, "alpha").expect("writable");
        std::fs::write(&second, "beta").expect("writable");

        let before = digest_of(&[first.clone(), second.clone()]);

        std::fs::write(&first, "beta").expect("writable");
        std::fs::write(&second, "alpha").expect("writable");

        assert_ne!(before, digest_of(&[first, second]));
    }
}
