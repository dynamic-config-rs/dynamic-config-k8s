//! The annotation registry against the page that documents it.
//!
//! The contract is what the webhook accepts; the book is where somebody
//! reads what that is. Two hand-maintained lists of the same names drift —
//! that is what the registry replaced inside the crate, and this is the
//! same argument crossing into the documentation.
//!
//! It is a check rather than a generator on purpose. The table in the book
//! carries a default and a sentence per key, which a generator would have
//! to invent; what a generator would actually catch is a key that was added
//! and never written down, and that is exactly what an assertion catches
//! for none of the cost.

use dynamic_config_webhook::registry::{AnnotationSpec, REGISTRY};

/// The annotations page, read from the repository rather than embedded, so
/// a change to it is picked up without recompiling.
fn book() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../book/src/annotations.md");

    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Every key the webhook accepts is written down where somebody looks.
#[test]
fn every_annotation_is_documented() {
    let page = book();

    let undocumented: Vec<&str> = REGISTRY
        .iter()
        .map(|spec: &AnnotationSpec| spec.name)
        // The status annotation is written by the webhook and refused from
        // a pod, so it is documented as a section rather than as a row.
        .filter(|name| *name != "status")
        .filter(|name| !page.contains(&format!("dynamic-config.rs/{name}")))
        .collect();

    assert!(
        undocumented.is_empty(),
        "these annotations are accepted and undocumented: {undocumented:?}"
    );
}

/// And a deprecation, once there is one, says so on the page as well as in
/// the warning — the warning reaches whoever ran `kubectl apply`, and the
/// page reaches whoever is about to write the annotation.
#[test]
fn every_deprecation_is_documented_as_one() {
    let page = book();

    let silent: Vec<&str> = REGISTRY
        .iter()
        .filter(|spec| spec.deprecated_since.is_some())
        .map(|spec| spec.name)
        .filter(|name| {
            let row = page
                .lines()
                .find(|line| line.contains(&format!("dynamic-config.rs/{name}")));

            !row.is_some_and(|row| row.to_lowercase().contains("deprecated"))
        })
        .collect();

    assert!(
        silent.is_empty(),
        "these are deprecated in the registry and not on the page: {silent:?}"
    );
}
